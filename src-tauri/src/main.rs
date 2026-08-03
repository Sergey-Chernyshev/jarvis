//! Jarvis — демон + меню-бар + панель (Rust/Tauri).
//!
//! Main-процесс и есть демон: слушает unix-сокет ~/.jarvis/run.sock,
//! на который jarvis-hook кидает события из хуков Claude Code.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[allow(dead_code)] // UI-потребитель подключается в фазе 7 (chat UI)
mod agent;
mod agent_vm;
mod agent_vm_cli;
mod agent_vm_terminal;
mod app_command_inventory;
#[allow(dead_code)] // Codex-методы наполняются по инкрементам (codex CLI support)
mod backend;
#[allow(dead_code)] // проекции/фасады подключаются по фазам (инкр. 8)
mod capability;
mod claude_bin;
mod commands_catalog;
mod config_health;
mod convo; // голосовой разговор: снапшот → Haiku-план → скилы → голосовой ответ (п/п-2)
mod coord; // координация голоса: пока юзер диктует/говорит — уведомления ждут, wake подавлен
mod daemon;
mod entities; // реестр сущностей ядра (спека plugin-system §6.4)
mod git; // ветка сессии из .git/HEAD — фоллбэк, когда её нет в транскрипте (#24)
mod gitdiff; // дифф файла для таба «Изменения» вьюера документов (спека 2026-07-18 §3.2)
mod history;
mod install;
mod ipc;
mod launch; // запуск новой/возобновляемой сессии в терминале из вкладки «Проекты»
mod limits;
mod log;
mod macos;
mod metrics;
mod model;
mod onboarding;
mod plugins;
mod power;
mod project_folder_picker;
mod route; // голосовая маршрутизация: скоринг → tie-break → пикер → stage-then-send
mod ru;
mod screen_prompt;
mod server;
mod settings;
mod shutdown;
#[allow(dead_code)] // STT-потребители подключаются в фазах 4-6 (инкр. 9)
mod stt;
mod tail;
mod terminal;
mod tmux;
mod transcript;
mod tray;
mod turns;
mod turnsum;
mod usage;
mod util;
mod voice;
mod wakeword; // wake-word детектор + шов верификации
mod windows;

use std::io;
use std::sync::Arc;
use std::time::Duration;
use tauri::Manager;

use daemon::Daemon;

macro_rules! build_app_invoke_handler {
    ($(($name:literal, $handler:path, $webviews:expr)),* $(,)?) => {
        tauri::generate_handler![$($handler),*]
    };
}

