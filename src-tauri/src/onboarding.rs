//! Readiness-driven onboarding and the process-wide install job.
//!
//! Rust owns the durable-in-process snapshot. Webviews treat events as a signal
//! to re-render this source of truth, so reopening onboarding cannot lose
//! progress or accidentally start a duplicate download.

use crate::install::{self, Artifact, Status, Step};
use serde::Serialize;
use serde_json::{Map, Value};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallJobState {
    #[default]
    Idle,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepView {
    pub scope: String,
    pub phase: String,
    pub state: String,
    pub msg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct: Option<u8>,
}

impl StepView {
    #[cfg(test)]
    fn new(phase: impl Into<String>, state: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            scope: String::new(),
            phase: phase.into(),
            state: state.into(),
            msg: msg.into(),
            pct: None,
        }
    }

    fn from_step(scope: &str, step: &Step) -> Self {
        let state = serde_json::to_value(step.state)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "info".into());
        Self {
            scope: scope.into(),
            phase: step.phase.clone(),
            state,
            msg: step.msg.clone(),
            pct: step.pct,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallJobSnapshot {
    pub id: u64,
    pub kind: String,
    pub state: InstallJobState,
    pub tasks: Vec<String>,
    pub steps: Vec<StepView>,
    pub failures: Vec<String>,
}

#[derive(Debug, Default)]
struct JobMachine {
    next_id: u64,
    snapshot: InstallJobSnapshot,
}

impl JobMachine {
    fn current(&self) -> InstallJobSnapshot {
        self.snapshot.clone()
    }

    fn start(
        &mut self,
        kind: impl Into<String>,
        tasks: Vec<String>,
    ) -> Result<InstallJobSnapshot, InstallJobSnapshot> {
        if self.snapshot.state == InstallJobState::Running {
            return Err(self.current());
        }
        self.next_id = self.next_id.saturating_add(1);
        self.snapshot = InstallJobSnapshot {
            id: self.next_id,
            kind: kind.into(),
            state: InstallJobState::Running,
            tasks,
            steps: Vec::new(),
            failures: Vec::new(),
        };
        Ok(self.current())
    }

    fn record_step(&mut self, scope: &str, mut step: StepView) {
        if self.snapshot.state != InstallJobState::Running {
            return;
        }
        step.scope = scope.into();
        if let Some(existing) = self
            .snapshot
            .steps
            .iter_mut()
            .find(|existing| existing.scope == step.scope && existing.phase == step.phase)
        {
            *existing = step;
        } else {
            self.snapshot.steps.push(step);
        }
    }

    fn finish(&mut self, failures: Vec<String>) {
        if self.snapshot.state != InstallJobState::Running {
            return;
        }
        self.snapshot.state = if failures.is_empty() {
            InstallJobState::Done
        } else {
            InstallJobState::Failed
        };
        self.snapshot.failures = failures;
    }
}

static INSTALL_JOB: OnceLock<Mutex<JobMachine>> = OnceLock::new();

fn install_job() -> &'static Mutex<JobMachine> {
    INSTALL_JOB.get_or_init(|| Mutex::new(JobMachine::default()))
}

fn current_job() -> InstallJobSnapshot {
    install_job()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .current()
}

fn start_job(kind: &str, tasks: Vec<String>) -> Result<InstallJobSnapshot, InstallJobSnapshot> {
    install_job()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .start(kind, tasks)
}

fn update_job_step(scope: &str, step: &Step) -> InstallJobSnapshot {
    let mut jobs = install_job()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    jobs.record_step(scope, StepView::from_step(scope, step));
    jobs.current()
}

fn finish_job(failures: Vec<String>) -> InstallJobSnapshot {
    let mut jobs = install_job()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    jobs.finish(failures);
    jobs.current()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessItem {
    pub id: String,
    pub label: String,
    pub ready: bool,
    pub available: bool,
    pub required: bool,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

impl ReadinessItem {
    fn new(
        id: &str,
        label: &str,
        ready: bool,
        available: bool,
        required: bool,
        detail: &str,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            ready,
            available,
            required,
            detail: detail.into(),
            action: None,
        }
    }

    fn action(mut self, action: &str) -> Self {
        self.action = Some(action.into());
        self
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessSnapshot {
    pub core_ready: bool,
    pub agents: Vec<ReadinessItem>,
    pub transport: Vec<ReadinessItem>,
    pub capabilities: Vec<ReadinessItem>,
    pub warnings: Vec<String>,
    pub proxy_configured: bool,
    pub job: InstallJobSnapshot,
}

fn build_readiness(
    health: install::IntegrationHealth,
    status: Status,
    job: InstallJobSnapshot,
    proxy_configured: bool,
) -> ReadinessSnapshot {
    let core_ready = health.ok();
    let mut warnings = Vec::new();
    if !health.claude_present && !health.codex_present {
        warnings.push("Не найден ни Claude Code, ни Codex CLI.".into());
    }
    if !health.hook_bin {
        warnings.push("Hook binary отсутствует — запусти восстановление интеграции.".into());
    }
    if health.claude_present && !health.claude_hooks_ok {
        warnings.push("Claude Code hooks требуют восстановления.".into());
    }
    if health.codex_present && !health.codex_hooks_ok {
        warnings.push("Codex hooks требуют восстановления или подтверждения доверия.".into());
    }
    if !status.tmux_conf || !status.path_block {
        warnings.push(
            "Удалённое управление терминалом ограничено; мониторинг hooks продолжит работать."
                .into(),
        );
    }

    let agents = vec![
        ReadinessItem::new(
            "claude",
            "Claude Code",
            health.claude_present && health.claude_hooks_ok && health.hook_bin,
            health.claude_present,
            health.claude_present,
            if health.claude_present {
                "События и lifecycle hooks"
            } else {
                "CLI не найден в PATH"
            },
        )
        .action("Установить Claude Code или обновить PATH"),
        ReadinessItem::new(
            "codex",
            "Codex",
            health.codex_present && health.codex_hooks_ok && health.hook_bin,
            health.codex_present,
            health.codex_present,
            if health.codex_present {
                "Hooks без глобального bypass; Codex может запросить доверие"
            } else {
                "CLI не найден в PATH"
            },
        )
        .action("Установить Codex или подтвердить доверие hooks"),
    ];
    let transport = vec![
        ReadinessItem::new(
            "hook",
            "Hook transport",
            health.hook_bin,
            true,
            true,
            "Локальный бинарь событий",
        )
        .action("Восстановить интеграцию"),
        ReadinessItem::new(
            "tmux",
            "Terminal remote",
            status.tmux_conf && status.path_block,
            status.tmux_conf,
            false,
            "Опциональные команды в живую tmux-сессию",
        )
        .action("Установить tmux и повторить настройку"),
        ReadinessItem::new(
            "socket",
            "Runtime socket",
            health.socket,
            true,
            false,
            if health.socket {
                "Демон принимает события"
            } else {
                "Запускается вместе с Jarvis"
            },
        ),
    ];
    let capabilities = vec![
        ReadinessItem::new(
            "whisper-turbo",
            "Whisper",
            status.whisper_model && status.whisper_native_built,
            status.whisper_native_built,
            false,
            "Локальная диктовка, ~574 МБ",
        ),
        ReadinessItem::new(
            "qwen3-runtime",
            "Qwen3-ASR",
            status.qwen3_sidecar
                && (install::qwen_weights_present("qwen3-0.6b")
                    || install::qwen_weights_present("qwen3-1.7b")),
            true,
            false,
            "MLX runtime + проверенный комплект весов",
        ),
        ReadinessItem::new(
            "silero",
            "Silero voice",
            status.silero,
            true,
            false,
            "Локальная озвучка; модель имеет non-commercial ограничения",
        ),
        ReadinessItem::new(
            "hey_jarvis",
            "Wake word",
            status.wakeword_models,
            true,
            false,
            "Опциональная голосовая активация",
        ),
    ];
    ReadinessSnapshot {
        core_ready,
        agents,
        transport,
        capabilities,
        warnings,
        proxy_configured,
        job,
    }
}

fn readiness_snapshot(app: &AppHandle) -> ReadinessSnapshot {
    let proxy_configured = crate::daemon::Daemon::get(app).settings.proxy().is_some();
    build_readiness(
        install::integration_health(),
        install::status(),
        current_job(),
        proxy_configured,
    )
}

fn emit_both(app: &AppHandle, event: &str, payload: Value) {
    let _ = app.emit_to("main", event, payload.clone());
    let _ = app.emit_to("onboarding", event, payload);
}

#[tauri::command]
pub fn onboarding_status() -> Status {
    install::status()
}

#[tauri::command]
pub fn onboarding_get(app: AppHandle) -> ReadinessSnapshot {
    readiness_snapshot(&app)
}

#[tauri::command]
pub fn onboarding_run(app: AppHandle, proxy: Option<String>) -> InstallJobSnapshot {
    let started = match start_job("core", Vec::new()) {
        Ok(started) => started,
        Err(running) => return running,
    };
    let d = crate::daemon::Daemon::get(&app);
    if let Some(proxy) = proxy {
        let mut service = Map::new();
        service.insert("proxy".into(), Value::String(proxy.trim().to_string()));
        d.settings.set_block("service", service);
    }
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            install::install_core(&|step: Step| {
                let snapshot = update_job_step("core", &step);
                let _ = app.emit_to("onboarding", "onboarding:progress", step);
                emit_both(
                    &app,
                    "install_job_changed",
                    serde_json::to_value(snapshot).unwrap_or(Value::Null),
                );
            })
        }));
        let failures = match outcome {
            Ok(health) if health.ok() => Vec::new(),
            Ok(_) => vec!["Core integration не прошла итоговую readiness-проверку".into()],
            Err(_) => vec!["Core installer аварийно остановился; безопасно повтори установку".into()],
        };
        finish_job(failures);
        let readiness = readiness_snapshot(&app);
        let payload = serde_json::to_value(&readiness).unwrap_or(Value::Null);
        emit_both(&app, "install_job_changed", payload.clone());
        let _ = app.emit_to("onboarding", "onboarding:done", payload);
    });
    started
}

