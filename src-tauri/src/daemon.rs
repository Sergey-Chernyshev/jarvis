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

use crate::model::{Question, QuestionItem, QuestionOption, Session, Status};
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
}

/// Побочные эффекты редьюсера — исполняются после освобождения лока реестра.
enum Effect {
    ResolveTmuxName { sid: String, pane: String },
    ResolveGuiApp { sid: String, pid: i64 },
    RefreshMeta { sid: String },
    RefreshTasks { sid: String },
    GenSummary { sid: String },
    /// Уведомление «спрашивает» (карточка вопроса).
    NotifyWaiting { title: String, body: String, sid: String },
    /// Уведомление «закончил» + ИИ-выжимка полного ответа вдогонку.
    NotifyDone { sid: String },
    StopFailure { sid: String, payload: Value },
}

impl Daemon {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            sessions: Mutex::new(HashMap::new()),
            settings: settings::Store::new(),
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
        windows::toast_add(self, &id, title, body, session_id, kind);
        id
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

        let mut effects: Vec<Effect> = Vec::new();
        {
            let mut sessions = self.sessions.lock().unwrap();

            if event == "session-end" {
                sessions.remove(&sid);
                drop(sessions);
                self.push();
                return;
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
                    effects.push(Effect::RefreshMeta { sid: sid.clone() });
                }

                "prompt" => {
                    s.status = Status::Working;
                    let txt = ellipsize(
                        &one_line(p.get("prompt").and_then(Value::as_str).unwrap_or("")),
                        140,
                    );
                    // системные инъекции (<task-notification>, <command-…>) — не промпт юзера
                    if !txt.is_empty() && !txt.starts_with('<') {
                        s.detail = txt.clone();
                        s.last_prompt = Some(txt); // живёт дольше detail
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
                    } else {
                        s.status = Status::Working;
                        track_activity(s, tool, p.get("tool_input"));
                        if s.branch.is_none() && s.title.is_none() {
                            effects.push(Effect::RefreshMeta { sid: sid.clone() }); // ожила после рестарта демона
                        }
                    }
                }

                "post-tool" => {
                    if p.get("tool_name").and_then(Value::as_str) == Some("AskUserQuestion")
                        && s.question.is_some()
                    {
                        s.question = None; // ответили (в терминале или из панели)
                        s.detail = "ответ получен".into();
                    }
                    s.status = Status::Working; // сессия дышит
                }

                "notification" => {
                    // AskUserQuestion дублируется PermissionRequest-уведомлением — вопрос
                    // уже показан карточкой, «Claude needs your permission» его не перетирает
                    if s.question.is_none() {
                        let msg = ru::ru_notification(&one_line(
                            p.get("message").and_then(Value::as_str).unwrap_or(""),
                        ));
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
                    if self.settings.bool("notifyDone") {
                        effects.push(Effect::NotifyDone { sid: sid.clone() });
                    }
                    effects.push(Effect::RefreshMeta { sid: sid.clone() });
                    effects.push(Effect::RefreshTasks { sid: sid.clone() });
                    effects.push(Effect::GenSummary { sid: sid.clone() });
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
                Effect::NotifyDone { sid } => d.notify_done(sid),
                Effect::StopFailure { sid, payload } => {
                    tauri::async_runtime::spawn(async move {
                        crate::limits::on_stop_failure(&d, &sid, &payload);
                    });
                }
            }
        }
    }

    /// Тост «закончил» сразу с черновым текстом, затем ИИ-выжимка полного
    /// ответа (haiku) обновляет карточку.
    fn notify_done(self: &std::sync::Arc<Self>, sid: String) {
        let d = self.clone();
        tauri::async_runtime::spawn(async move {
            let Some(s) = d.session(&sid) else { return };
            let reply = s
                .transcript
                .as_deref()
                .and_then(transcript::last_assistant_reply);
            // как JS `a || b || c`: пустая строка падает на следующий фолбэк
            let non_empty = |v: Option<String>| v.filter(|t| !t.is_empty());
            let body = non_empty(reply)
                .or_else(|| non_empty(s.task.clone()))
                .or_else(|| non_empty(s.summary.clone()))
                .or_else(|| non_empty(s.title.clone()))
                .unwrap_or_else(|| "Ответ готов".into());
            let title = format!("{} — закончил", s.project.as_deref().unwrap_or("?"));
            let toast_id = d.notify(&title, &body, Some(&sid), "done");
            d.ai_toast_summary(sid, toast_id).await;
        });
    }