fn headless_from(value: Option<&std::ffi::OsStr>) -> bool {
    value
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

fn is_headless() -> bool {
    headless_from(std::env::var_os("JARVIS_HEADLESS").as_deref())
}

fn main() {
    if let Some(exit_code) = agent_vm_cli::maybe_run() {
        std::process::exit(exit_code);
    }

    let mut builder = tauri::Builder::default();

    // single-instance — только в проде; в dev-сборке (JARVIS_DEV=1) НЕ ставим,
    // чтобы dev и установленный прод крутились рядом, не гася друг друга.
    if std::env::var("JARVIS_DEV").is_err() && !is_headless() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            windows::show_panel(&Daemon::get(app));
        }));
    }

    builder
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    use tauri_plugin_global_shortcut::ShortcutState;
                    let d = Daemon::get(app);

                    // PTT-диктовка обрабатывается на ОБОИХ событиях (Pressed + Released).
                    if ipc::is_dictation_hotkey(&d, shortcut) {
                        match event.state() {
                            ShortcutState::Pressed => d.dictation.on_press(),
                            ShortcutState::Released => d.dictation.on_release(),
                        }
                        return;
                    }

                    // Остальные хоткеи — только на Pressed.
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    // ⌘⌥J — тихий; ⌘⌥C — «Продолжить»; ⌘⌥R — повтор увед.;
                    // ⌘⌥M — без звука; ⌘⌥1..9 — выбор варианта; прочее — панель.
                    if ipc::is_quiet_hotkey(&d, shortcut) {
                        d.toggle_quiet();
                    } else if ipc::is_continue_hotkey(&d, shortcut) {
                        if let Some(sid) = d.last_session() {
                            let h = app.clone();
                            tauri::async_runtime::spawn(async move {
                                let _ = ipc::session_continue(h, sid).await;
                            });
                        }
                    } else if ipc::is_repeat_hotkey(&d, shortcut) {
                        d.repeat_last_toast();
                    } else if ipc::is_mute_hotkey(&d, shortcut) {
                        d.toggle_mute();
                    } else if let Some(n) = ipc::is_select_hotkey(&d, shortcut) {
                        d.answer_question_hotkey(n);
                    } else {
                        windows::toggle_hotkey_panel(&d);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(crate::app_command_inventory::with_app_commands!(
            build_app_invoke_handler
        ))
        .setup(|app| {
            // Профильный lock ДО Daemon::new: второй процесс того же JARVIS_DIR
            // не стартует, но prod/dev и чужие listeners больше не убиваются.
            install::prepare_clean_start().map_err(|err| {
                io::Error::new(io::ErrorKind::AlreadyExists, format!("Jarvis profile already running: {err}"))
            })?;

            // One-way v1 ownership migration is reconciled before any
            // headless/UI branch or subsystem construction. After startup,
            // persistent power mutations belong only to the attested helper.
            // Failure is fail-closed for later arm, but never blocks startup.
            let power_recovery = power::recover_on_startup();
            crate::log::line(&format!(
                "[power] startup recovery {}",
                power_recovery.summary()
            ));

            // миграция схемы settings.json ДО первого чтения настроек (Daemon::new
            // их читает). Сейчас no-op v0→v1; задел под ломающие изменения формата.
            settings::Store::new().migrate_on_startup();
            match jarvis_secret_store::migrate_legacy_claude_secret(
                &crate::util::jarvis_dir().join("settings.json"),
                &jarvis_secret_store::MacKeychainStore,
            ) {
                Ok(report) if report.migrated => {
                    crate::log::line("[security] Claude credential migrated to Keychain");
                }
                Ok(_) => {}
                Err(error) => {
                    crate::log::line(&format!(
                        "[security] Claude credential migration deferred: {error}"
                    ));
                }
            }

            if let Err(err) = plugins::install::install_bundled() {
                crate::log::line(&format!("[plugins] Agent VM install skipped: {err}"));
            }

            // чистое меню-бар приложение: без иконки в доке
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let d = Arc::new(Daemon::new(app.handle().clone()));
            app.manage(d.clone());
            shutdown::install(app.handle().clone());

            // Конфиг служебного LLM («Под капотом»: Claude/Codex + модель) — из
            // настроек в процесс-глобал, чтобы run_service_llm сразу его видел.
            crate::claude_bin::set_service_config(
                crate::claude_bin::ServiceConfig::from_settings(&d.settings.load()),
            );

            d.restore_state(); // реестр переживает перезапуск

            // Smoke/plugin-host процессы могут использовать daemon/socket, но не
            // имеют права открывать пользовательские окна или регистрировать
            // глобальные интерактивные ресурсы.
            if is_headless() {
                tauri::async_runtime::spawn(server::serve(d.clone()));
                crate::log::line("[startup] JARVIS_HEADLESS=1 — UI и tray отключены");
                return Ok(());
            }

            windows::create_panel(app.handle())?;
            windows::create_toast(app.handle())?;
            tray::init(&d)?;

            // первый запуск без интеграции — онбординг; иначе показываем панель,
            // чтобы запуск приложения был видимым (а не «ничего не открылось»).
            if d.settings.health().has_errors() || !install::integration_health().ok() {
                let _ = windows::create_onboarding(app.handle());
            } else {
                windows::show_panel(&d);
            }

            // unix-сокет — канал событий от хуков
            tauri::async_runtime::spawn(server::serve(d.clone()));
            d.plugins.init(&d);

            // плагины питания (Не спать, Крышка) — после трея:
            // их changed() обновляет title
            power::Power::init(&d);

            // Прогрев кэша размеров моделей в фоне: первое открытие настроек не
            // ждёт обхода venv (~21k файлов) — см. install::dir_size_cached.
            std::thread::spawn(|| {
                let _ = crate::install::model_inventory();
            });

            // Самопроверка интеграции на старте («тесты под капотом»): лечим дрейф
            // регистраций хуков — главный баг, из-за которого codex молчит (stale
            // prod-путь ~/.jarvis после смены на dev-профиль ~/.jarvis-dev) — и
            // пишем health-снимок в лог. Дёшево и без сети, но в отдельном потоке,
            // чтобы не тормозить создание окна.
            std::thread::spawn(|| {
                crate::install::reconcile_hooks(&|s| {
                    if !s.msg.is_empty() {
                        crate::log::line(&format!("[integration] {}: {}", s.phase, s.msg));
                    }
                });
                let h = crate::install::integration_health();
                crate::log::line(&format!(
                    "[integration] dir={} hook_bin={} sock={} claude_hooks={} \
                     codex_present={} codex_hooks={} codex_shim={} → {}",
                    h.jarvis_dir, h.hook_bin, h.socket, h.claude_hooks_ok,
                    h.codex_present, h.codex_hooks_ok, h.codex_shim,
                    if h.ok() { "OK" } else { "ВНИМАНИЕ: интеграция неполная" },
                ));
            });

            let hk0 = ipc::action_accel(&d, ipc::HkAction::Panel).unwrap_or_default();
            if let Err(e) = ipc::register_hotkey(&d, &hk0) {
                eprintln!("[jarvis] хоткей не зарегистрировался: {e}");
            }
            ipc::register_quiet_hotkey(&d); // тумблер тихого режима (⌘⌥J)
            ipc::register_continue_hotkey(&d); // «Продолжить» последнюю сессию (⌘⌥C)
            ipc::register_dictation_hotkey(&d); // PTT-диктовка (F8)
            ipc::register_repeat_hotkey(&d); // повторить последнее уведомление (⌘⌥R)
            ipc::register_mute_hotkey(&d); // без звука / mute (⌘⌥M)
            // ⌘⌥1..9 (выбор варианта) регистрируются динамически в do_push,
            // только пока висит активный вопрос — см. ipc::set_select_hotkeys

            spawn_timers(&d);

            // updater: тихая проверка на старте; есть свежий релиз — ставим (применится
            // на следующем запуске). Гейт настройкой autoUpdate (по умолчанию вкл).
            if d.settings.load().get("autoUpdate").and_then(|v| v.as_bool()).unwrap_or(true) {
                use tauri_plugin_updater::UpdaterExt;
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Ok(updater) = handle.updater() {
                        if let Ok(Some(update)) = updater.check().await {
                            crate::log::line(&format!("[updater] доступна версия {}", update.version));
                            match update.download_and_install(|_, _| {}, || {}).await {
                                Ok(()) => crate::log::line("[updater] обновление установлено, применится при следующем запуске"),
                                Err(e) => crate::log::line(&format!("[updater] не удалось обновиться: {e}")),
                            }
                        }
                    }
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            match event {
                // ⌘W и крестик — просто прячем, демон живёт
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    windows::hide_panel(&Daemon::get(window.app_handle()));
                }
                // клик вне панели — спрятать. Но с задержкой и перепроверкой:
                // навигация стрелками перерисовывает DOM (render() пересоздаёт и
                // рефокусит queryEl), отчего WKWebView даёт ложный blur→focus за
                // один кадр. Гасим только если фокус реально ушёл из приложения и
                // не вернулся за 120 мс — иначе панель моргала бы на каждой стрелке.
                tauri::WindowEvent::Focused(false) => {
                    let w = window.clone();
                    let app = window.app_handle().clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(120));
                        if !w.is_focused().unwrap_or(false) && w.is_visible().unwrap_or(false) {
                            windows::hide_panel(&Daemon::get(&app));
                        }
                    });
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("jarvis: не удалось собрать приложение")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let d = Daemon::get(app);
                let report = shutdown::cleanup(&d);
                if !report.complete() {
                    crate::log::line(&format!(
                        "[shutdown] Exit fallback remains incomplete: {report:?}"
                    ));
                }
            }
        });
}

