//! Команды окна онбординга: статус интеграции + запуск установки со стримом шагов.
//!
//! Установка идёт в отдельном потоке (Silero тянет PyTorch — минуты), каждый шаг
//! летит событием `onboarding:progress` в окно `onboarding`; по завершении —
//! `onboarding:done` с финальным статусом.

use crate::install::{self, Step};
use tauri::{AppHandle, Emitter};

#[tauri::command]
pub fn onboarding_status() -> install::Status {
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