/// Открыть окно онбординга (кнопка «Настроить/Переустановить» из настроек).
#[tauri::command]
pub fn onboarding_open(app: AppHandle) {
    let _ = crate::windows::create_onboarding(&app);
}

/// Закрыть окно онбординга (кнопка ×) — надёжно, со стороны Rust.
#[tauri::command]
pub fn onboarding_close(app: AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("onboarding") {
        let _ = w.close();
    }
}

/// Открыть панель и переключить на вкладку настроек (кнопка из онбординга).
#[tauri::command]
pub fn onboarding_open_settings(app: AppHandle) {
    crate::windows::show_panel(&crate::daemon::Daemon::get(&app));
    let _ = app.emit_to("main", "goto-settings", ());
}

/// Открыть основную панель после успешного onboarding, без принудительного
/// перехода в настройки.
#[tauri::command]
pub fn onboarding_open_panel(app: AppHandle) {
    crate::windows::show_panel(&crate::daemon::Daemon::get(&app));
}

/// Полная сводка интеграции для карточки настроек.
#[derive(Serialize)]
pub struct IntegrationInfo {
    status: Status,
    readiness: ReadinessSnapshot,
    foreign_hooks: usize,
    models: Vec<Artifact>,
    quiet: bool,
    proxy_configured: bool,
}

