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
pub fn onboarding_run(app: AppHandle) {
    std::thread::spawn(move || {
        install::install(&|step: Step| {
            let _ = app.emit_to("onboarding", "onboarding:progress", step);
        });
        let _ = app.emit_to("onboarding", "onboarding:done", install::status());
    });
}

/// Открыть окно онбординга (кнопка «Настроить/Переустановить» из настроек).
#[tauri::command]
pub fn onboarding_open(app: AppHandle) {
    let _ = crate::windows::create_onboarding(&app);
}

/// Полная сводка интеграции для карточки настроек.
#[derive(Serialize)]
pub struct IntegrationInfo {
    status: Status,
    foreign_hooks: usize,
    models: Vec<Artifact>,
}

fn integration_info() -> IntegrationInfo {
    IntegrationInfo {
        status: install::status(),
        foreign_hooks: install::foreign_hook_count(),
        models: install::model_artifacts(),
    }
}

#[tauri::command]
pub fn integration_get() -> IntegrationInfo {
    integration_info()
}

/// Умный откат: снять наши хуки/шим/tmux/PATH (чужие хуки и Silero не трогаем).
#[tauri::command]
pub fn integration_remove() -> IntegrationInfo {
    install::uninstall(&|_step| {}); // быстрый, без сети/Silero
    integration_info()
}

/// Удалить голосовой артефакт по id и вернуть обновлённую сводку.
#[tauri::command]
pub fn model_delete(id: String) -> Result<IntegrationInfo, String> {
    install::delete_model(&id)?;
    Ok(integration_info())
}
