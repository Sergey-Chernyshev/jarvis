//! Push-to-talk диктовка: хранит активную сессию захвата, транскрибирует и
//! вставляет текст при отпускании хоткея.
//!
//! Жизненный цикл:
//!   on_press()   → запустить микрофон (идемпотентно; двойное нажатие — no-op)
//!   on_release() → остановить, транскрибировать PCM → вставить текст (async, spawn)
//!
//! Всё fail-safe: любой шаг пишет в лог и возвращается без паники.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Manager;

use super::hub::{AudioHub, CaptureSession};
use super::insert::InsertionTarget;
use super::SttService;

/// PTT-потребитель диктовки. Живёт в Arc внутри Daemon.
/// С инкр.10 захват идёт через общий `AudioHub` (единая зона ответственности),
/// а не через собственный cpal-стрим.
pub struct Dictation {
    service: Arc<SttService>,
    hub: Arc<AudioHub>,
    /// Активная сессия захвата аудио + момент старта (None = не пишем). Метка
    /// времени нужна watchdog'у `abort_if_stuck`: если key-up PTT потерялся,
    /// сессия висела бы вечно и держала микрофон.
    capturing: Mutex<Option<(CaptureSession, std::time::Instant, InsertionTarget)>>,
    /// One dictation finishes before the next can overwrite its clipboard/HUD.
    processing: Arc<AtomicBool>,
    insertion: Arc<super::insert::InsertionControl>,
    /// AppHandle для HUD-фаз («Слушаю…/Анализирую…/Услышал») и истории реплик.
    /// None в юнит-тестах — тогда HUD/история становятся no-op.
    app: Option<tauri::AppHandle>,
}

impl Dictation {
    pub fn new(service: Arc<SttService>, hub: Arc<AudioHub>, app: tauri::AppHandle) -> Arc<Self> {
        Arc::new(Dictation {
            service,
            hub,
            capturing: Mutex::new(None),
            processing: Arc::new(AtomicBool::new(false)),
            insertion: Arc::new(super::insert::InsertionControl::default()),
            app: Some(app),
        })
    }

    /// Конструктор для тестов: без AppHandle (HUD/история — no-op).
    #[cfg(test)]
    fn new_headless(service: Arc<SttService>, hub: Arc<AudioHub>) -> Arc<Self> {
        Arc::new(Dictation {
            service,
            hub,
            capturing: Mutex::new(None),
            processing: Arc::new(AtomicBool::new(false)),
            insertion: Arc::new(super::insert::InsertionControl::default()),
            app: None,
        })
    }

    /// Daemon для HUD/истории (None, если нет AppHandle — например в тестах).
    fn daemon(&self) -> Option<std::sync::Arc<crate::daemon::Daemon>> {
        self.app.as_ref().map(crate::daemon::Daemon::get)
    }

    pub fn cancel_insertion(&self, attempt_id: u64) -> bool {
        self.insertion.cancel(attempt_id)
    }

