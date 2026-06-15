//! Ядро демона: реестр сессий, редьюсер событий хуков и фоновые задачи.
//!
//! Состояния сессии: idle → working → (waiting ⇄ working) → done → working → ...
//!   session-start → idle        (сессия открыта, ничего не делает)
//!   prompt        → working     (юзер отправил промпт)
//!   notification  → waiting     (нужен пермишен / ждёт ввода) → уведомление
//!   stop          → done        (закончил ответ)              → уведомление
//!   session-end   → сессия удаляется
//!
//! Редьюсер мутирует реестр строго под локом и возвращает список эффектов;
//! всё асинхронное (tmux, транскрипты, haiku) исполняется после разблокировки —
//! лок никогда не живёт через await.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager};

use crate::model::{
    Question, QuestionItem, QuestionOption, Session, Status, Subagent, TaskBoard, TaskItem,
};
use crate::util::*;
use crate::{claude_bin, ru, settings, tail, tmux, transcript, windows};

pub struct Daemon {
    pub app: AppHandle,
    pub sessions: Mutex<HashMap<String, Session>>,
    pub settings: settings::Store,
    pub translator: ru::Translator,
    pub usage: std::sync::Arc<crate::usage::Usage>,
    pub history: std::sync::Arc<crate::history::History>,
    pub commands: crate::commands_catalog::Catalog,
    pub limits: crate::limits::Limits,
    pub power: crate::power::Power,
    pub tail: tail::TailHandle,
    pub panel_focus_mode: AtomicBool,
    pub effort_levels: Mutex<Vec<String>>,
    toast_seq: AtomicU64,
    /// Окно тостов загрузилось и слушает события (до этого — буферим).
    pub toast_ready: AtomicBool,
    pub pending_toasts: Mutex<Vec<(&'static str, serde_json::Value)>>,
    push_pending: AtomicBool,
    persist_pending: AtomicBool,
    /// In-flight guards фоновых задач: (вид, session_id).
    busy: Mutex<HashSet<(&'static str, String)>>,
    /// Время последнего РЕАЛЬНОГО prompt-события (хук UserPromptSubmit), мс.
    /// Так подтверждаем доставку ответа из Jarvis: хук сработал → текст дошёл.
    last_prompt_at: Mutex<HashMap<String, i64>>,
    /// Голос (инкремент 7): озвучка событий локальным TTS. Fail-safe.
    pub voice: std::sync::Arc<crate::voice::Voice>,
}

/// Побочные эффекты редьюсера — исполняются после освобождения лока реестра.
enum Effect {
    ResolveTmuxName { sid: String, pane: String },
    ResolveGuiApp { sid: String, pid: i64 },
    RefreshMeta { sid: String },
    RefreshTasks { sid: String },
    /// Выжимка «над чем идёт работа» для строки списка (на промт юзера).
    GenSummary { sid: String },
    /// Уведомление «спрашивает» (карточка вопроса).
    NotifyWaiting { title: String, body: String, sid: String },
    /// Завершение: одна ИИ-выжимка результата — и в строку списка, и в тост.
    DoneSummary { sid: String },
    /// Ручной /compact завершился (session-start, source=compact) — явный тост.
    NotifyCompact { title: String, sid: String },
    StopFailure { sid: String, payload: Value },
}

impl Daemon {
    pub fn new(app: AppHandle) -> Self {
        // голос строим до литерала: cfg читаем из тех же settings.json
        let settings = settings::Store::new();
        let vcfg = crate::voice::config::VoiceConfig::from_settings(&settings.load());
        let voice = crate::voice::Voice::new(
            &vcfg,
            jarvis_dir().join("piper").join("piper"),
            jarvis_dir().join("silero"),
        );
        // прогрев движка на старте — первая реальная реплика не ловит холодный старт
        {
            let v = voice.clone();
            std::thread::spawn(move || v.warmup());
        }
        Self {
            app,
            sessions: Mutex::new(HashMap::new()),
            settings,
            translator: ru::Translator::load(),
            usage: std::sync::Arc::new(crate::usage::Usage::load()),
            history: std::sync::Arc::new(crate::history::History::load()),
            commands: crate::commands_catalog::Catalog::new(),
            limits: crate::limits::Limits::new(),
            power: crate::power::Power::new(),
            tail: tail::TailHandle::new(),
            panel_focus_mode: AtomicBool::new(false),
            effort_levels: Mutex::new(
                ["low", "medium", "high", "xhigh", "max"].map(String::from).to_vec(),
            ),
            toast_seq: AtomicU64::new(0),
            toast_ready: AtomicBool::new(false),
            pending_toasts: Mutex::new(Vec::new()),
            push_pending: AtomicBool::new(false),
            persist_pending: AtomicBool::new(false),
            busy: Mutex::new(HashSet::new()),
            last_prompt_at: Mutex::new(HashMap::new()),
            voice,
        }
    }

    pub fn get(app: &AppHandle) -> std::sync::Arc<Daemon> {
        app.state::<std::sync::Arc<Daemon>>().inner().clone()
    }

    /* ================= снапшот и доставка состояния ================= */

    pub fn snapshot(&self) -> Vec<Session> {
        let mut list: Vec<Session> = self.sessions.lock().unwrap().values().cloned().collect();
        crate::model::sort_snapshot(&mut list);
        list
    }

    /// Троттлим: tool-события сыплются часто, рендерить чаще ~8 раз/с незачем.
    pub fn push(self: &std::sync::Arc<Self>) {
        if self.push_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let d = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            d.push_pending.store(false, Ordering::SeqCst);
            d.do_push();
        });
    }

    fn do_push(self: &std::sync::Arc<Self>) {
        let list = self.snapshot();
        self.power.on_sessions(self, &list); // плагины первыми — бейджи к трею уже свежие
        windows::emit_to_panel(&self.app, "state", &list);
        windows::emit_to_panel(&self.app, "plugins", &self.power.statuses(self));
        crate::tray::update(self, &list);
        self.persist();
    }

    /* ================= персистентность реестра ================= */
    /* Реестр в памяти — перезапуск демона не должен «ронять» сессии. */

    fn state_file() -> std::path::PathBuf {
        jarvis_dir().join("state.json")
    }

    pub fn write_state_now(&self) {
        let arr: Vec<Session> = self.sessions.lock().unwrap().values().cloned().collect();
        let _ = std::fs::create_dir_all(jarvis_dir());
        if let Ok(json) = serde_json::to_string(&arr) {
            let _ = std::fs::write(Self::state_file(), json + "\n");
        }
    }