fn integration_info(app: &AppHandle) -> IntegrationInfo {
    let d = crate::daemon::Daemon::get(app);
    IntegrationInfo {
        status: install::status(),
        readiness: readiness_snapshot(app),
        foreign_hooks: install::foreign_hook_count(),
        models: install::model_artifacts(),
        quiet: d.is_quiet(),
        proxy_configured: d.settings.proxy().is_some(),
    }
}

#[tauri::command]
pub fn integration_get(app: AppHandle) -> IntegrationInfo {
    integration_info(&app)
}

/// Умный откат: снять наши хуки/шим/tmux/PATH (чужие хуки и Silero не трогаем).
#[tauri::command]
pub fn integration_remove(app: AppHandle) -> IntegrationInfo {
    install::uninstall(&|_step| {}); // быстрый, без сети/Silero
    integration_info(&app)
}

/// Удалить голосовой артефакт по id и вернуть обновлённую сводку.
#[tauri::command]
pub fn model_delete(app: AppHandle, id: String) -> Result<IntegrationInfo, String> {
    install::delete_model(&id)?;
    Ok(integration_info(&app))
}

/// Включить/выключить тихий режим (разработчик) из настроек.
#[tauri::command]
pub fn quiet_set(app: AppHandle, on: bool) {
    crate::daemon::Daemon::get(&app).set_quiet(on);
}

/// Скачать модель Whisper large-v3-turbo-q5 (~574 МБ) по запросу из настроек.
/// Раньше скачивания не было вообще — теперь панель ПРЕДЛАГАЕТ загрузку (по
/// умолчанию ничего не тянем, как и просил пользователь). Фоном, fail-safe:
/// прогресс → `stt_install_progress`, финал → `stt_install_done` (kind=whisper).
#[tauri::command]
pub fn stt_install_whisper(app: AppHandle) {
    let d = crate::daemon::Daemon::get(&app);
    let proxy = d.settings.proxy();
    std::thread::spawn(move || {
        let r = install::install_whisper(
            &|step: Step| {
                let _ = app.emit_to("main", "stt_install_progress", step);
            },
            proxy.as_deref(),
        );
        let _ = app.emit_to(
            "main",
            "stt_install_done",
            serde_json::json!({
                "kind": "whisper",
                "ok": r.is_ok(),
                "error": r.err(),
                "ready": install::status().whisper_model,
            }),
        );
    });
}