    /// ИИ-выжимка финального ответа для тоста: haiku, ~4 строки.
    async fn ai_toast_summary(self: &std::sync::Arc<Self>, sid: String, toast_id: String) {
        if claude_bin::resolve_claude_bin().is_none() || !self.busy_take("aisum", &sid) {
            return;
        }
        let result = async {
            let s = self.session(&sid)?;
            let reply = transcript::full_final_reply(s.transcript.as_deref()?)?;
            if reply.chars().count() < 80 {
                return None; // короткий ответ и так влез целиком
            }
            let prompt = format!(
                "Ответ агента:\n{reply}\n\nЗадача: сожми этот ответ в выжимку до 280 символов по-русски — что сделано и каков итог. Без вступлений, без markdown, технические термины не переводи. Только текст выжимки."
            );
            let out = claude_bin::run_haiku(&prompt, Duration::from_secs(60)).await?;
            let text = ellipsize(&one_line(&out), 320);
            if text.is_empty() {
                return None;
            }
            Some(text)
        }
        .await;
        self.busy_release("aisum", &sid);
        if let Some(text) = result {
            println!("[jarvis] ai-toast: {}…", ellipsize(&text, 80));
            windows::toast_update(self, &toast_id, &text);
        }
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
        let mut changed = false;
        self.with_session(sid, |s| {
            if s.task != task || s.task_progress.as_deref() != Some(&progress) {
                s.task = task;
                s.task_progress = Some(progress);
                changed = true;
            }
            s.todo_list = Some(todo_list);
        });
        if changed {
            self.push();
        }
    }

    /// Саммаризация последних задач сессии (haiku) — живой контекст строки
    /// списка. Обновляется на stop с кулдауном 2 мин.
    pub fn gen_summary(self: &std::sync::Arc<Self>, sid: String) {
        let d = self.clone();
        tauri::async_runtime::spawn(async move {
            let Some(s) = d.session(&sid) else { return };
            if s.summary_at.is_some_and(|at| now_ms() - at < 2 * 60 * 1000) {
                return;
            }
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
                "Хвост диалога рабочей сессии:\n{convo}\n\nЗадача: одной строкой по-русски (до 90 символов) суммаризируй последние задачи — что просил юзер и что сделал агент. Без кавычек, без вступлений, технические термины не переводи."
            );
            let out = claude_bin::run_haiku(&prompt, Duration::from_secs(90)).await;
            d.busy_release("summary", &sid);
            let Some(out) = out else { return }; // без квоты/сети живём на lastPrompt/title
            let t = ellipsize(
                one_line(&out).trim_matches(|c| c == '"' || c == '«' || c == '»'),
                110,
            );
            if t.is_empty() {
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
                        return false; // пана мертва — claude умер вместе с ней
                    }
                }
                true
            });
            for s in sessions.values_mut() {
                if s.status == Status::Working && now - s.updated_at > 15 * 60 * 1000 {
                    s.status = Status::Idle;
                    s.detail = "связь потеряна — событий нет 15 минут".into();
                    changed = true;
                }
            }
        }
        if changed {
            self.push();
        }
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
    let todos = match todos_raw {
        Some(Value::String(raw)) => serde_json::from_str::<Value>(&raw).ok(),
        other => other,
    };
    let Some(Value::Array(todos)) = todos else { return };
    if todos.is_empty() {
        return;
    }
    let items: Vec<(String, String)> = todos
        .iter()
        .filter_map(|t| t.as_object())
        .map(|t| {
            // как JS `activeForm || content`: пустой activeForm падает на content
            let text = ["activeForm", "content"]
                .iter()
                .find_map(|k| t.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()))
                .unwrap_or("")
                .to_string();
            let status = t.get("status").and_then(Value::as_str).unwrap_or("").to_string();
            (text, status)
        })
        .collect();
    // apply_tasks хочет &Arc<Daemon> и лок свободным — а мы под локом редьюсера.
    // Дублируем его логику инлайном на уже захваченной сессии.
    let total = items.len();
    if total == 0 {
        return;
    }
    let done = items.iter().filter(|(_, st)| st == "completed").count();
    // d.ru, не translator.ru: запускает насос переводов (ru() лишь спавнит
    // таймер — под локом редьюсера это безопасно)
    let task = items
        .iter()
        .find(|(_, st)| st == "in_progress")
        .map(|(text, _)| d.ru(&ellipsize(&one_line(text), 100)));
    let progress = format!("{done}/{total}");
    s.todo_list = Some(
        items
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
            .collect(),
    );
    s.task = task;
    s.task_progress = Some(progress);
}