/// Все периодические задачи демона — расписание из Electron-версии.
fn spawn_timers(d: &Arc<Daemon>) {
    // сверка живости сессий (мёртвый pid/пана → выселяем): сразу и раз в 30с
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            dd.reconcile_sessions().await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });

    // снять ложный лимит-баннер по официальному usage — раз в минуту
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            ipc::reconcile_limit(&dd);
        }
    });

    // детект интерактивных промптов на экране — раз в 7с по всем сессиям
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(7)).await;
            let ids: Vec<String> = dd.sessions.lock().unwrap().keys().cloned().collect();
            for sid in ids {
                screen_prompt::detect_stuck_prompt(&dd, &sid).await;
            }
        }
    });

    // Секундный пульс UI-сторожей питания. Helper lease renewal lives in its
    // own cancellable worker so shutdown can stop and join it before release.
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            power::Power::tick(&dd).await;
        }
    });

    // секундный supervisor внешних плагинов; первый tick после bind UDS.
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            dd.plugins.tick(&dd);
        }
    });

    // Закреплённые project VM стартуют после PluginHost строго по одной.
    // Новые agent turns без пользовательского prompt здесь не создаются.
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        crate::agent_vm::autostart_profiles(dd).await;
    });

    // супервизор Silero-сайдкара: раз в 5с перезапускаем, если упал
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let v = dd.voice.clone();
            let _ = tokio::task::spawn_blocking(move || v.tick()).await;
        }
    });

    // супервизор Qwen3-MLX-сайдкара (STT): раз в 5с перезапускаем, если упал
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let s = dd.stt.clone();
            let _ = tokio::task::spawn_blocking(move || s.tick()).await;
        }
    });

    // супервизор wake-word (инкр. 10): раз в 5с поднимаем consumer-поток, если умер
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let w = dd.wake.clone();
            let _ = tokio::task::spawn_blocking(move || w.tick()).await;
        }
    });

    // watchdog общего аудио-входа (инкр. 10): раз в 5с проверяем живость захвата
    // (устройство могло отвалиться без явной ошибки) и перезапускаем при застое
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let a = dd.audio.clone();
            let _ = tokio::task::spawn_blocking(move || a.tick()).await;
        }
    });

    // watchdog залипшего PTT: раз в 10с принудительно ЗАВЕРШАЕМ (транскрипция +
    // вставка, не выброс) сессию диктовки старше 5 минут. Диктовка — hold-PTT,
    // поэтому по одному времени «залип» не отличить от честного долгого
    // удержания: порог должен быть заведомо больше реальной длинной диктовки
    // (60с рубил живую речь: медиа возобновлялось прямо в микрофон, HUD
    // застревал на «Слушаю…», следующая транскрипция шла под музыку → мусор).
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let dict = dd.dictation.clone();
            let _ =
                tokio::task::spawn_blocking(move || dict.abort_if_stuck(Duration::from_secs(300)))
                    .await;
        }
    });

    // режим логов/диагностики: раз в 15с пишем метрики (RAM/CPU/счётчики) в лог
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            dd.sample_metrics().await;
        }
    });

    // effort-уровни из `claude --help`
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        dd.detect_effort_levels().await;
    });

    // usage: backfill/инкрементальные сканы транскриптов (раз в 30с)
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        let initial = if dd.usage.backfilled() { 3000 } else { 500 };
        tokio::time::sleep(Duration::from_millis(initial)).await;
        loop {
            let u = dd.usage.clone();
            let _ = tokio::task::spawn_blocking(move || u.scan()).await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });

    // официальные лимиты подписки — через 5с и далее раз в 5 минут
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            dd.usage.fetch_official(&dd).await;
            tokio::time::sleep(Duration::from_secs(5 * 60)).await;
        }
    });

    // история чатов по проектам — через 1.2с и далее раз в минуту
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        loop {
            let h = dd.history.clone();
            let _ = tokio::task::spawn_blocking(move || h.scan()).await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });

    // hover над тостами: курсор ловим нативно (mouseenter в WKWebView молчит,
    // пока активно чужое приложение). Тик 200мс — пауза ощущается мгновенно.
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if let Some(toast) = dd.app.get_webview_window("toast") {
                macos::poll_toast_hover(&toast);
            }
        }
    });
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn headless_flag_accepts_only_explicit_truthy_values() {
        assert!(headless_from(Some(OsStr::new("1"))));
        assert!(headless_from(Some(OsStr::new("true"))));
        assert!(!headless_from(Some(OsStr::new("0"))));
        assert!(!headless_from(None));
    }
}