    /// Начать захват аудио при нажатии хоткея. Идемпотентно: если захват уже
    /// идёт (двойное срабатывание или авто-повтор клавиши) — пропуск.
    pub fn on_press(&self) {
        // Auto-repeat must not make extra AX requests while the session runs.
        if self.is_capturing() || self.processing.load(Ordering::Acquire) {
            return;
        }
        if self.hub.is_muted() {
            if let Some(d) = self.daemon() {
                crate::route::hud::emit(
                    &d,
                    crate::route::hud::Phase::Error {
                        msg: "Микрофон выключен. Включите его, затем начните диктовку.".into(),
                    },
                );
            }
            return;
        }
        if let Err(msg) = super::mic_permission::require_authorized() {
            if let Some(d) = self.daemon() {
                crate::route::hud::emit(&d, crate::route::hud::Phase::Error { msg });
            }
            return;
        }
        // Capture before entering the microphone gate: this can dispatch to
        // AppKit, whose thread must remain free to stop/start a meeting.
        let target = InsertionTarget::capture(self.app.as_ref());
        {
            let _microphone = crate::meetings::microphone_operation_lock();
            if self
                .app
                .as_ref()
                .and_then(|app| app.try_state::<Arc<crate::meetings::Meetings>>())
                .is_some_and(|meetings| meetings.is_recording())
            {
                if let Some(d) = self.daemon() {
                    crate::route::hud::emit(
                        &d,
                        crate::route::hud::Phase::Error {
                            msg: "Сначала останови запись встречи, затем начни диктовку.".into(),
                        },
                    );
                }
                return;
            }
            let mut guard = match self.capturing.lock() {
                Ok(g) => g,
                Err(e) => {
                    crate::log::line(&format!("[dictation] on_press lock: {e}"));
                    return;
                }
            };
            if guard.is_some() || self.processing.load(Ordering::Acquire) {
                // Уже пишем — идемпотентный пропуск.
                return;
            }
            // Захват через общий хаб (без преролла — PTT пишет с момента нажатия).
            self.insertion.begin();
            *guard = Some((
                self.hub.open_capture(false),
                std::time::Instant::now(),
                target,
            ));
        } // лок захвата отпущен ДО прогрева (spawn питона его не держит)
          // Греем STT-модель ПОКА человек говорит: к отпусканию клавиши она уже
          // загружена (прячет cold-start после idle-stop). Неблокирующий вызов.
        self.service.warm();
        // видимая фаза «Слушаю…» в тосте (PTT — без кольца отсчёта, secs=0)
        if let Some(d) = self.daemon() {
            // взаимодействие началось: пока диктуем — уведомления откладываются, wake
            // подавлён (не сработает на собственную речь). Парный leave — в on_release.
            d.interaction.enter();
            // пауза чужого медиа (музыки) на время диктовки — как при озвучке.
            d.duck_media_for_capture();
            crate::route::hud::emit(&d, crate::route::hud::Phase::Listening { secs: 0 });
        }
        crate::log::line("[dictation] запись начата");
    }

    /// Остановить захват, транскрибировать и вставить текст. Если захват не
    /// шёл — no-op. Тяжёлая работа (transcribe) выполняется в отдельном потоке.
    pub fn on_release(&self) {
        let session = {
            let mut guard = match self.capturing.lock() {
                Ok(g) => g,
                Err(e) => {
                    crate::log::line(&format!("[dictation] on_release lock: {e}"));
                    return;
                }
            };
            guard.take().map(|(s, _started, target)| {
                self.processing.store(true, Ordering::Release);
                (s, target)
            })
        };

        let Some((session, target)) = session else {
            // Нет активного захвата — no-op.
            return;
        };
        self.finish_session(session, target);
    }

