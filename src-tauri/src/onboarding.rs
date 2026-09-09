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

/// Подпись строки Claude Code на экране «Интеграция».
fn claude_detail(health: &install::IntegrationHealth) -> String {
    if !health.claude_present {
        return "CLI не найден в PATH".into();
    }
    if !health.hooks_elsewhere.is_empty() {
        return format!("Хуки уведены другой копией Jarvis: {}", health.hooks_elsewhere);
    }
    if !health.claude_hooks_ok {
        return "Хуки в ~/.claude/settings.json не совпадают с текущими".into();
    }
    if !health.hook_bin {
        return "Нет бинаря хука — агент зовёт несуществующий файл".into();
    }
    "События и lifecycle hooks".into()
}

/// Почему установка не сошлась — словами, по которым можно действовать.
///
/// Берём ровно те поля, что решают `IntegrationHealth::ok()`, и ни одного
/// лишнего: список причин, в котором есть неотносящееся к делу, читается как
/// шум и перестаёт читаться вовсе.
fn install_failure_reasons(health: &install::IntegrationHealth) -> Vec<String> {
    let mut out = Vec::new();
    if !health.hook_bin {
        out.push(format!(
            "Бинарь хука не встал: {}/bin/jarvis-hook. Без него агент зовёт несуществующий файл.",
            health.jarvis_dir
        ));
    }
    if !health.claude_present && !health.codex_present && !health.kimi_present {
        out.push(
            "Не найден ни один агентский CLI (claude, codex, kimi) — интеграции не с чем работать."
                .into(),
        );
    }
    if !health.hooks_elsewhere.is_empty() {
        out.push(format!(
            "Хуки зарегистрированы на другой каталог Jarvis ({}), а эта копия работает в {}. \
                 События уходят туда. Переустанови интеграцию из ЭТОЙ сборки — она \
                 перенацелит их на себя (обратно вернёт установка из той копии).",
            health.hooks_elsewhere, health.jarvis_dir
        ));
    } else {
        if health.claude_present && !health.claude_hooks_ok {
            out.push("Хуки Claude Code в ~/.claude/settings.json не совпадают с текущими.".into());
        }
        if health.codex_present && !health.codex_hooks_ok {
            out.push("Хуки Codex в ~/.codex/hooks.json не совпадают с текущими.".into());
        }
        if health.kimi_present && !health.kimi_hooks_ok {
            out.push("Блок хуков Kimi в ~/.kimi-code/config.toml не совпадает с текущим.".into());
        }
    }
    out
}

#[cfg(test)]
mod failure_reasons_tests {
    use super::*;

    fn health() -> install::IntegrationHealth {
        install::IntegrationHealth {
            jarvis_dir: "/home/u/.jarvis-dev".into(),
            hook_bin: true,
            socket: true,
            claude_present: true,
            claude_hooks_ok: true,
            codex_present: false,
            codex_hooks_ok: true,
            kimi_present: false,
            kimi_hooks_ok: true,
            hooks_elsewhere: String::new(),
            claude_shim: true,
            codex_shim: false,
            kimi_shim: false,
            input_guard: true,
        }
    }

    /// Ровно тот тупик, в который упирался человек: две копии приложения делят
    /// один ~/.claude/settings.json, установка из одной уводит хуки на её
    /// каталог — а отказ говорил лишь «не прошла итоговую проверку».
    #[test]
    fn hooks_stolen_by_another_copy_are_named_with_both_paths() {
        let mut h = health();
        h.claude_hooks_ok = false;
        h.hooks_elsewhere = "/home/u/.jarvis".into();
        assert!(!h.ok(), "такая установка не должна считаться готовой");

        let why = install_failure_reasons(&h).join(" ");
        assert!(why.contains("(/home/u/.jarvis)"), "не назван чужой каталог: {why}");
        assert!(why.contains("/home/u/.jarvis-dev"), "не назван свой каталог: {why}");
        assert!(why.contains("ЭТОЙ сборки"), "не сказано, что делать: {why}");
        assert!(!why.contains("  "), "в сообщении слиплись пробелы переноса: {why}");
        // И не сваливаем сверху общую фразу про «хуки не совпадают»: причина
        // одна, а два объяснения подряд читаются как два разных отказа.
        assert!(!why.contains("не совпадают с текущими"), "{why}");
    }