/// Установить Qwen3-ASR MLX-сайдкар (venv + зависимости, ~2.6 ГБ) по запросу из
/// настроек. Сами веса Qwen3 догрузятся сайдкаром при первом запросе. Фоном,
/// fail-safe; прогресс → `stt_install_progress`, финал → `stt_install_done`
/// (kind=qwen3).
#[tauri::command]
pub fn stt_install_sidecar(app: AppHandle) {
    let d = crate::daemon::Daemon::get(&app);
    let proxy = d.settings.proxy();
    std::thread::spawn(move || {
        let r = install::install_stt_sidecar(
            &|step: Step| {
                let _ = app.emit_to("main", "stt_install_progress", step);
            },
            proxy.as_deref(),
        );
        let _ = app.emit_to(
            "main",
            "stt_install_done",
            serde_json::json!({
                "kind": "qwen3",
                "ok": r.is_ok(),
                "error": r.err(),
                "ready": install::status().qwen3_sidecar,
            }),
        );
    });
}

/// Установить Codex-SDK сайдкар (venv + `openai-codex`) — служебный LLM «под
/// капотом» на Codex. Фоном, fail-safe; прогресс → `codex_install_progress`,
/// финал → `codex_install_done`.
#[tauri::command]
pub fn codex_install_sidecar(app: AppHandle) {
    let d = crate::daemon::Daemon::get(&app);
    let proxy = d.settings.proxy();
    std::thread::spawn(move || {
        let r = install::install_codex_sdk_sidecar(
            &|step: Step| {
                let _ = app.emit_to("main", "codex_install_progress", step);
            },
            proxy.as_deref(),
        );
        let _ = app.emit_to(
            "main",
            "codex_install_done",
            serde_json::json!({
                "ok": r.is_ok(),
                "error": r.err(),
                "ready": install::status().codex_sdk_sidecar,
            }),
        );
    });
}

/// Скачать 3 ONNX-модели wake-word (инкр. 10) с прогрессом в панель. Фоном,
/// fail-safe; по завершении — событие `wake_install_done` со статусом.
#[tauri::command]
pub fn wake_install_models(app: AppHandle) {
    let d = crate::daemon::Daemon::get(&app);
    let proxy = d.settings.proxy();
    std::thread::spawn(move || {
        let r = install::install_wakeword(
            &|step: Step| {
                let _ = app.emit_to("main", "wake_install_progress", step);
            },
            proxy.as_deref(),
        );
        let _ = app.emit_to(
            "main",
            "wake_install_done",
            serde_json::json!({
                "ok": r.is_ok(),
                "error": r.err(),
                "models_present": install::status().wakeword_models,
            }),
        );
    });
}

/// Установить голос Silero (venv + torch/deps + модель) по запросу из раздела
/// «Модели». Переиспользует UI-события STT: прогресс → `stt_install_progress`,
/// финал → `stt_install_done` (kind=silero) — строка модели «silero» в той же панели.
#[tauri::command]
pub fn voice_install_silero(app: AppHandle) {
    let d = crate::daemon::Daemon::get(&app);
    let proxy = d.settings.proxy();
    std::thread::spawn(move || {
        let r = install::install_silero(
            &|step: Step| {
                let _ = app.emit_to("main", "stt_install_progress", step);
            },
            proxy.as_deref(),
        );
        let _ = app.emit_to(
            "main",
            "stt_install_done",
            serde_json::json!({
                "kind": "silero",
                "ok": r.is_ok(),
                "error": r.err(),
                "ready": install::status().silero,
            }),
        );
    });
}

/// Скачать веса Qwen3 (`qwen3-0.6b`/`qwen3-1.7b`) в локальную папку сайдкара —
/// гибридной загрузкой (HF через прокси, CDN напрямую). Сайдкар затем берёт их
/// локально, без похода в HF. Фоном, fail-safe; прогресс → `stt_install_progress`,
/// финал → `stt_install_done` (kind = ключ модели).
#[tauri::command]
pub fn stt_install_qwen(app: AppHandle, key: String) {
    let d = crate::daemon::Daemon::get(&app);
    let proxy = d.settings.proxy();
    std::thread::spawn(move || {
        let r = install::preload_qwen(
            &key,
            &|step: Step| {
                let _ = app.emit_to("main", "stt_install_progress", step);
            },
            proxy.as_deref(),
        );
        let ready = install::qwen_weights_present(&key);
        let _ = app.emit_to(
            "main",
            "stt_install_done",
            serde_json::json!({
                "kind": key,
                "ok": r.is_ok(),
                "error": r.err(),
                "ready": ready,
            }),
        );
    });
}