    /// Общий финал диктовки: транскрибировать накопленное и вставить текст.
    /// Зовётся из on_release (обычный путь) и из watchdog'а залипшего PTT —
    /// принудительное завершение тоже отдаёт человеку его текст, а не тишину.
    fn finish_session(&self, session: CaptureSession, target: InsertionTarget) {
        let service = self.service.clone();
        let daemon = self.daemon();
        let processing = self.processing.clone();
        let insertion = self.insertion.clone();
        let attempt_id = insertion.current();
        std::thread::spawn(move || {
            // Взаимодействие завершится при выходе из потока (ЛЮБОЙ путь, включая
            // ранние return) — RAII-гард снимает счётчик и сливает отложенные
            // уведомления ровно один раз. Парный enter был в on_press.
            struct LeaveGuard(
                Option<std::sync::Arc<crate::daemon::Daemon>>,
                Arc<AtomicBool>,
            );
            impl Drop for LeaveGuard {
                fn drop(&mut self) {
                    if let Some(d) = &self.0 {
                        // возобновить медиа, поставленное на паузу на старте диктовки
                        d.unduck_media_for_capture();
                        d.interaction_leave_and_flush();
                    }
                    self.1.store(false, Ordering::Release);
                }
            }
            let _leave = LeaveGuard(daemon.clone(), processing);
            // видимая фаза «Анализирую…» — пока идёт finish + транскрипция
            if let Some(d) = &daemon {
                crate::route::hud::emit(d, crate::route::hud::Phase::Analyzing);
            }
            // ── finish() → PCM ───────────────────────────────────────────────
            let pcm = match session.finish() {
                Ok(p) => p,
                Err(e) => {
                    crate::log::line(&format!("[dictation] finish: {e}"));
                    if let Some(d) = &daemon {
                        crate::route::hud::emit(
                            d,
                            crate::route::hud::Phase::Error {
                                msg: "захват не удался".into(),
                            },
                        );
                    }
                    return;
                }
            };
            if pcm.is_empty() {
                crate::log::line("[dictation] пустой PCM-буфер, пропуск");
                if let Some(d) = &daemon {
                    crate::route::hud::emit(d, crate::route::hud::Phase::Empty);
                }
                return;
            }

            // ── VAD-гейт (Tier 3): не пускать не-речь в STT ───────────────────
            // Фон/музыка/тишина → STT пропускаем, чтобы не «придумать» слова.
            // Отключаемо настройкой stt.noiseGate («шумодав»).
            let noise_gate = daemon
                .as_ref()
                .map(|d| {
                    crate::stt::config::SttConfig::from_settings(&d.settings.load()).noise_gate
                })
                .unwrap_or(true);
            if noise_gate && !crate::stt::vad_silero::has_speech(&pcm) {
                crate::log::line("[dictation] VAD: речи нет — пропуск (фон/шум/тишина)");
                if let Some(d) = &daemon {
                    crate::route::hud::emit(d, crate::route::hud::Phase::Empty);
                }
                return;
            }

            // ── transcribe() → text ──────────────────────────────────────────
            let opts = service.options();
            let (text, raw_text) = match service.transcribe_with_raw(&pcm, &opts) {
                Ok((r, raw)) => (r.text, raw),
                Err(e) => {
                    crate::log::line(&format!("[dictation] transcribe: {e}"));
                    if let Some(d) = &daemon {
                        crate::route::hud::emit(
                            d,
                            crate::route::hud::Phase::Error {
                                msg: "распознавание не удалось".into(),
                            },
                        );
                    }
                    return;
                }
            };
            // Anti-hallucination (Tier 2): сперва схлопнуть дегенеративные петли
            // декодера («Писать. Писать…»), затем убрать известные фразы-галлюцинации
            // — общая сетка для whisper и qwen (qwen не отдаёт посегментный
            // no_speech_prob, поэтому чистим здесь).
            let collapsed = crate::stt::dehallucinate::collapse_repeats(text.trim());
            let text = crate::stt::dehallucinate::scrub(&collapsed);
            if text.is_empty() {
                crate::log::line("[dictation] пустой результат транскрипции, пропуск");
                if let Some(d) = &daemon {
                    crate::route::hud::emit(d, crate::route::hud::Phase::Empty);
                }
                return;
            }
            crate::log::line(&format!(
                "[dictation] транскрипция готова: chars={}",
                text.chars().count()
            ));
            // Optional formatting uses one configured service-LLM pass. Semantic
            // rewrites (commit/prompt/translation) require a manual history action.
            // Fail-safe: на сбой/таймаут остаётся исходный текст без применения.
            let (text, applied) = match &daemon {
                Some(d) if d.prompts.smart() => {
                    let p = crate::stt::prompts::smart_transform_prompt(&text);
                    match tauri::async_runtime::block_on(crate::claude_bin::run_service_text_transform(
                        &p,
                        std::time::Duration::from_secs(12),
                    )) {
                        Some(raw) => {
                            let r = crate::stt::prompts::parse_smart_result(&raw, &text);
                            if let Some(a) = &r.applied {
                                crate::log::line(&format!(
                                    "[dictation] умный промпт применён: {a}"
                                ));
                            }
                            (r.text, r.applied)
                        }
                        None => (text, None),
                    }
                }
                _ => (text, None),
            };

            // история «что я говорил» (с пометкой стиля)
            let saved = if let Some(d) = &daemon {
                let (id, saved) =
                    d.transcripts
                        .push_dictated(&text, &raw_text, applied.as_deref(), true);
                // сохранить СЖАТОЕ аудио диктовки → можно перегенерировать
                // распознавание, если анализ дал ошибку/мусор. Best-effort.
                if id != 0 {
                    if let Err(e) = crate::stt::audio_store::save(id, &pcm) {
                        crate::log::line(&format!("[dictation] сохранение аудио: {e}"));
                    }
                }
                saved
            } else {
                false
            };

            // ── insert_text() → ⌘V ──────────────────────────────────────────
            let app = daemon.as_ref().map(|d| &d.app);
            let delivery = super::insert::insert_text_cancellable(&text, app, &target, &|| {
                insertion.is_cancelled(attempt_id)
            });
            let inserted = delivery.verdict == super::insert::InsertVerdict::Confirmed;
            if let Some(e) = &delivery.error {
                crate::log::line(&format!("[dictation] insert_text: {e}"));
            }
            crate::log::line(&format!(
                "[dictation] вставка {}",
                if inserted {
                    "подтверждена (тост 2с)"
                } else {
                    "не подтверждена (доступно восстановление текста)"
                }
            ));
            // «Услышал …» — ПОСЛЕ вставки: тост знает её исход и живёт короче,
            // если текст уже на месте (inserted). Задержка эмита ~0.2с не заметна.
            if let Some(d) = &daemon {
                let mut payload =
                    crate::route::hud::hud_payload(crate::route::hud::Phase::Dictated {
                        text: text.clone(),
                        inserted,
                        copied: delivery.copied,
                        saved,
                        paste_sent: delivery.paste_sent,
                        insertion_error: delivery.error,
                    });
                payload["insertionBlocked"] = if delivery.permission_required {
                    serde_json::json!("accessibility")
                } else {
                    serde_json::Value::Null
                };
                payload["insertionAttemptId"] = serde_json::json!(attempt_id);
                payload["insertionCancelled"] = serde_json::json!(delivery.cancelled);
                payload["rawText"] = serde_json::json!(raw_text);
                payload["formatted"] = serde_json::json!(text != raw_text);
                payload["formatStyle"] = serde_json::json!(applied);
                crate::windows::hud_emit(d, payload);
            }
        });
    }

