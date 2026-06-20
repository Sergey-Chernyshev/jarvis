//! Команды окна онбординга: статус интеграции + запуск установки со стримом шагов.
//!
//! Установка идёт в отдельном потоке (Silero тянет PyTorch — минуты), каждый шаг
//! летит событием `onboarding:progress` в окно `onboarding`; по завершении —
//! `onboarding:done` с финальным статусом.

use crate::install::{self, Artifact, Status, Step};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

#[tauri::command]
pub fn onboarding_status() -> Status {
    install::status()
}

#[tauri::command]
pub fn onboarding_run(app: AppHandle, proxy: Option<String>) {
    let d = crate::daemon::Daemon::get(&app);
    // прокси: из аргумента, иначе из сохранённых настроек; непустой — сохраняем
    let proxy = proxy
        .filter(|p| !p.trim().is_empty())
        .or_else(|| {
            let s = d.settings.string("proxy");
            (!s.is_empty()).then_some(s)
        });
    if let Some(p) = &proxy {
        d.settings.set_top("proxy", serde_json::Value::String(p.clone()));
    }
    std::thread::spawn(move || {
        install::install(
            &|step: Step| {
                let _ = app.emit_to("onboarding", "onboarding:progress", step);
            },
            proxy.as_deref(),
        );
        let _ = app.emit_to("onboarding", "onboarding:done", install::status());
    });
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

/// Полная сводка интеграции для карточки настроек.
#[derive(Serialize)]
pub struct IntegrationInfo {
    status: Status,
    foreign_hooks: usize,
    models: Vec<Artifact>,
    quiet: bool,
    proxy: String,
}

fn integration_info(app: &AppHandle) -> IntegrationInfo {
    let d = crate::daemon::Daemon::get(app);
    IntegrationInfo {
        status: install::status(),
        foreign_hooks: install::foreign_hook_count(),
        models: install::model_artifacts(),
        quiet: d.is_quiet(),
        proxy: d.settings.string("proxy"),
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

/// Скачать 3 ONNX-модели wake-word (инкр. 10) с прогрессом в панель. Фоном,
/// fail-safe; по завершении — событие `wake_install_done` со статусом.
#[tauri::command]
pub fn wake_install_models(app: AppHandle) {
    let d = crate::daemon::Daemon::get(&app);
    let proxy = {
        let s = d.settings.string("proxy");
        (!s.is_empty()).then_some(s)
    };
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
