//! Jarvis — демон + меню-бар + панель (Rust/Tauri).
//!
//! Main-процесс и есть демон: слушает unix-сокет ~/.jarvis/run.sock,
//! на который jarvis-hook кидает события из хуков Claude Code.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod claude_bin;
mod commands_catalog;
mod daemon;
mod history;
mod ipc;
mod limits;
mod log;
mod macos;
mod model;
mod power;
mod ru;
mod screen_prompt;
mod server;
mod settings;
mod tail;
mod terminal;
mod tmux;
mod transcript;
mod tray;
mod usage;
mod util;
mod voice;
mod windows;

use std::sync::Arc;
use std::time::Duration;
use tauri::Manager;

use daemon::Daemon;

fn main() {
    tauri::Builder::default()
        // одна копия демона; вторая — просто показывает панель первой
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            windows::show_panel(&Daemon::get(app));
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        windows::toggle_hotkey_panel(&Daemon::get(app));
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            ipc::state_get,
            ipc::state_clear,
            ipc::panel_hide,
            ipc::settings_get,
            ipc::settings_set,
            ipc::chat_open,
            ipc::chat_close,
            ipc::commands_get,
            ipc::app_meta,
            ipc::plugins_status,
            ipc::plugins_cmd,
            ipc::usage_summary,
            ipc::limit_get,
            ipc::history_get,
            ipc::usage_session,
            ipc::session_set_pin,
            ipc::session_set_model,
            ipc::session_set_effort,
            ipc::terminal_ping,
            ipc::question_answer,
            ipc::task_action,
            ipc::voice_get,
            ipc::voice_set_speaker,
            ipc::voice_test,
            ipc::voice_set_mute,
            ipc::session_reply,
            ipc::terminal_focus,
            ipc::toast_resize,
            ipc::toast_ready,
            ipc::toast_click,
        ])
        .setup(|app| {
            // чистое меню-бар приложение: без иконки в доке
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let d = Arc::new(Daemon::new(app.handle().clone()));
            app.manage(d.clone());

            d.restore_state(); // реестр переживает перезапуск
            windows::create_panel(app.handle())?;
            windows::create_toast(app.handle())?;
            tray::init(&d)?;

            // unix-сокет — канал событий от хуков
            tauri::async_runtime::spawn(server::serve(d.clone()));

            // плагины питания (Не спать, Крышка) — после трея:
            // их changed() обновляет title
            power::Power::init(&d);

            if let Err(e) = ipc::register_hotkey(&d, &d.settings.string("hotkey")) {
                eprintln!("[jarvis] хоткей не зарегистрировался: {e}");
            }

            spawn_timers(&d);
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            let d = Daemon::get(window.app_handle());
            match event {
                // ⌘W и крестик — просто прячем, демон живёт
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                // raycast-режим: потеря фокуса — спрятаться
                tauri::WindowEvent::Focused(false) => {
                    if d.panel_focus_mode.load(std::sync::atomic::Ordering::SeqCst)
                        && window.is_visible().unwrap_or(false)
                    {
                        let _ = window.hide();
                    }
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("jarvis: не удалось собрать приложение")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let d = Daemon::get(app);
                d.write_state_now(); // реестр переживает перезапуск
                power::Power::dispose(&d); // снять assertion, вернуть disablesleep
                d.voice.dispose(); // погасить Silero-сайдкар, если был поднят
                let _ = std::fs::remove_file(util::sock_path());
            }
        });
}

/// Все периодические задачи демона — расписание из Electron-версии.
fn spawn_timers(d: &Arc<Daemon>) {
    // чистка умерших сессий: сразу и раз в 30с
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            dd.sweep_sessions().await;
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

    // секундный пульс плагинов питания (таймеры, сторожа, детект сна)
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            power::Power::tick(&dd).await;
        }
    });

    // супервизор Silero-сайдкара: раз в 5с перезапускаем, если упал (no-op для piper)
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let v = dd.voice.clone();
            let _ = tokio::task::spawn_blocking(move || v.tick()).await;
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