    /// Watchdog залипшего PTT: если сессия захвата открыта дольше `max` (потерян
    /// key-up хоткея / паника потребителя), принудительно её ЗАВЕРШИТЬ — иначе
    /// микрофон держится бессрочно («индикатор вечно слушает»). Завершение идёт
    /// обычным путём (finish_session): накопленная речь транскрибируется и
    /// вставляется, HUD обновляется, медиа возвращается LeaveGuard'ом — человек
    /// не теряет надиктованное, даже если порог сработал на живой длинной
    /// диктовке. Возвращает true, если сессию пришлось завершить. Зовётся
    /// периодически из супервизора.
    pub fn abort_if_stuck(&self, max: std::time::Duration) -> bool {
        let stuck = {
            let mut guard = match self.capturing.lock() {
                Ok(g) => g,
                Err(e) => {
                    crate::log::line(&format!("[dictation] abort_if_stuck lock: {e}"));
                    return false;
                }
            };
            match guard.as_ref() {
                Some((_, started, _)) if started.elapsed() >= max => {
                    guard.take().map(|(s, _, target)| {
                        self.processing.store(true, Ordering::Release);
                        (s, target)
                    })
                }
                _ => None,
            }
        }; // лок захвата отпущен ДО finish (Drop сессии) — без взаимоблокировки
        let Some((session, target)) = stuck else {
            return false;
        };
        crate::log::line(&format!(
            "[dictation] PTT дольше {}с — принудительное завершение диктовки",
            max.as_secs()
        ));
        self.finish_session(session, target);
        true
    }

