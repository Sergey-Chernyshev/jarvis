//! Jarvis — демон + меню-бар + панель (Rust/Tauri).
//!
//! Main-процесс и есть демон: слушает unix-сокет ~/.jarvis/run.sock,
//! на который jarvis-hook кидает события из хуков Claude Code.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[allow(dead_code)] // проекции/фасады подключаются по фазам (инкр. 8)
mod capability;
#[allow(dead_code)] // UI-потребитель подключается в фазе 7 (chat UI)
mod agent;
mod claude_bin;
mod commands_catalog;
mod daemon;
mod history;
mod install;
mod ipc;
mod limits;
mod log;
mod macos;
mod metrics;
mod model;
mod onboarding;
mod power;
mod route; // голосовая маршрутизация: скоринг → tie-break → пикер → stage-then-send
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
#[allow(dead_code)] // STT-потребители подключаются в фазах 4-6 (инкр. 9)
mod stt;
mod wakeword; // инкремент 10: wake-word детектор + шов верификации

use std::sync::Arc;
use std::time::Duration;
use tauri::Manager;

use daemon::Daemon;

fn main() {
    let mut builder = tauri::Builder::default();

    // single-instance — только в проде; в dev-сборке (JARVIS_DEV=1) НЕ ставим,
    // чтобы dev и установленный прод крутились рядом, не гася друг друга.
    if std::env::var("JARVIS_DEV").is_err() {
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
                    } else if let Some(n) = ipc::is_select_hotkey(shortcut) {
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
            ipc::voice_set_rate,
            ipc::voice_test,
            ipc::voice_set_mute,
            ipc::voice_set_duck,
            ipc::session_reply,
            ipc::session_continue,
            ipc::agent_confirm,
            ipc::voice_pick_resolve,
            ipc::voice_stage_cancel,
            ipc::voice_audio_state,
            ipc::agent_chat_open,
            ipc::terminal_focus,
            ipc::toast_resize,
            ipc::toast_ready,
            ipc::toast_click,
            onboarding::onboarding_status,
            onboarding::onboarding_run,
            onboarding::onboarding_open,
            onboarding::onboarding_close,
            onboarding::onboarding_open_settings,
            onboarding::integration_get,
            onboarding::integration_remove,
            onboarding::model_delete,
            onboarding::quiet_set,
            ipc::agent_send,
            ipc::stt_get,
            ipc::stt_set_engine,
            ipc::stt_test,
            onboarding::stt_install_whisper,
            onboarding::stt_install_sidecar,
            ipc::wake_get,
            ipc::wake_set_enabled,
            ipc::wake_set_threshold,
            ipc::audio_set_mute,
            onboarding::wake_install_models,
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

            // первый запуск без интеграции — онбординг; иначе показываем панель,
            // чтобы запуск приложения был видимым (а не «ничего не открылось»).
            if !install::status().integrated() {
                let _ = windows::create_onboarding(app.handle());
            } else {
                windows::show_panel(&d);
            }

            // unix-сокет — канал событий от хуков
            tauri::async_runtime::spawn(server::serve(d.clone()));

            // плагины питания (Не спать, Крышка) — после трея:
            // их changed() обновляет title
            power::Power::init(&d);

            if let Err(e) = ipc::register_hotkey(&d, &d.settings.string("hotkey")) {
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

            // updater: тихая проверка на старте; есть свежий релиз — ставим и просим перезапуск
            {
                use tauri_plugin_updater::UpdaterExt;
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Ok(updater) = handle.updater() {
                        if let Ok(Some(update)) = updater.check().await {
                            crate::log::line(&format!("[updater] доступна версия {}", update.version));
                            let _ = update.download_and_install(|_, _| {}, || {}).await;
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
                    let _ = window.hide();
                }
                // клик вне панели — спрятать. Но с задержкой и перепроверкой:
                // навигация стрелками перерисовывает DOM (render() пересоздаёт и
                // рефокусит queryEl), отчего WKWebView даёт ложный blur→focus за
                // один кадр. Гасим только если фокус реально ушёл из приложения и
                // не вернулся за 120 мс — иначе панель моргала бы на каждой стрелке.
                tauri::WindowEvent::Focused(false) => {
                    let w = window.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(120));
                        if !w.is_focused().unwrap_or(false) && w.is_visible().unwrap_or(false) {
                            let _ = w.hide();
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
                d.write_state_now(); // реестр переживает перезапуск
                power::Power::dispose(&d); // снять assertion, вернуть disablesleep
                d.voice.dispose(); // погасить Silero-сайдкар, если был поднят
                d.stt.dispose(); // погасить Qwen3-MLX-сайдкар, если был поднят
                d.wake.dispose(); // остановить wake-word consumer-поток
                d.audio.dispose(); // остановить общий аудио-захват (drop cpal Stream)
                let _ = std::fs::remove_file(util::sock_path());
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

    // секундный пульс плагинов питания (таймеры, сторожа, детект сна)
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            power::Power::tick(&dd).await;
        }
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