    #[test]
    fn a_plain_mismatch_is_still_reported_plainly() {
        let mut h = health();
        h.claude_hooks_ok = false;
        let why = install_failure_reasons(&h).join(" ");
        assert!(why.contains("~/.claude/settings.json"), "{why}");
        assert!(!why.contains("другой каталог"), "чужого каталога нет — не выдумываем: {why}");
    }

    #[test]
    fn a_missing_agent_and_a_missing_hook_binary_are_named() {
        let mut h = health();
        h.claude_present = false;
        h.hook_bin = false;
        let why = install_failure_reasons(&h).join(" ");
        assert!(why.contains("jarvis-hook"), "{why}");
        assert!(why.contains("ни один агентский CLI"), "{why}");
    }

    /// Экран «Интеграция» — тот, на который ссылается отказ. Он обязан говорить
    /// то же самое, а не «всё хорошо» рядом с сообщением об отказе.
    #[test]
    fn the_integration_screen_says_the_same_thing_as_the_refusal() {
        let mut h = health();
        h.claude_hooks_ok = false;
        h.hooks_elsewhere = "/home/u/.jarvis".into();
        assert_eq!(claude_detail(&h), "Хуки уведены другой копией Jarvis: /home/u/.jarvis");

        h.hooks_elsewhere.clear();
        assert!(claude_detail(&h).contains("не совпадают"), "{}", claude_detail(&h));

        h.claude_hooks_ok = true;
        assert_eq!(claude_detail(&h), "События и lifecycle hooks");

        h.claude_present = false;
        assert_eq!(claude_detail(&h), "CLI не найден в PATH");
    }

    /// Здоровой установке объяснять нечего — иначе список причин появлялся бы
    /// там, где всё в порядке.
    #[test]
    fn a_healthy_install_has_nothing_to_explain() {
        assert!(install_failure_reasons(&health()).is_empty());
    }
}