    /// Вспомогательный предикат: возвращает true, если захват активен.
    /// Используется в тестах для проверки state machine.
    pub fn is_capturing(&self) -> bool {
        self.capturing.lock().map(|g| g.is_some()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::config::SttConfig;

    fn make_dictation() -> Arc<Dictation> {
        // SttService с дефолтным конфигом (qwen3-0.6b, но сайдкар не запущен).
        let svc = SttService::new(SttConfig::default());
        // Хаб без AppHandle; в тестах ensure_running() — no-op (живой микрофон не трогаем).
        let hub = super::super::hub::AudioHub::new(None, None);
        Dictation::new_headless(svc, hub)
    }

    // on_release без предшествующего on_press — no-op (не паникует)
    #[test]
    fn on_release_without_press_is_noop() {
        let d = make_dictation();
        assert!(!d.is_capturing());
        d.on_release(); // не должен паниковать
        assert!(!d.is_capturing());
    }

    // on_release с None-сессией идемпотентен: повторный вызов тоже no-op
    #[test]
    fn double_on_release_is_noop() {
        let d = make_dictation();
        d.on_release();
        d.on_release(); // второй — тоже нормально
        assert!(!d.is_capturing());
    }

    // Начальное состояние: захват не активен
    #[test]
    fn initial_state_not_capturing() {
        let d = make_dictation();
        assert!(!d.is_capturing());
    }

    #[test]
    fn press_during_transcription_cannot_start_an_overlapping_session() {
        let d = make_dictation();
        d.processing.store(true, Ordering::Release);
        d.on_press();
        assert!(!d.is_capturing());
        d.processing.store(false, Ordering::Release);
        d.on_press();
        assert!(d.is_capturing());
    }

    // Залипший PTT (потерянный key-up): сессия старше порога — авто-освобождение.
    #[test]
    fn abort_if_stuck_releases_old_session() {
        let d = make_dictation();
        d.on_press();
        assert!(d.is_capturing(), "on_press открыл сессию");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let aborted = d.abort_if_stuck(std::time::Duration::from_millis(1));
        assert!(aborted, "старая сессия должна быть освобождена");
        assert!(!d.is_capturing(), "после аборта захват очищен");
    }

    // Свежая сессия (в пределах порога) не трогается.
    #[test]
    fn abort_if_stuck_keeps_fresh_session() {
        let d = make_dictation();
        d.on_press();
        let aborted = d.abort_if_stuck(std::time::Duration::from_secs(60));
        assert!(!aborted, "свежую сессию не трогаем");
        assert!(d.is_capturing());
    }

    // Нет активного захвата — абортить нечего, no-op.
    #[test]
    fn abort_if_stuck_noop_when_idle() {
        let d = make_dictation();
        assert!(!d.abort_if_stuck(std::time::Duration::from_millis(0)));
    }

    // Двойной on_press не паникует (идемпотентный guard)
    // Реальный CaptureSession::start в тестах не открываем (нет микрофона CI),
    // тест проверяет только что is_capturing() не ломается при повторном вызове.
    #[test]
    fn double_press_guard_logic_no_panic() {
        let d = make_dictation();
        // Первый on_press может завершиться с ошибкой (нет реального микрофона),
        // но не должен паниковать.
        d.on_press();
        // Второй on_press: если первый не поставил сессию — всё равно no-op.
        d.on_press();
        // Независимо от результата — on_release не должен паниковать.
        d.on_release();
    }
}