/// Скачать НАБОР моделей последовательно в фоне (онбординг и панель «Модели»).
/// Единые события: `model_install_progress {id, step}` (прогресс по строке id),
/// `model_install_done {id, ok, error}`, в конце `models_install_all_done`.
/// Сбой одной модели НЕ прерывает очередь — остальные качаются дальше.
#[tauri::command]
pub fn models_install(app: AppHandle, ids: Vec<String>) -> InstallJobSnapshot {
    let d = crate::daemon::Daemon::get(&app);
    let proxy = d.settings.proxy();
    let plan = install::plan_install(&ids, &install::installed_state());
    let tasks: Vec<String> = plan.iter().map(|task| task.id.clone()).collect();
    let started = match start_job("models", tasks) {
        Ok(started) => started,
        Err(running) => return running,
    };
    std::thread::spawn(move || {
        let mut failures = Vec::new();
        for task in &plan {
            let app_p = app.clone();
            let id_p = task.id.clone();
            let prog = move |step: Step| {
                let snapshot = update_job_step(&id_p, &step);
                emit_both(
                    &app_p,
                    "model_install_progress",
                    serde_json::json!({ "id": id_p, "step": step }),
                );
                emit_both(
                    &app_p,
                    "install_job_changed",
                    serde_json::to_value(snapshot).unwrap_or(Value::Null),
                );
            };
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                install::run_install_task(&task.id, &prog, proxy.as_deref())
            }))
            .unwrap_or_else(|_| Err("installer аварийно остановился; повтор безопасен".into()));
            let error = r.err();
            if let Some(message) = &error {
                failures.push(format!("{}: {message}", task.id));
            }
            emit_both(
                &app,
                "model_install_done",
                serde_json::json!({ "id": task.id, "ok": error.is_none(), "error": error }),
            );
        }
        let snapshot = finish_job(failures);
        let payload = serde_json::to_value(&snapshot).unwrap_or(Value::Null);
        emit_both(&app, "install_job_changed", payload.clone());
        emit_both(&app, "models_install_all_done", payload);
    });
    started
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_machine_rejects_duplicate_start_and_replaces_latest_step() {
        let mut jobs = JobMachine::default();
        let first = jobs.start("core", vec![]).expect("first job starts");
        assert_eq!(first.state, InstallJobState::Running);
        assert!(jobs.start("models", vec!["silero".into()]).is_err());

        jobs.record_step("core", StepView::new("Хуки", "start", ""));
        jobs.record_step("core", StepView::new("Хуки", "done", "готово"));
        assert_eq!(
            jobs.snapshot.steps.len(),
            1,
            "phase is upserted, not duplicated"
        );
        assert_eq!(jobs.snapshot.steps[0].state, "done");

        jobs.finish(Vec::new());
        assert_eq!(jobs.snapshot.state, InstallJobState::Done);
        assert!(jobs.start("models", vec!["silero".into()]).is_ok());
    }

    #[test]
    fn job_machine_keeps_failures_for_reopen_snapshot() {
        let mut jobs = JobMachine::default();
        jobs.start("models", vec!["silero".into()]).unwrap();
        jobs.finish(vec!["silero: сеть недоступна".into()]);
        let reopened = jobs.current();
        assert_eq!(reopened.state, InstallJobState::Failed);
        assert_eq!(reopened.failures, vec!["silero: сеть недоступна"]);
    }

    #[test]
    fn readiness_requires_real_core_health_not_thread_completion() {
        let status = Status::default();
        let mut health = install::IntegrationHealth {
            jarvis_dir: "/tmp/jarvis".into(),
            hook_bin: false,
            socket: false,
            claude_present: true,
            claude_hooks_ok: true,
            codex_present: false,
            codex_hooks_ok: true,
            claude_shim: false,
            codex_shim: false,
        };
        let done_job = InstallJobSnapshot {
            state: InstallJobState::Done,
            ..InstallJobSnapshot::default()
        };
        assert!(
            !build_readiness(health.clone(), status.clone(), done_job.clone(), false).core_ready
        );
        health.hook_bin = true;
        assert!(build_readiness(health, status, done_job, true).core_ready);
    }
}