fn build_readiness(
    health: install::IntegrationHealth,
    status: Status,
    job: InstallJobSnapshot,
    proxy_configured: bool,
) -> ReadinessSnapshot {
    let core_ready = health.ok();
    let mut warnings = Vec::new();
    if !health.claude_present && !health.codex_present && !health.kimi_present {
        warnings.push("Не найден ни один агентский CLI (Claude Code, Codex, Kimi Code).".into());
    }
    if !health.hook_bin {
        warnings.push("Hook binary отсутствует — запусти восстановление интеграции.".into());
    }
    if health.claude_present && !health.claude_hooks_ok {
        // Разные болезни лечатся одинаково («переустанови»), но выглядят
        // по-разному, и молчать о разнице нельзя: человек, увидевший «требуют
        // восстановления» после того, как он только что восстанавливал, идёт
        // искать поломку не там.
        warnings.push(if health.hooks_elsewhere.is_empty() {
            "Claude Code hooks требуют восстановления.".into()
        } else {
            format!(
                "Хуки Claude Code зарегистрированы на другой каталог Jarvis ({}), а эта \
                 копия работает в {}. События уходят туда — переустанови интеграцию \
                 из ЭТОЙ сборки.",
                health.hooks_elsewhere, health.jarvis_dir
            )
        });
    }
    if health.codex_present && !health.codex_hooks_ok {
        warnings.push("Codex hooks требуют восстановления или подтверждения доверия.".into());
    }
    if health.kimi_present && !health.kimi_hooks_ok {
        warnings.push("Kimi hooks требуют восстановления.".into());
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
            // Экран, на который ссылается отказ установки, обязан говорить то же
            // самое, что и сам отказ. Раньше он показывал «События и lifecycle
            // hooks» при уведённых хуках — то есть подтверждал, что всё хорошо,
            // ровно там, куда человека послали искать причину.
            &claude_detail(&health),
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
        ReadinessItem::new(
            "kimi",
            "Kimi Code",
            health.kimi_present && health.kimi_hooks_ok && health.hook_bin,
            health.kimi_present,
            health.kimi_present,
            if health.kimi_present {
                "Hooks в ~/.kimi-code/config.toml; статусы, разрешения и пульс"
            } else {
                "CLI не найден в PATH"
            },
        )
        .action("Установить Kimi Code CLI или обновить PATH"),
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
            if status.whisper_native_built {
                "Локальная диктовка, ~574 МБ"
            } else {
                "Нужна сборка Jarvis с поддержкой Whisper"
            },
        ),
        ReadinessItem::new(
            "qwen3-runtime",
            "Qwen3-ASR",
            install::model_install_support("qwen3-runtime").is_ok()
                && status.qwen3_sidecar
                && (install::qwen_weights_present("qwen3-0.6b")
                    || install::qwen_weights_present("qwen3-1.7b")),
            install::model_install_support("qwen3-runtime").is_ok(),
            false,
            if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                "MLX runtime + проверенный комплект весов"
            } else {
                "Требуется Mac с Apple Silicon"
            },
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
            status.wakeword_models && (status.wakeword_ort_built && install::model_install_support("hey_jarvis").is_ok()),
            (status.wakeword_ort_built && install::model_install_support("hey_jarvis").is_ok()),
            false,
            if cfg!(feature = "wakeword-ort") {
                "Опциональная голосовая активация"
            } else {
                "Нужна сборка Jarvis с голосовой активацией"
            },
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
    crate::windows::emit_to_panel(app, event, &payload);
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
        if let Err(error) = d.settings.try_set_block("service", service) {
            let failed = finish_job(vec![error]);
            emit_both(&app, "install_job_changed", serde_json::to_value(&failed).unwrap_or(Value::Null));
            return failed;
        }
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
        // Отказ обязан НАЗЫВАТЬ причину. «Не прошла итоговую проверку» плюс
        // совет открыть вкладку «Интеграция» были тупиком вдвойне: вкладка
        // считала чужой хук своим и показывала всё зелёным.
        let failures = match outcome {
            Ok(health) if health.ok() => Vec::new(),
            Ok(health) => {
                let why = install_failure_reasons(&health);
                if why.is_empty() {
                    vec!["Core integration не прошла итоговую readiness-проверку".into()]
                } else {
                    why
                }
            }
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
pub fn quiet_set(app: AppHandle, on: bool) -> Result<(), String> {
    crate::daemon::Daemon::get(&app).set_quiet(on)
}

/// Скачать модель Whisper large-v3-turbo-q5 (~574 МБ) по запросу из настроек.
/// Раньше скачивания не было вообще — теперь панель ПРЕДЛАГАЕТ загрузку (по
/// умолчанию ничего не тянем, как и просил пользователь). Фоном, fail-safe:
/// прогресс → `stt_install_progress`, финал → `stt_install_done` (kind=whisper).
#[tauri::command]
pub fn stt_install_whisper(app: AppHandle) -> Result<(), String> {
    install::model_install_support("whisper-turbo")?;
    let d = crate::daemon::Daemon::get(&app);
    let proxy = d.settings.proxy();
    std::thread::spawn(move || {
        let r = install::install_whisper(
            &|step: Step| {
                crate::windows::emit_to_panel(&app, "stt_install_progress", &step);
            },
            proxy.as_deref(),
        );
        crate::windows::emit_to_panel(
            &app,
            "stt_install_done",
            &serde_json::json!({
                "kind": "whisper",
                "ok": r.is_ok(),
                "error": r.err(),
                "ready": install::status().whisper_model,
            }),
        );
    });
    Ok(())
}

/// Установить Qwen3-ASR MLX-сайдкар (venv + зависимости, ~2.6 ГБ) по запросу из
/// настроек. Сами веса Qwen3 догрузятся сайдкаром при первом запросе. Фоном,
/// fail-safe; прогресс → `stt_install_progress`, финал → `stt_install_done`
/// (kind=qwen3).
#[tauri::command]
pub fn stt_install_sidecar(app: AppHandle) -> Result<(), String> {
    install::model_install_support("qwen3-runtime")?;
    let d = crate::daemon::Daemon::get(&app);
    let proxy = d.settings.proxy();
    std::thread::spawn(move || {
        let r = install::install_stt_sidecar(
            &|step: Step| {
                crate::windows::emit_to_panel(&app, "stt_install_progress", &step);
            },
            proxy.as_deref(),
        );
        crate::windows::emit_to_panel(
            &app,
            "stt_install_done",
            &serde_json::json!({
                "kind": "qwen3",
                "ok": r.is_ok(),
                "error": r.err(),
                "ready": install::status().qwen3_sidecar,
            }),
        );
    });
    Ok(())
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
                crate::windows::emit_to_panel(&app, "codex_install_progress", &step);
            },
            proxy.as_deref(),
        );
        crate::windows::emit_to_panel(
            &app,
            "codex_install_done",
            &serde_json::json!({
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
pub fn wake_install_models(app: AppHandle) -> Result<(), String> {
    install::model_install_support("hey_jarvis")?;
    let d = crate::daemon::Daemon::get(&app);
    let proxy = d.settings.proxy();
    std::thread::spawn(move || {
        let r = install::install_wakeword(
            &|step: Step| {
                crate::windows::emit_to_panel(&app, "wake_install_progress", &step);
            },
            proxy.as_deref(),
        );
        crate::windows::emit_to_panel(
            &app,
            "wake_install_done",
            &serde_json::json!({
                "ok": r.is_ok(),
                "error": r.err(),
                "models_present": install::status().wakeword_models,
            }),
        );
    });
    Ok(())
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
                crate::windows::emit_to_panel(&app, "stt_install_progress", &step);
            },
            proxy.as_deref(),
        );
        crate::windows::emit_to_panel(
            &app,
            "stt_install_done",
            &serde_json::json!({
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
pub fn stt_install_qwen(app: AppHandle, key: String) -> Result<(), String> {
    if !matches!(key.as_str(), "qwen3-0.6b" | "qwen3-1.7b") {
        return Err(format!("Неизвестная модель Qwen: {key}"));
    }
    install::model_install_support(&key)?;
    let d = crate::daemon::Daemon::get(&app);
    let proxy = d.settings.proxy();
    std::thread::spawn(move || {
        let r = install::preload_qwen(
            &key,
            &|step: Step| {
                crate::windows::emit_to_panel(&app, "stt_install_progress", &step);
            },
            proxy.as_deref(),
        );
        let ready = install::qwen_weights_present(&key);
        crate::windows::emit_to_panel(
            &app,
            "stt_install_done",
            &serde_json::json!({
                "kind": key,
                "ok": r.is_ok(),
                "error": r.err(),
                "ready": ready,
            }),
        );
    });
    Ok(())
}

/// Скачать НАБОР моделей последовательно в фоне (онбординг и панель «Модели»).
/// Единые события: `model_install_progress {id, step}` (прогресс по строке id),
/// `model_install_done {id, ok, error}`, в конце `models_install_all_done`.
/// Сбой одной модели НЕ прерывает очередь — остальные качаются дальше.
#[tauri::command]
pub fn models_install(app: AppHandle, ids: Vec<String>) -> Result<InstallJobSnapshot, String> {
    // Validate the entire request before spawning any installer, even when a
    // stale UI or direct IPC caller submits a model unavailable in this build.
    for id in &ids {
        install::model_install_support(id)?;
    }
    let d = crate::daemon::Daemon::get(&app);
    let proxy = d.settings.proxy();
    let plan = install::plan_install_checked(&ids, &install::installed_state())?;
    let tasks: Vec<String> = plan.iter().map(|task| task.id.clone()).collect();
    let started = match start_job("models", tasks) {
        Ok(started) => started,
        Err(running) => return Ok(running),
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
    Ok(started)
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

    /// Скачанные веса без вкомпилированного движка — не «почти готово», а
    /// недоступная возможность: по этому полю онбординг не предлагает загрузку.
    #[test]
    fn capability_without_compiled_engine_is_unavailable() {
        let health = install::IntegrationHealth {
            jarvis_dir: "/tmp/jarvis".into(),
            hook_bin: true,
            socket: true,
            claude_present: true,
            claude_hooks_ok: true,
            codex_present: false,
            codex_hooks_ok: true,
            kimi_present: false,
            kimi_hooks_ok: true,
            hooks_elsewhere: String::new(),
        claude_shim: true,
            codex_shim: false,
            kimi_shim: false,
            input_guard: false,
        };
        let mut status = Status {
            whisper_model: true,
            wakeword_models: true,
            ..Status::default()
        };
        let cap = |snapshot: &ReadinessSnapshot, id: &str| {
            snapshot
                .capabilities
                .iter()
                .find(|item| item.id == id)
                .cloned()
                .expect("capability present")
        };

        let stub = build_readiness(
            health.clone(),
            status.clone(),
            InstallJobSnapshot::default(),
            false,
        );
        for id in ["whisper-turbo", "hey_jarvis"] {
            assert!(!cap(&stub, id).available, "{id} offered without engine");
            assert!(!cap(&stub, id).ready, "{id} pretends to be ready");
        }

        status.whisper_native_built = true;
        status.wakeword_ort_built = true;
        let built = build_readiness(health, status, InstallJobSnapshot::default(), false);
        for id in ["whisper-turbo", "hey_jarvis"] {
            assert!(cap(&built, id).available && cap(&built, id).ready);
        }
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
            kimi_present: false,
            kimi_hooks_ok: true,
            hooks_elsewhere: String::new(),
        claude_shim: false,
            codex_shim: false,
            kimi_shim: false,
            input_guard: false,
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

    #[test]
    fn cached_artifacts_do_not_claim_features_absent_from_this_build() {
        let health = install::IntegrationHealth {
            jarvis_dir: "/tmp/jarvis-onboarding-test".into(),
            hook_bin: true,
            socket: true,
            claude_present: true,
            claude_hooks_ok: true,
            codex_present: false,
            codex_hooks_ok: false,
            claude_shim: false,
            codex_shim: false,
            kimi_shim: false, kimi_present: false, kimi_hooks_ok: false, input_guard: true, hooks_elsewhere: String::new(),
        };
        let status = Status {
            whisper_model: true,
            whisper_native_built: false,
            wakeword_models: true,
            wakeword_ort_built: cfg!(feature = "wakeword-ort"),
            qwen3_sidecar: true,
            ..Status::default()
        };
        let readiness = build_readiness(health, status, InstallJobSnapshot::default(), false);
        for capability in &readiness.capabilities {
            if !capability.available {
                assert!(
                    !capability.ready,
                    "{} cannot run in this build",
                    capability.id
                );
            }
        }
        let whisper = readiness
            .capabilities
            .iter()
            .find(|item| item.id == "whisper-turbo")
            .unwrap();
        assert!(!whisper.ready && !whisper.available);
        let wake = readiness
            .capabilities
            .iter()
            .find(|item| item.id == "hey_jarvis")
            .unwrap();
        assert_eq!(wake.available, cfg!(feature = "wakeword-ort"));
        assert_eq!(wake.ready, wake.available);
        let qwen = readiness
            .capabilities
            .iter()
            .find(|item| item.id == "qwen3-runtime")
            .unwrap();
        assert_eq!(
            qwen.available,
            cfg!(all(target_os = "macos", target_arch = "aarch64"))
        );
    }

    #[test]
    fn voice_capabilities_are_ready_without_installed_cli_agents() {
        let health = install::IntegrationHealth {
            jarvis_dir: "/tmp/jarvis-onboarding-test".into(),
            hook_bin: false,
            socket: false,
            claude_present: false,
            claude_hooks_ok: false,
            codex_present: false,
            codex_hooks_ok: false,
            claude_shim: false,
            codex_shim: false,
            kimi_shim: false, kimi_present: false, kimi_hooks_ok: false, input_guard: true, hooks_elsewhere: String::new(),
        };
        let status = Status {
            silero: true,
            ..Status::default()
        };
        let readiness = build_readiness(health, status, InstallJobSnapshot::default(), false);
        assert!(!readiness.core_ready);
        assert!(readiness.agents.iter().all(|agent| !agent.ready));
        assert!(readiness
            .capabilities
            .iter()
            .any(|item| item.id == "silero" && item.ready));
    }
}