    fn persist(self: &std::sync::Arc<Self>) {
        if self.persist_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let d = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            d.persist_pending.store(false, Ordering::SeqCst);
            d.write_state_now();
        });
    }

    pub fn restore_state(&self) {
        let Ok(raw) = std::fs::read_to_string(Self::state_file()) else { return };
        let Ok(arr) = serde_json::from_str::<Vec<Session>>(&raw) else { return };
        let cutoff = now_ms() - 24 * 3600 * 1000; // суточный мусор не тащим
        let mut sessions = self.sessions.lock().unwrap();
        for mut s in arr {
            if s.id.is_empty() || s.updated_at <= cutoff {
                continue;
            }
            // англ. заголовки доезжают переводом из кэша
            if let Some(t) = &s.title {
                s.title = Some(self.translator.ru(t).0);
            }
            if let Some(t) = &s.task {
                s.task = Some(self.translator.ru(t).0);
            }
            sessions.insert(s.id.clone(), s);
        }
    }

    /* ================= русификация ================= */

    /// ru() с автозапуском насоса переводов (как setTimeout(pump, 300) в JS).
    pub fn ru(self: &std::sync::Arc<Self>, text: &str) -> String {
        let (out, needs_pump) = self.translator.ru(text);
        if needs_pump {
            let d = self.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                d.pump_translations().await;
            });
        }
        out
    }

    async fn pump_translations(self: std::sync::Arc<Self>) {
        loop {
            let Some(batch) = self.translator.take_batch() else { return };
            let prompt = ru::Translator::prompt_for(&batch);
            let out = claude_bin::run_haiku(&prompt, Duration::from_secs(60)).await;
            let changed = self.translator.finish_batch(&batch, out.as_deref());
            if changed {
                self.apply_translations();
            }
            if self.translator.queue_len() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(800)).await;
        }
    }

    /// Долить готовые переводы в реестр (title/task хранят оригинал до перевода).
    fn apply_translations(self: &std::sync::Arc<Self>) {
        let mut changed = false;
        {
            let mut sessions = self.sessions.lock().unwrap();
            for s in sessions.values_mut() {
                for field in [&mut s.title, &mut s.task] {
                    if let Some(v) = field {
                        if let Some(tr) = self.translator.lookup(v) {
                            *v = tr;
                            changed = true;
                        }
                    }
                }
            }
        }
        if changed {
            self.push();
        }
    }

    /* ================= уведомления (собственные тосты) ================= */

    /// Тост снизу экрана: приходит всегда (не зависит от разрешений macOS и
    /// Focus-режимов), кликом открывает чат сессии. Возвращает id карточки.
    pub fn notify(&self, title: &str, body: &str, session_id: Option<&str>, kind: &str) -> String {
        let id = format!("t{}", self.toast_seq.fetch_add(1, Ordering::SeqCst) + 1);
        self.notify_id(&id, title, body, session_id, kind);
        id
    }

    /// То же, но со стабильным id: повторное уведомление того же id окно тостов
    /// применяет к существующей карточке, а не плодит новую (дедуп «закончил»
    /// для одной сессии — не было «одно за другим»).
    pub fn notify_id(&self, id: &str, title: &str, body: &str, session_id: Option<&str>, kind: &str) {
        crate::log::line(&format!(
            "[toast:{kind}] id={id} sid={} «{}» / {}",
            session_id.unwrap_or("-"),
            crate::util::ellipsize(title, 60),
            crate::util::ellipsize(body, 90),
        ));
        windows::toast_add(self, id, title, body, session_id, kind);

        // голос (инкремент 7): озвучиваем РЕАЛЬНОЕ уведомление — тот же текст,
        // что в тосте. Одна точка на все события; gated по voice-конфигу.
        let vcfg = crate::voice::config::VoiceConfig::from_settings(&self.settings.load());
        let speak = match kind {
            "done" => vcfg.ev_stop,
            "waiting" => vcfg.ev_notification,
            "limit" => vcfg.ev_stop_failure,
            _ => false,
        };
        if speak {
            self.voice.speak_text(title, body, kind);
        }
    }

    /* ================= busy-флаги фоновых задач ================= */

    fn busy_take(&self, kind: &'static str, sid: &str) -> bool {
        self.busy.lock().unwrap().insert((kind, sid.to_string()))
    }

    fn busy_release(&self, kind: &'static str, sid: &str) {
        self.busy.lock().unwrap().remove(&(kind, sid.to_string()));
    }

    /// Снапшот одной сессии (для эффектов, которым лок не нужен).
    pub fn session(&self, sid: &str) -> Option<Session> {
        self.sessions.lock().unwrap().get(sid).cloned()
    }

    /// Мутация одной сессии под локом; true — сессия существовала.
    pub fn with_session(&self, sid: &str, f: impl FnOnce(&mut Session)) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get_mut(sid) {
            Some(s) => {
                f(s);
                true
            }
            None => false,
        }
    }

    /* ================= редьюсер ================= */

    pub fn reduce(self: &std::sync::Arc<Self>, evt: &Value) {
        let Some(evt_obj) = evt.as_object() else { return };
        let payload = evt.get("payload").and_then(Value::as_object);
        let empty = serde_json::Map::new();
        let p = payload.unwrap_or(&empty);
        let sid = p
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let now = now_ms();
        let event = evt_obj.get("event").and_then(Value::as_str).unwrap_or("");
        // SessionStart несёт source: startup|resume|clear|compact — нас интересует compact.
        let source = p.get("source").and_then(Value::as_str).unwrap_or("");

        // лог жизненного цикла (tool-события не пишем — их сотни)
        if matches!(
            event,
            "session-start" | "prompt" | "notification" | "stop" | "stop-failure" | "session-end"
        ) {
            crate::log::line(&format!("[event] {event} sid={}", ellipsize(&sid, 8)));
        }

        let mut effects: Vec<Effect> = Vec::new();
        {
            let mut sessions = self.sessions.lock().unwrap();

            if event == "session-end" {
                sessions.remove(&sid);
                drop(sessions);
                self.push();
                return;
            }

            // Инвариант «одна пана — одна сессия»: если событие пришло из паны,
            // которую занимала другая (уже мёртвая) сессия, выселяем призрака —
            // иначе ответ ему уйдёт в живую сессию той же паны.
            if let Some(pane) = evt_obj
                .get("tmux_pane")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
            {
                for g in evict_pane(&mut sessions, &sid, pane) {
                    crate::log::line(&format!(
                        "[evict] пана {pane} → sid={}, снят призрак sid={}",
                        ellipsize(&sid, 8),
                        ellipsize(&g, 8)
                    ));
                }
            }

            let s = sessions
                .entry(sid.clone())
                .or_insert_with(|| Session::new(sid.clone(), now));

            /* ---- общие поля события ---- */
            if let Some(cwd) = p.get("cwd").and_then(Value::as_str) {
                s.cwd = Some(cwd.to_string());
                s.project = Some(basename(cwd));
            }
            if s.project.is_none() {
                s.project = Some("?".into());
            }
            if let Some(agent) = evt_obj.get("agent").and_then(Value::as_str) {
                s.agent = Some(agent.to_string());
            }
            if let Some(pane) = evt_obj.get("tmux_pane").and_then(Value::as_str) {
                if !pane.is_empty() && s.tmux_pane.as_deref() != Some(pane) {
                    s.tmux_pane = Some(pane.to_string());
                    effects.push(Effect::ResolveTmuxName { sid: sid.clone(), pane: pane.to_string() });
                }
            }
            if let Some(host) = evt_obj.get("host").and_then(Value::as_str) {
                if !host.is_empty() {
                    s.host = Some(host.to_string());
                }
            }
            if let Some(tty) = evt_obj.get("tty").and_then(Value::as_str) {
                if !tty.is_empty() {
                    s.tty = Some(tty.to_string());
                }
            }
            if let Some(tp) = p.get("transcript_path").and_then(Value::as_str) {
                s.transcript = Some(tp.to_string());
            }
            s.updated_at = now;

            // pid процесса claude (= $PPID хука) → один раз резолвим GUI-приложение
            if let Some(pid) = evt_obj.get("pid").and_then(Value::as_i64) {
                if pid > 0 && s.pid != Some(pid) {
                    s.pid = Some(pid);
                    if s.app.is_none() {
                        effects.push(Effect::ResolveGuiApp { sid: sid.clone(), pid });
                    }
                }
            }

            /* ---- сам переход ---- */
            match event {
                "session-start" => {
                    s.status = Status::Idle;
                    s.detail = String::new();
                    // ручной /compact: SessionStart c source=compact — явно говорим, что сжатие готово
                    if source == "compact" && self.settings.bool("notifyDone") {
                        effects.push(Effect::NotifyCompact {
                            title: format!("{} — компакт завершён", s.project.as_deref().unwrap_or("?")),
                            sid: sid.clone(),
                        });
                    }
                    effects.push(Effect::RefreshMeta { sid: sid.clone() });
                }

                "prompt" => {
                    // подтверждение доставки: реальный хук UserPromptSubmit сработал
                    self.last_prompt_at.lock().unwrap().insert(sid.clone(), now);
                    s.status = Status::Working;
                    let txt = ellipsize(
                        &one_line(p.get("prompt").and_then(Value::as_str).unwrap_or("")),
                        140,
                    );
                    // системные инъекции (<task-notification>, <command-…>) — не промпт юзера
                    if !txt.is_empty() && !txt.starts_with('<') {
                        s.detail = txt.clone();
                        s.last_prompt = Some(txt); // живёт дольше detail
                        // саммари пересчитывается после каждого промта юзера
                        effects.push(Effect::GenSummary { sid: sid.clone() });
                    }
                    s.limit_wait = false; // юзер сам продолжил — авто-резюме не нужно
                    effects.push(Effect::RefreshMeta { sid: sid.clone() });
                    effects.push(Effect::RefreshTasks { sid: sid.clone() });
                }

                // живая лента: что агент делает прямо сейчас
                "pre-tool" => {
                    let tool = p.get("tool_name").and_then(Value::as_str).unwrap_or("");
                    if tool == "AskUserQuestion" {
                        // это опрос, не пермишен: показываем вопрос карточкой и ждём выбора
                        if let Some(q) = build_question(p.get("tool_input"), now) {
                            s.status = Status::Waiting;
                            s.detail = q
                                .questions
                                .first()
                                .map(|x| ellipsize(&x.question, 140))
                                .filter(|t| !t.is_empty())
                                .unwrap_or_else(|| "Опрос".into());
                            s.question = Some(q);
                            effects.push(Effect::NotifyWaiting {
                                title: format!("{} — спрашивает", s.project.as_deref().unwrap_or("?")),
                                body: s.detail.clone(),
                                sid: sid.clone(),
                            });
                        }
                    } else if matches!(tool, "TaskCreate" | "TaskUpdate" | "TaskGet" | "TaskList" | "TodoWrite") {
                        // таск-тулы: обновляем «текущую задачу», в живую ленту их не пишем
                        s.status = Status::Working;
                        if tool == "TodoWrite" {
                            parse_todos(self, s, p.get("tool_input"));
                        }
                        effects.push(Effect::RefreshTasks { sid: sid.clone() });
                    } else if tool == "Task" {
                        // диспатч сабагента: старт реестра (стоп — на post-tool).
                        // Источник истины по задачам — оркестратор, мы лишь читаем.
                        s.status = Status::Working;
                        subagent_start(s, p.get("tool_input"), now);
                        track_activity(s, tool, p.get("tool_input"));
                    } else {
                        s.status = Status::Working;
                        track_activity(s, tool, p.get("tool_input"));
                        if s.branch.is_none() && s.title.is_none() {
                            effects.push(Effect::RefreshMeta { sid: sid.clone() }); // ожила после рестарта демона
                        }
                    }
                }

                "post-tool" => {
                    let tool = p.get("tool_name").and_then(Value::as_str).unwrap_or("");
                    if tool == "AskUserQuestion" && s.question.is_some() {
                        s.question = None; // ответили (в терминале или из панели)
                        s.detail = "ответ получен".into();
                    }
                    if tool == "Task" {
                        // сабагент завершился — закрываем запись в реестре
                        subagent_stop(s, p.get("tool_input"), now);
                    }
                    s.status = Status::Working; // сессия дышит
                }

                "notification" => {
                    // AskUserQuestion дублируется PermissionRequest-уведомлением — вопрос
                    // уже показан карточкой, «Claude needs your permission» его не перетирает
                    let raw = one_line(p.get("message").and_then(Value::as_str).unwrap_or(""));
                    // «ждёт твоего ввода» сразу после ответа — дубль тоста «закончил».
                    // Пока сессия в Done (юзер ещё не реагировал), не пишем второй раз;
                    // статус оставляем Done, чтобы строка списка так и читалась «готово».
                    let redundant_idle =
                        ru::is_idle_input_notification(&raw) && s.status == Status::Done;
                    if s.question.is_none() && !redundant_idle {
                        let msg = ru::ru_notification(&raw);
                        let msg = if msg.is_empty() { "Claude ждёт ввода".to_string() } else { msg };
                        // Claude Code повторяет idle-уведомления — не спамим одним и тем же
                        let is_new = !(s.status == Status::Waiting && s.detail == msg);
                        s.status = Status::Waiting;
                        s.detail = msg.clone();
                        if is_new && self.settings.bool("notifyWaiting") {
                            effects.push(Effect::NotifyWaiting {
                                title: format!("{} — нужен ты", s.project.as_deref().unwrap_or("?")),
                                body: msg,
                                sid: sid.clone(),
                            });
                        }
                    }
                }

                "stop" => {
                    s.status = Status::Done;
                    s.done_at = Some(now); // по нему сортируется список
                    s.detail = "Ответ готов".into();
                    effects.push(Effect::RefreshMeta { sid: sid.clone() });
                    effects.push(Effect::RefreshTasks { sid: sid.clone() });
                    // одна выжимка результата: строка списка + тост (если включён).
                    // GenSummary тут не нужен — DoneSummary даёт ту же строку, но
                    // в стиле «что сделано», как в уведомлении
                    effects.push(Effect::DoneSummary { sid: sid.clone() });
                }

                "stop-failure" => {
                    effects.push(Effect::StopFailure {
                        sid: sid.clone(),
                        payload: Value::Object(p.clone()),
                    });
                }

                _ => {}
            }
        } // лок реестра отпущен

        self.run_effects(effects);
        self.push();
    }

    fn run_effects(self: &std::sync::Arc<Self>, effects: Vec<Effect>) {
        for eff in effects {
            let d = self.clone();
            match eff {
                Effect::ResolveTmuxName { sid, pane } => {
                    tauri::async_runtime::spawn(async move {
                        if let Some(name) = tmux::session_name(&pane).await {
                            if d.with_session(&sid, |s| s.tmux_name = Some(name)) {
                                d.push();
                            }
                        }
                    });
                }
                Effect::ResolveGuiApp { sid, pid } => {
                    tauri::async_runtime::spawn(async move {
                        if let Some(app) = crate::terminal::gui_ancestor_app(pid).await {
                            // сессия могла исчезнуть/смениться — with_session проверит
                            if d.with_session(&sid, |s| {
                                if s.app.is_none() {
                                    s.app = Some(app.name);
                                }
                            }) {
                                d.push();
                            }
                        }
                    });
                }
                Effect::RefreshMeta { sid } => d.refresh_meta(sid),
                Effect::RefreshTasks { sid } => d.refresh_tasks(sid),
                Effect::GenSummary { sid } => d.gen_summary(sid),
                Effect::NotifyWaiting { title, body, sid } => {
                    d.notify(&title, &body, Some(&sid), "waiting");
                }
                Effect::DoneSummary { sid } => d.done_summary(sid),
                Effect::NotifyCompact { title, sid } => {
                    d.notify_id(
                        &format!("compact-{sid}"),
                        &title,
                        "История разговора сжата, контекст освобождён",
                        Some(&sid),
                        "done",
                    );
                }
                Effect::StopFailure { sid, payload } => {
                    // голос «упёрся в лимит» эмитит сам on_stop_failure — только на
                    // подтверждённый лимит, не на транзиентные сбои
                    tauri::async_runtime::spawn(async move {
                        crate::limits::on_stop_failure(&d, &sid, &payload);
                    });
                }
            }
        }
    }

    /// Завершение ответа: ОДНА ИИ-выжимка результата («что по сути сделано»),
    /// используемая И в строке списка, И в тосте «закончил». Раньше это были два
    /// отдельных саммари (gen_summary «над чем работа» + выжимка для тоста) —
    /// строка списка и уведомление расходились. Теперь источник один.
    ///
    /// Текст готовим ДО показа тоста (haiku ~неск. секунд): статус в панели уже
    /// обновился мгновенно через push, а тост-карточку показываем один раз
    /// финальной — без плейсхолдера и подмен (любое изменение уже показанной
    /// карточки читалось как второе уведомление).
    fn done_summary(self: &std::sync::Arc<Self>, sid: String) {
        let d = self.clone();
        tauri::async_runtime::spawn(async move {
            if d.session(&sid).is_none() {
                return;
            }
            let ai = d.ai_toast_summary(&sid).await;

            // строка списка = тот же результат (если выжимка получилась)
            if let Some(text) = ai.as_deref().filter(|t| !t.is_empty()) {
                let list = ellipsize(text, 140);
                if d.with_session(&sid, |s| {
                    s.summary = Some(list);
                    s.summary_at = Some(now_ms());
                }) {
                    d.push();
                }
            }

            if !d.settings.bool("notifyDone") {
                return; // уведомления выключены — строку списка уже обновили
            }
            let Some(s) = d.session(&sid) else { return };
            let non_empty = |v: Option<String>| v.filter(|t| !t.is_empty());
            let body = non_empty(ai)
                .or_else(|| non_empty(s.summary.clone()))
                .or_else(|| non_empty(s.task.clone()))
                .or_else(|| non_empty(s.title.clone()))
                .unwrap_or_else(|| "Ответ готов".into());
            let title = format!("{} — закончил", s.project.as_deref().unwrap_or("?"));
            // стабильный id на сессию: повторный «закончил» переиспользует карточку
            d.notify_id(&format!("done-{sid}"), &title, &body, Some(&sid), "done");
        });
    }

    /// ИИ-выжимка финального ответа агента (haiku) → одно русское предложение.
    /// None — нет claude/квоты, ответ слишком короткий или haiku ушёл в
    /// англоязычный «мета»-ответ (фильтр по кириллице).
    async fn ai_toast_summary(self: &std::sync::Arc<Self>, sid: &str) -> Option<String> {
        if claude_bin::resolve_claude_bin().is_none() || !self.busy_take("aisum", sid) {
            return None;
        }
        let result = async {
            let s = self.session(sid)?;
            let reply = transcript::full_final_reply(s.transcript.as_deref()?)?;
            // длинный ответ режем — haiku отвечает быстрее, а сути хватает
            let reply = ellipsize(&reply, 3000);
            let prompt = format!(
                "Вот ответ агента:\n{reply}\n\nОпиши простыми словами по-русски, ЧТО по сути изменилось и какой результат для пользователя. Только суть и итог, без технических деталей: не упоминай конкретные имена переменных, файлов, функций, команд. Одно короткое предложение, до 160 символов. Только обычный текст, без markdown, без форматирования, без списков."
            );
            let out = claude_bin::run_haiku(&prompt, Duration::from_secs(30)).await?;
            // squeeze_reply страхует от форматирования (markdown/списки/жирный)
            let text = ellipsize(&transcript::squeeze_reply(&out), 200);
            // нет кириллицы → англоязычный «мета»-ответ: отбрасываем
            if text.is_empty() || !ru::has_cyrillic(&text) {
                return None;
            }
            Some(text)
        }
        .await;
        self.busy_release("aisum", sid);
        result
    }

    /* ================= идентичность сессии ================= */

    /// Слой 1+3: ветка и готовый summary из хвоста транскрипта (дебаунс per-сессия).
    pub fn refresh_meta(self: &std::sync::Arc<Self>, sid: String) {
        if !self.busy_take("meta", &sid) {
            return;
        }
        let d = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            d.busy_release("meta", &sid);
            let Some(snap) = d.session(&sid) else { return };
            let Some(tr) = snap.transcript.clone() else { return };

            let entries = transcript::read_recent_entries(std::path::Path::new(&tr), 64 * 1024);

            // ветка лежит в метаданных каждой записи
            let branch = entries.iter().rev().find_map(|e| {
                e.get("gitBranch")
                    .and_then(Value::as_str)
                    .filter(|b| !b.is_empty() && *b != "HEAD")
                    .map(String::from)
            });
            // Claude Code сам пишет заголовок сессии — type:ai-title / summary
            let raw_title = entries.iter().rev().find_map(|e| {
                let t = match e.get("type").and_then(Value::as_str) {
                    Some("ai-title") => e.get("aiTitle"),
                    Some("summary") => e.get("summary"),
                    _ => None,
                };
                t.and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(String::from)
            });
            let title = raw_title.map(|t| d.ru(&ellipsize(&one_line(&t), 60)));

            // модель — бесплатно из транскрипта; не перетираем свежий ручной
            // выбор (modelAt) — транскрипт догонит позже
            let model_fresh = snap
                .model_at
                .is_some_and(|at| now_ms() - at <= 30_000);
            let model = if model_fresh {
                None
            } else {
                entries
                    .iter()
                    .rev()
                    .find_map(|e| {
                        (e.get("type").and_then(Value::as_str) == Some("assistant"))
                            .then(|| e.pointer("/message/model").and_then(Value::as_str))
                            .flatten()
                            .map(friendly_model)
                    })
                    .or_else(|| {
                        snap.cwd
                            .as_deref()
                            .and_then(transcript::read_model_from_project)
                    })
            };

            let mut changed = false;
            let mut rename: Option<(String, String)> = None;
            d.with_session(&sid, |s| {
                if let Some(b) = branch {
                    if s.branch.as_deref() != Some(&b) {
                        s.branch = Some(b);
                        changed = true;
                    }
                }
                if let Some(t) = title {
                    if s.title.as_deref() != Some(&t) {
                        s.title = Some(t.clone());
                        changed = true;
                        // обратный канал: терминал подписывает сам себя
                        if let Some(pane) = &s.tmux_pane {
                            let name = ellipsize(&t, 24);
                            if s.renamed_to.as_deref() != Some(&name) {
                                rename = Some((pane.clone(), name));
                            }
                        }
                    }
                }
                if let Some(m) = model {
                    if s.model.as_deref() != Some(&m) {
                        s.model = Some(m);
                        changed = true;
                    }
                }
            });
            if let Some((pane, name)) = rename {
                let d2 = d.clone();
                let sid2 = sid.clone();
                tauri::async_runtime::spawn(async move {
                    if tmux::rename_window(&pane, &name).await.is_ok() {
                        d2.with_session(&sid2, |s| s.renamed_to = Some(name));
                    }
                });
            }
            if changed {
                d.push();
            }
        });
    }

    /// «Чем занята сейчас»: задачи сессии из ~/.claude/tasks/<id>/N.json
    /// (их ведёт сам Claude Code через TaskCreate/TaskUpdate).
    pub fn refresh_tasks(self: &std::sync::Arc<Self>, sid: String) {
        if !self.busy_take("tasks", &sid) {
            return;
        }
        let d = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            d.busy_release("tasks", &sid);
            let dir = claude_dir().join("tasks").join(&sid);
            let Ok(rd) = std::fs::read_dir(&dir) else { return }; // тасками не пользуется
            let mut tasks: Vec<(i64, String, String)> = rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .filter_map(|p| {
                    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
                    let subject = v.get("subject")?.as_str()?.to_string();
                    let id = match v.get("id") {
                        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
                        Some(Value::String(s)) => s.parse().unwrap_or(0),
                        _ => 0,
                    };
                    let status = v
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Some((id, subject, status))
                })
                .collect();
            if tasks.is_empty() {
                return;
            }
            tasks.sort_by_key(|t| t.0);
            let items: Vec<(String, String)> =
                tasks.into_iter().map(|(_, text, status)| (text, status)).collect();
            d.apply_tasks(&sid, items);
        });
    }

    /// Общий финал refresh_tasks / parse_todos: прогресс, текущая задача, тултип.
    fn apply_tasks(self: &std::sync::Arc<Self>, sid: &str, items: Vec<(String, String)>) {
        let total = items.len();
        if total == 0 {
            return;
        }
        let done = items.iter().filter(|(_, st)| st == "completed").count();
        let task = items
            .iter()
            .find(|(_, st)| st == "in_progress")
            .map(|(text, _)| self.ru(&ellipsize(&one_line(text), 100)));
        let progress = format!("{done}/{total}");
        let todo_list: Vec<String> = items
            .iter()
            .take(12)
            .map(|(text, st)| {
                let mark = match st.as_str() {
                    "completed" => '✓',
                    "in_progress" => '▸',
                    _ => '○',
                };
                format!("{mark} {}", ellipsize(&one_line(text), 70))
            })
            .collect();
        let board_items: Vec<(String, String, Option<String>)> =
            items.into_iter().map(|(text, st)| (text, st, None)).collect();
        let now = now_ms();
        let mut changed = false;
        self.with_session(sid, |s| {
            if s.task != task || s.task_progress.as_deref() != Some(&progress) {
                s.task = task;
                s.task_progress = Some(progress);
                changed = true;
            }
            s.todo_list = Some(todo_list);
            set_board(s, &board_items, now); // структурная доска из файловых тасков
        });
        if changed {
            self.push();
        }
    }

    /// Саммаризация последних задач сессии (haiku) — живой контекст строки
    /// списка. Пересчитывается после каждого промта юзера и каждого ответа.
    pub fn gen_summary(self: &std::sync::Arc<Self>, sid: String) {
        let d = self.clone();
        tauri::async_runtime::spawn(async move {
            // пересчёт после каждого промта и каждого ответа: кулдауна нет,
            // только пауза, чтобы транскрипт успел дописаться на диск
            tokio::time::sleep(Duration::from_millis(1200)).await;
            let Some(s) = d.session(&sid) else { return };
            let Some(tr) = s.transcript.clone() else { return };
            if claude_bin::resolve_claude_bin().is_none() || !d.busy_take("summary", &sid) {
                return;
            }

            let turns: Vec<transcript::ChatItem> = transcript::chain_from_entries(
                transcript::read_recent_entries(std::path::Path::new(&tr), 512 * 1024),
            )
            .iter()
            .flat_map(transcript::to_chat_items)
            .filter(|i| i.kind == "text")
            .collect();
            let turns: Vec<_> = turns.into_iter().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect();
            if turns.len() < 2 {
                d.busy_release("summary", &sid);
                return;
            }
            let convo = turns
                .iter()
                .map(|i| {
                    format!(
                        "{}: {}",
                        if i.role == "user" { "Юзер" } else { "Агент" },
                        ellipsize(&i.text, 240)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let prompt = format!(
                "Вот хвост рабочего диалога:\n{convo}\n\nНапиши простыми словами по-русски одной короткой строкой, над чем сейчас идёт работа. Только обычный текст: без кавычек, без markdown, без форматирования. Не длиннее 90 символов. Названия кода, файлов и команд оставляй как есть."
            );
            let out = claude_bin::run_haiku(&prompt, Duration::from_secs(90)).await;
            d.busy_release("summary", &sid);
            let Some(out) = out else { return }; // без квоты/сети живём на lastPrompt/title
            // squeeze_reply страхует от форматирования, если модель его всё же выдала
            let clean = transcript::squeeze_reply(&out);
            let t = ellipsize(
                clean.trim_matches(|c| c == '"' || c == '«' || c == '»'),
                110,
            );
            // пустой ответ или англоязычный «мета»-ответ haiku — оставляем как есть
            if t.is_empty() || !ru::has_cyrillic(&t) {
                return;
            }
            if d.with_session(&sid, |s| {
                s.summary = Some(t);
                s.summary_at = Some(now_ms());
            }) {
                d.push();
            }
        });
    }

    /// Отметить «промпт ушёл в сессию» (ответ из панели / авто-резюме).
    /// Ждём подтверждения доставки ответа: реальный prompt-хук этой сессии,
    /// случившийся ПОСЛЕ `after`. true — текст дошёл до агента и тот его принял.
    pub async fn await_prompt_ack(&self, sid: &str, after: i64, timeout: Duration) -> bool {
        let steps = (timeout.as_millis() / 100).max(1);
        for _ in 0..steps {
            let acked = self
                .last_prompt_at
                .lock()
                .unwrap()
                .get(sid)
                .copied()
                .unwrap_or(0)
                > after;
            if acked {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.last_prompt_at.lock().unwrap().get(sid).copied().unwrap_or(0) > after
    }

    pub fn mark_prompt_sent(self: &std::sync::Arc<Self>, sid: &str, prompt: &str) {
        self.with_session(sid, |s| {
            s.status = Status::Working;
            s.detail = ellipsize(&one_line(prompt), 140);
            s.updated_at = now_ms();
        });
        self.push();
    }

    /* ================= чистка умерших сессий ================= */
    /* Жёстко убитый терминал не шлёт SessionEnd. Раз в 30с сверяем tmux-паны;
     * working-сессии без событий 15 минут считаем потерянными. */

    pub async fn sweep_sessions(self: &std::sync::Arc<Self>) {
        let alive = match tmux::list_panes().await {
            Ok(set) => set,                                   // None — tmux не установлен
            Err(()) => Some(std::collections::HashSet::new()), // ошибка = сервер пуст
        };
        let mut changed = false;
        {
            let mut sessions = self.sessions.lock().unwrap();
            let now = now_ms();
            sessions.retain(|_, s| {
                if let (Some(pane), Some(alive)) = (&s.tmux_pane, &alive) {
                    if !alive.contains(pane) {
                        changed = true;
                        // Сессия с доской не исчезает молча: замораживаем доску
                        // (in_progress → interrupted) и помечаем остановленной —
                        // план не должен врать. Остальных призраков выселяем.
                        if s.board.as_ref().is_some_and(|b| !b.tasks.is_empty()) {
                            freeze_board(s);
                            s.status = Status::Done;
                            s.detail = "сессия остановлена".into();
                            return true;
                        }
                        return false; // пана мертва — claude умер вместе с ней
                    }
                }
                true
            });
            for s in sessions.values_mut() {
                if s.status == Status::Working && now - s.updated_at > 15 * 60 * 1000 {
                    s.status = Status::Idle;
                    s.detail = "связь потеряна — событий нет 15 минут".into();
                    freeze_board(s); // оборванная связь — задачи «в работе» прерваны
                    changed = true;
                }
            }
        }
        if changed {
            self.push();
        }
    }

    /* ================= диагностика / метрики ================= */
    /* Режим логов из настроек: периодически пишем в jarvis.log RAM/CPU демона и
     * Silero-сайдкара + счётчики, чтобы потом собрать и разобрать. */

    pub async fn sample_metrics(self: &std::sync::Arc<Self>) {
        if !self.settings.bool("diagnostics") {
            return;
        }
        let mut parts: Vec<String> = Vec::new();
        if let Some((rss, cpu)) = ps_metrics(std::process::id()).await {
            parts.push(format!("демон rss={rss:.0}МБ cpu={cpu}%"));
        }
        if let Some(pid) = self.voice.sidecar_pid() {
            if let Some((rss, cpu)) = ps_metrics(pid).await {
                parts.push(format!("silero rss={rss:.0}МБ cpu={cpu}%"));
            }
        }
        let sessions = self.sessions.lock().unwrap().len();
        parts.push(format!("сессий={sessions}"));
        parts.push(format!(
            "голос={} очередь={} mute={}",
            self.voice.engine_name(),
            self.voice.queue_len(),
            self.voice.is_muted()
        ));
        crate::log::line(&format!("[metrics] {}", parts.join(" · ")));
    }

    /* ================= effort-уровни из CLI ================= */
    /* Берём из `claude --help`, чтобы не отставать от релизов. */

    pub async fn detect_effort_levels(self: &std::sync::Arc<Self>) {
        let Some(out) = claude_bin::run_claude(&["--help"], Duration::from_secs(20)).await else {
            return;
        };
        let re = regex::RegexBuilder::new(r"--effort <level>[^(]*\(([^)]+)\)")
            .dot_matches_new_line(true)
            .build()
            .unwrap();
        let Some(c) = re.captures(&out) else { return };
        let word = regex::Regex::new(r"^[a-z]+$").unwrap();
        let levels: Vec<String> = c[1]
            .split(',')
            .map(str::trim)
            .filter(|x| word.is_match(x))
            .map(String::from)
            .collect();
        if levels.len() >= 3 {
            println!("[jarvis] effort-уровни из CLI: {}", levels.join(", "));
            *self.effort_levels.lock().unwrap() = levels;
        }
    }
}

/* ================= чистые помощники редьюсера ================= */

/// Карточка вопроса из tool_input AskUserQuestion (лимиты — как в панели).
fn build_question(input: Option<&Value>, now: i64) -> Option<Question> {
    let qs = input?.get("questions")?.as_array()?;
    if qs.is_empty() {
        return None;
    }
    let questions: Vec<QuestionItem> = qs
        .iter()
        .take(4)
        .map(|q| QuestionItem {
            question: ellipsize(&one_line(q.get("question").and_then(Value::as_str).unwrap_or("")), 300),
            header: ellipsize(&one_line(q.get("header").and_then(Value::as_str).unwrap_or("")), 40),
            multi_select: q.get("multiSelect").and_then(Value::as_bool).unwrap_or(false),
            options: q
                .get("options")
                .and_then(Value::as_array)
                .map(|opts| {
                    opts.iter()
                        .take(9)
                        .map(|o| QuestionOption {
                            label: ellipsize(&one_line(o.get("label").and_then(Value::as_str).unwrap_or("")), 80),
                            description: ellipsize(
                                &one_line(o.get("description").and_then(Value::as_str).unwrap_or("")),
                                140,
                            ),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();
    Some(Question { at: now, from_screen: false, questions })
}

/// Слой 2: последняя команда + тронутые директории из tool-событий.
fn track_activity(s: &mut Session, name: &str, input: Option<&Value>) {
    let label = transcript::short_tool_label(name, input);
    let mut is_command = false;
    if let Some(Value::Object(input)) = input {
        if let Some(cmd) = input.get("command").and_then(Value::as_str) {
            is_command = true;
            s.last_cmd = Some(ellipsize(&one_line(cmd), 48));
        } else if let Some(fp) = input.get("file_path").and_then(Value::as_str) {
            let rel = match &s.cwd {
                Some(cwd) if fp.starts_with(cwd.as_str()) => {
                    fp[cwd.len()..].trim_start_matches('/').to_string()
                }
                _ => fp.to_string(),
            };
            let dir = std::path::Path::new(&rel)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let spot = if dir.is_empty() || dir == "." {
                basename(&rel)
            } else {
                format!("{dir}/")
            };
            let mut touched = s.touched.take().unwrap_or_default();
            touched.retain(|d| d != &spot);
            touched.push(spot);
            while touched.len() > 3 {
                touched.remove(0);
            }
            s.touched = Some(touched);
        }
    }
    let touched_suffix = match &s.touched {
        Some(t) if !t.is_empty() => format!(" · трогает {}", t.join(" ")),
        _ => String::new(),
    };
    if is_command {
        s.detail = ellipsize(
            &format!("▸ {}{}", s.last_cmd.as_deref().unwrap_or(""), touched_suffix),
            140,
        );
    } else if !label.is_empty() && label != "tool" {
        s.detail = ellipsize(&format!("▸ {label}"), 140);
    }
}

/// Fallback для агентов на старом TodoWrite: todos прямо в payload хука
/// (бывает и массивом, и JSON-строкой — парсим defensively).
fn parse_todos(d: &std::sync::Arc<Daemon>, s: &mut Session, input: Option<&Value>) {
    let todos_raw = input.and_then(|i| i.get("todos")).cloned();
    let had_payload = todos_raw.is_some();
    let todos = match todos_raw {
        Some(Value::String(raw)) => serde_json::from_str::<Value>(&raw).ok(),
        other => other,
    };
    let Some(Value::Array(todos)) = todos else {
        // payload был, но не распарсился в массив — schema drift. Не падаем,
        // доску не трогаем (degraded), но причину пишем в лог (сценарий 9).
        if had_payload {
            crate::log::line("[board] TodoWrite: неизвестная форма tool_input.todos — доска не обновлена");
        }
        return;
    };
    if todos.is_empty() {
        return; // пустой список — легитимная «очистка», не дрейф схемы
    }
    // text доски — content (стабильный заголовок); activeForm держим отдельно
    // как живую форму («Exploring …») для строки активности in-progress задачи.
    let items: Vec<(String, String, Option<String>)> = todos
        .iter()
        .filter_map(|t| t.as_object())
        .map(|t| {
            let content = t.get("content").and_then(Value::as_str).unwrap_or("").trim().to_string();
            let active = t
                .get("activeForm")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(String::from);
            let status = t
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending")
                .to_string();
            let text = if content.is_empty() { active.clone().unwrap_or_default() } else { content };
            (text, status, active)
        })
        .filter(|(text, _, _)| !text.is_empty())
        .collect();
    let total = items.len();
    if total == 0 {
        return; // неизвестная форма payload — доску не трогаем, не падаем (degraded)
    }
    // структурная доска (last-write-wins, длительности переносятся по тексту)
    set_board(s, &items, now_ms());

    // компактные поля строки/тултипа (как раньше): для показа activeForm || content
    let disp = |text: &str, af: &Option<String>| af.as_deref().unwrap_or(text).to_string();
    let done = items.iter().filter(|(_, st, _)| st == "completed").count();
    // d.ru запускает насос переводов (ru() лишь спавнит таймер — под локом ок)
    let task = items
        .iter()
        .find(|(_, st, _)| st == "in_progress")
        .map(|(text, _, af)| d.ru(&ellipsize(&one_line(&disp(text, af)), 100)));
    s.todo_list = Some(
        items
            .iter()
            .take(12)
            .map(|(text, st, af)| {
                let mark = match st.as_str() {
                    "completed" => '✓',
                    "in_progress" => '▸',
                    _ => '○',
                };
                format!("{mark} {}", ellipsize(&one_line(&disp(text, af)), 70))
            })
            .collect(),
    );
    s.task = task;
    s.task_progress = Some(format!("{done}/{total}"));
}

/* ================= движок доски задач (инкремент 6) ================= */
/* Источник истины — оркестратор сессии. Эти функции только ЧИТАЮТ события и
 * собирают структуру; ни одна не мутирует план агента. */

/// «Task 5», «task#6», «TASK 12» → номер задачи. Иначе None (не привязываем).
fn task_ref_from(name: &str) -> Option<i64> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)\btask\s*#?\s*(\d+)").unwrap());
    re.captures(name)?.get(1)?.as_str().parse().ok()
}

/// Старт сабагента: PreToolUse(Task). Имя — description, иначе subagent_type.
fn subagent_start(s: &mut Session, input: Option<&Value>, now: i64) {
    let Some(inp) = input else { return };
    let name = inp
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .or_else(|| inp.get("subagent_type").and_then(Value::as_str))
        .unwrap_or("сабагент")
        .to_string();
    let kind = inp
        .get("subagent_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(String::from);
    s.subagents.push(Subagent { name, kind, model: None, started_at: now, stopped_at: None, task_ref: None });
    // реестр не растёт бесконечно — держим последние 64
    let n = s.subagents.len();
    if n > 64 {
        s.subagents.drain(0..n - 64);
    }
    recorrelate(s, now);
}

/// Стоп сабагента: PostToolUse(Task). Закрываем последний открытый с таким
/// описанием (а если описания нет — просто последний открытый).
fn subagent_stop(s: &mut Session, input: Option<&Value>, now: i64) {
    let name = input.and_then(|i| i.get("description")).and_then(Value::as_str).map(str::trim);
    let pos = s
        .subagents
        .iter()
        .rposition(|sa| sa.stopped_at.is_none() && name.is_none_or(|n| sa.name == n));
    if let Some(i) = pos {
        s.subagents[i].stopped_at = Some(now);
    }
    recorrelate(s, now);
}

/// Пересобрать доску из её же текущих задач + обновлённого реестра сабагентов
/// (на событие сабагента, без нового TodoWrite). Нет доски — ничего не делаем.
fn recorrelate(s: &mut Session, now: i64) {
    let Some(b) = s.board.as_ref() else { return };
    let items: Vec<(String, String, Option<String>)> = b
        .tasks
        .iter()
        .map(|t| (t.text.clone(), t.status.clone(), t.active_form.clone()))
        .collect();
    set_board(s, &items, now);
}

/// Записать доску из снапшота задач: переносит длительности и коррелирует
/// сабагентов. Единственное место, где `s.board` присваивается из задач.
fn set_board(s: &mut Session, items: &[(String, String, Option<String>)], now: i64) {
    s.board = Some(build_board(s.board.as_ref(), items, &s.subagents, now));
}

/// Чистая сборка доски: last-write-wins по `items`, перенос started_at/dur_ms
/// по совпадению текста, эвристическая привязка сабагентов по «Task N».
fn build_board(
    prev: Option<&TaskBoard>,
    items: &[(String, String, Option<String>)],
    subagents: &[Subagent],
    now: i64,
) -> TaskBoard {
    let mut tasks: Vec<TaskItem> = items
        .iter()
        .enumerate()
        .map(|(i, (text, status, af))| {
            let prev_t = prev.and_then(|pb| pb.tasks.iter().find(|t| &t.text == text));
            let was_ip = prev_t.is_some_and(|t| t.status == "in_progress");
            let mut started_at = None;
            let mut dur_ms = prev_t.and_then(|t| t.dur_ms);
            if status == "in_progress" {
                // переносим момент начала, если задача уже была в работе; иначе now
                started_at = if was_ip { prev_t.and_then(|t| t.started_at) } else { None }.or(Some(now));
            } else if status == "completed" && dur_ms.is_none() {
                if let Some(st) = prev_t.filter(|t| t.status == "in_progress").and_then(|t| t.started_at) {
                    dur_ms = Some((now - st).max(0));
                }
            }
            TaskItem {
                n: (i + 1) as i64,
                text: text.clone(),
                status: status.clone(),
                active_form: af.clone(),
                model: None,
                dur_ms,
                started_at,
            }
        })
        .collect();

    // корреляция: уверенно ссылается на «Task N» в диапазоне → бейдж на задаче;
    // иначе — в отдельную полоску, без выдуманной привязки.
    let mut strip = Vec::new();
    for sa in subagents {
        match task_ref_from(&sa.name).filter(|n| *n >= 1 && (*n as usize) <= tasks.len()) {
            Some(n) => {
                let t = &mut tasks[(n - 1) as usize];
                if t.model.is_none() {
                    t.model = sa.model.clone();
                }
                if t.dur_ms.is_none() {
                    if let Some(stp) = sa.stopped_at {
                        t.dur_ms = Some((stp - sa.started_at).max(0));
                    }
                }
            }
            None => {
                let mut c = sa.clone();
                c.task_ref = None;
                strip.push(c);
            }
        }
    }

    TaskBoard { tasks, subagents: strip, updated_at: now, stopped: false }
}

/// Текст-инструкция оркестратору для действия с доски. НИЧЕГО не отправляет —
/// панель префилит этим composer, пользователь правит и шлёт сам. Все шаблоны
/// (на языке интерфейса) собраны здесь, единым местом. `None` — действие чужое.
pub fn task_action_text(action: &str, n: i64, title: Option<&str>) -> Option<String> {
    let q = title
        .map(|t| one_line(t))
        .filter(|t| !t.is_empty())
        .map(|t| format!(" «{}»", ellipsize(&t, 80)))
        .unwrap_or_default();
    Some(match action {
        "goto" => format!("Перейди к задаче {n}{q} сейчас. Остальную очередь пока не трогай — доделаешь после."),
        "skip" => format!("Пропусти задачу {n}{q} и переходи к следующей по плану. Отметь её пропущенной, если ведёшь чек-лист."),
        "rerun" => format!("Перезапусти задачу {n}{q} заново, с нуля — предыдущий результат считай неактуальным."),
        _ => return None,
    })
}

/// RSS (МБ) и CPU (%) процесса по pid через `ps`. None — процесса нет.
async fn ps_metrics(pid: u32) -> Option<(f64, f64)> {
    let out = tokio::process::Command::new("ps")
        .args(["-o", "rss=,pcpu=", "-p", &pid.to_string()])
        .output()
        .await
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut it = s.split_whitespace();
    let rss = it.next()?.parse::<f64>().ok()? / 1024.0;
    let cpu = it.next()?.parse::<f64>().ok()?;
    Some((rss, cpu))
}

/// Сессия умерла: доска заморожена, задачи «в работе» → прерванные.
fn freeze_board(s: &mut Session) {
    if let Some(b) = s.board.as_mut() {
        b.stopped = true;
        for t in b.tasks.iter_mut() {
            if t.status == "in_progress" {
                t.status = "interrupted".into();
            }
        }
    }
}


/// Инвариант «одна tmux-пана — одна сессия».
///
/// Когда событие из паны `pane` приходит для `keep_sid`, любая ДРУГАЯ сессия,
/// всё ещё числящаяся на этой пане, — призрак: её claude завершился без
/// `session-end` и был заменён новым в той же пане. Снимаем призраков, иначе
/// ответ, адресованный призраку, уйдёт в живую сессию той же паны (мисроутинг).
/// Возвращает id выселенных сессий — для лога и обновления UI.
fn evict_pane(sessions: &mut HashMap<String, Session>, keep_sid: &str, pane: &str) -> Vec<String> {
    let ghosts: Vec<String> = sessions
        .iter()
        .filter_map(|(id, s)| {
            (id.as_str() != keep_sid && s.tmux_pane.as_deref() == Some(pane)).then(|| id.clone())
        })
        .collect();
    for g in &ghosts {
        sessions.remove(g);
    }
    ghosts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess(id: &str, pane: Option<&str>) -> Session {
        let mut s = Session::new(id.to_string(), 0);
        s.tmux_pane = pane.map(str::to_string);
        s
    }

    #[test]
    fn evict_pane_drops_ghost_sharing_pane() {
        let mut m = HashMap::new();
        m.insert("ghost".into(), sess("ghost", Some("%1"))); // старый claude в %1
        m.insert("live".into(), sess("live", Some("%1"))); // новый claude в той же %1
        m.insert("other".into(), sess("other", Some("%2"))); // другая пана — не трогать

        let evicted = evict_pane(&mut m, "live", "%1");

        assert_eq!(evicted, vec!["ghost".to_string()]);
        assert!(!m.contains_key("ghost"), "призрак должен быть снят");
        assert!(m.contains_key("live"), "живую сессию не трогаем");
        assert!(m.contains_key("other"), "сессию на другой пане не трогаем");
    }

    #[test]
    fn evict_pane_keeps_parallel_sessions_on_distinct_panes() {
        let mut m = HashMap::new();
        m.insert("a".into(), sess("a", Some("%0")));
        m.insert("b".into(), sess("b", Some("%1")));

        // событие для b из её собственной паны — никого не выселяем
        let evicted = evict_pane(&mut m, "b", "%1");

        assert!(evicted.is_empty());
        assert_eq!(m.len(), 2, "две параллельные сессии в разных панах живут обе");
    }

    /* ----- доска задач (инкремент 6) ----- */

    fn it(text: &str, status: &str) -> (String, String, Option<String>) {
        (text.to_string(), status.to_string(), None)
    }

    fn agg(b: &TaskBoard) -> (usize, usize, usize, usize) {
        let total = b.tasks.len();
        let done = b.tasks.iter().filter(|t| t.status == "completed").count();
        let run = b.tasks.iter().filter(|t| t.status == "in_progress").count();
        let queued = b.tasks.iter().filter(|t| t.status == "pending").count();
        (total, done, run, queued)
    }

    #[test]
    fn board_aggregates_and_numbers_tasks() {
        let items = [
            it("Скелет", "completed"),
            it("Модель", "completed"),
            it("Контроллеры", "in_progress"),
            it("CI", "pending"),
        ];
        let b = build_board(None, &items, &[], 1000);
        assert_eq!(agg(&b), (4, 2, 1, 1));
        assert_eq!(b.tasks[2].n, 3, "номера 1-based позиционные");
        assert_eq!(b.tasks[2].status, "in_progress");
        assert_eq!(b.tasks[2].started_at, Some(1000), "in_progress получил старт");
    }

    #[test]
    fn board_last_write_wins_replaces_whole_list() {
        let first = [it("A", "completed"), it("B", "in_progress")];
        let b1 = build_board(None, &first, &[], 0);
        // агент переписал план целиком: добавил Task 3, B завершил
        let second = [it("A", "completed"), it("B", "completed"), it("C", "pending")];
        let b2 = build_board(Some(&b1), &second, &[], 5000);
        assert_eq!(b2.tasks.len(), 3, "ровно новый список, без осколков старого");
        assert_eq!(agg(&b2), (3, 2, 0, 1));
        // B был in_progress с t=0 → completed на t=5000: длительность зафиксирована
        assert_eq!(b2.tasks[1].dur_ms, Some(5000));
    }

    #[test]
    fn board_carries_started_at_across_snapshots() {
        let s1 = [it("A", "in_progress")];
        let b1 = build_board(None, &s1, &[], 1000);
        // повторный снапшот без смены статуса — старт не сбрасывается на now
        let b2 = build_board(Some(&b1), &s1, &[], 9000);
        assert_eq!(b2.tasks[0].started_at, Some(1000));
    }

    #[test]
    fn subagent_correlates_by_task_number_else_strip() {
        let subs = vec![
            Subagent { name: "Implement Task 3: controllers".into(), kind: Some("general-purpose".into()), model: Some("sonnet".into()), started_at: 0, stopped_at: Some(2000), task_ref: None },
            Subagent { name: "code-reviewer".into(), kind: Some("code-reviewer".into()), model: Some("opus".into()), started_at: 0, stopped_at: Some(3000), task_ref: None },
        ];
        let items = [it("Скелет", "completed"), it("Модель", "completed"), it("Контроллеры", "in_progress")];
        let b = build_board(None, &items, &subs, 0);
        // "Task 3" → бейдж на третьей задаче
        assert_eq!(b.tasks[2].model.as_deref(), Some("sonnet"));
        assert_eq!(b.tasks[2].dur_ms, Some(2000));
        // "code-reviewer" без номера → полоска, НЕ привязан наугад
        assert_eq!(b.subagents.len(), 1, "несопоставленный сабагент уходит в полоску");
        assert_eq!(b.subagents[0].name, "code-reviewer");
        assert!(b.tasks.iter().all(|t| t.model.as_deref() != Some("opus")), "opus никуда не прилеплен");
    }

    #[test]
    fn subagent_out_of_range_number_not_correlated() {
        let subs = vec![Subagent { name: "Task 9 — что-то".into(), model: Some("sonnet".into()), started_at: 0, stopped_at: Some(10), ..Default::default() }];
        let items = [it("A", "completed"), it("B", "in_progress")]; // только 2 задачи
        let b = build_board(None, &items, &subs, 0);
        assert_eq!(b.subagents.len(), 1, "номер вне диапазона → не привязан");
        assert!(b.tasks.iter().all(|t| t.model.is_none()));
    }

    #[test]
    fn freeze_marks_in_progress_interrupted() {
        let items = [it("A", "completed"), it("B", "in_progress"), it("C", "pending")];
        let mut s = Session::new("x".into(), 0);
        s.board = Some(build_board(None, &items, &[], 0));
        freeze_board(&mut s);
        let b = s.board.unwrap();
        assert!(b.stopped);
        assert_eq!(b.tasks[1].status, "interrupted", "была в работе → прервана");
        assert_eq!(b.tasks[0].status, "completed", "готовую не трогаем");
        assert_eq!(b.tasks[2].status, "pending", "очередь не трогаем");
    }

    #[test]
    fn task_ref_parsing() {
        assert_eq!(task_ref_from("Implement Task 5: docker"), Some(5));
        assert_eq!(task_ref_from("TASK#12 review"), Some(12));
        assert_eq!(task_ref_from("code-reviewer"), None);
        assert_eq!(task_ref_from("multitask runner"), None, "не ловим внутри слова");
    }

    #[test]
    fn task_action_text_generates_and_rejects() {
        let goto = task_action_text("goto", 5, Some("docker-compose · .env")).unwrap();
        assert!(goto.contains("задаче 5") && goto.contains("docker-compose"));
        assert!(task_action_text("skip", 6, None).unwrap().contains("Пропусти задачу 6"));
        assert!(task_action_text("rerun", 2, None).unwrap().contains("Перезапусти задачу 2"));
        assert!(task_action_text("blow-up", 1, None).is_none(), "чужое действие → None, ничего не шлём");
    }
}
