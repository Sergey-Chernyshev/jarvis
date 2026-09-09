//! Jarvis — демон + меню-бар + панель (Rust/Tauri).
//!
//! Main-процесс и есть демон: слушает unix-сокет ~/.jarvis/run.sock,
//! на который jarvis-hook кидает события из хуков Claude Code.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[allow(dead_code)] // UI-потребитель подключается в фазе 7 (chat UI)
mod agent;
mod attachments;
mod artifacts;
mod agents; // реестр внешних агентов: qwen/opencode/свои — шимы и жизненный цикл
mod agent_instances;
mod session_identity;
mod instance_ipc;
mod codex_live;
mod codex_titles;
mod rollout_scope;
use jarvis_node_shared::codex_hooks;
mod analytics; // локальная аналитика качества процесса, Git и результатов задач
#[allow(dead_code)] // Codex-методы наполняются по инкрементам (codex CLI support)
mod backend;
mod budget; // бюджет лимитов: живые проценты подписок, резерв, темп, лестница
mod bundle; // режим «Связка»: несколько агентов в worktree над одним проектом + очередь слияний
#[allow(dead_code)] // проекции/фасады подключаются по фазам (инкр. 8)
mod capability;
mod changes; // свод правок задачи: список файлов, дифф, приём и откат
mod claude_bin;
mod commands_catalog;
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
mod loops; // режим «Циклы»: рутина, которую агент крутит сам — с концом и стенами
mod platform; // окна, медиа, звук: платформенное за общим API (macos.rs / linux.rs)
mod meetings;
mod plugin; // плагинное ядро: «всё есть плагин» (спека 2026-08-19)
#[cfg(target_os = "macos")]
mod plugin_packages; // non-activating package foundations; runtime integration is separate
mod metrics;
mod model;
mod native_smoke;
mod native_smoke_checks;
mod onboarding;
mod origin; // происхождение промпта: человек / заход цепочки / оживление — ставит транспорт
mod power;
#[allow(dead_code)] // потребитель — daemon (маршрутизация удалённых сессий), следующий шаг инкремента
mod remote; // удалённые узлы: ssh-туннель, HTTP-клиент узла, поллер событий
mod remote_observer;
#[allow(dead_code)] // проводка (IPC/команды) — отдельным шагом, не этим инкрементом
mod revive; // оживление старых сессий: целостность транскрипта, оценка цены до запуска
mod route; // голосовая маршрутизация: скоринг → tie-break → пикер → stage-then-send
mod ru;
mod screen_prompt;
mod question_delivery;
mod symbols; // что именно тронул агент: объявления под правкой
mod search; // поиск по проекту задачи: git grep там, где живёт сессия
mod server;
mod settings;
mod shutdown;
#[allow(dead_code)] // STT-потребители подключаются в фазах 4-6 (инкр. 9)
mod stt;
mod tail;
mod terminal;
mod session_terminal;
use jarvis_node_shared::terminal_stream;
mod launch_task;
mod projects;
mod project_icons;
mod vm;
mod teleport;
mod tmux;
mod transcript;
mod tray;
mod turns;
mod turnsum;
mod usage;
mod util;
mod voice;
mod watchdog; // сторож главного потока: «зависло» → строка в логе с длительностью
mod wakeword; // wake-word детектор + шов верификации
mod windows;

use std::io;
use std::sync::Arc;
use std::time::Duration;
use tauri::Manager;
use tauri_plugin_autostart::ManagerExt; // autolaunch().is_enabled() — старт по логину или рукой

use daemon::Daemon;

/// Что просит второй запуск у уже работающего приложения.
///
/// Список намеренно короткий: это не CLI, а мостик для горячих клавиш
/// оконного менеджера. Всё остальное панель умеет сама.
#[derive(Debug, PartialEq)]
enum Command {
    Show,
    Hide,
    Toggle,
    Quit,
}

impl Command {
    fn parse(arg: &str) -> Option<Self> {
        match arg.trim_start_matches('-') {
            "toggle" => Some(Command::Toggle),
            "show" | "open" => Some(Command::Show),
            "hide" => Some(Command::Hide),
            "quit" | "exit" => Some(Command::Quit),
            _ => None,
        }
    }
}

fn main() {
    // До всего остального: паника, случившаяся раньше установки крючка, уйдёт
    // только в stderr — то есть мимо лога, который и присылают при разборе.
    log::install_panic_hook();
    if let Err(error) = native_smoke::initialize() {
        eprintln!("[native-smoke] {error}");
        std::process::exit(2);
    }

    let mut builder = tauri::Builder::default();

    // single-instance — только в проде; в dev-сборке (JARVIS_DEV=1) НЕ ставим,
    // чтобы dev и установленный прод крутились рядом, не гася друг друга.
    if std::env::var("JARVIS_DEV").is_err() && !native_smoke::enabled() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // Второй запуск — это не «подними ещё одно окно», а команда уже
            // работающему. На Wayland (Sway) без этого никак: глобальные
            // хоткеи там перехватывает композитор, а не приложение, и
            // `bindsym $mod+j exec jarvis --toggle` — единственный честный
            // способ дать панели горячую клавишу. Аргумента нет — прежнее
            // поведение, «покажись».
            let d = Daemon::get(app);
            match argv.iter().find_map(|a| Command::parse(a)) {
                Some(Command::Toggle) => windows::toggle_panel(&d),
                Some(Command::Hide) => windows::hide_panel(&d),
                Some(Command::Quit) => d.app.exit(0),
                Some(Command::Show) => windows::show_panel(&d),
                None => windows::show_application(&d),
            }
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
        .invoke_handler(tauri::generate_handler![
            instance_ipc::agent_instances_list,
            instance_ipc::agent_instances_save,
            instance_ipc::agent_instances_repair,
            instance_ipc::remote_source_repair,
            meetings::meetings_sources,
            meetings::meetings_list,
            meetings::meetings_status,
            meetings::meetings_start,
            meetings::meetings_stop,
            meetings::meetings_get,
            meetings::meetings_retranscribe,
            loops::ipc::loops_get,
            loops::ipc::loops_draft,
            loops::ipc::loops_catalog,
            bundle::ipc::bundle_get,
            bundle::ipc::bundle_draft,
            bundle::ipc::bundle_save,
            bundle::ipc::bundle_start,
            bundle::ipc::bundle_add_hand,
            bundle::ipc::bundle_pause,
            bundle::ipc::bundle_merge,
            bundle::ipc::bundle_remove,
            bundle::ipc::bundle_places,
            bundle::ipc::bundle_browse,
            loops::ipc::loops_compose,
            loops::ipc::loops_save,
            loops::ipc::loops_remove,
            loops::ipc::loops_bpmn_export,
            loops::ipc::loops_bpmn_import,
            loops::ipc::loops_bpmn_unlink,
            loops::ipc::loops_start,
            loops::ipc::loops_stop,
            loops::ipc::loops_intervene,
            loops::ipc::loops_answer,
            loops::ipc::loops_review,
            loops::ipc::loops_resume,
            loops::ipc::loops_diff,
            ipc::agents_list,
            native_smoke::native_smoke_report,
            native_smoke::native_smoke_screenshot,
            native_smoke_checks::native_smoke_probe,
            ipc::agents_save,
            ipc::state_get,
            ipc::workspace_open,
            ipc::system_accessibility_settings,
            ipc::dictation_cancel_insertion,
            ipc::state_clear,
            ipc::panel_hide,
            ipc::settings_get,
            ipc::settings_set,
            ipc::chat_open,
            ipc::chat_summarize,
            ipc::file_open,
            ipc::file_read,
            ipc::file_diff,
            ipc::session_changes,
            ipc::session_change_diff,
            ipc::session_commit,
            ipc::session_revert,
            ipc::session_push,
            ipc::session_review,
            ipc::session_search,
            ipc::preview_open,
            ipc::session_touched,
            ipc::url_open,
            ipc::ui_error,
            ipc::chat_close,
            ipc::commands_get,
            ipc::app_meta,
            ipc::update_check_install,
            ipc::app_relaunch,
            ipc::plugins_status,
            ipc::plugins_cmd,
            ipc::plugin_set,
            ipc::usage_summary,
            ipc::analytics_report,
            ipc::analytics_save_outcome,
            ipc::analytics_config_get,
            ipc::analytics_config_save,
            ipc::analytics_config_defaults,
            ipc::limit_get,
            ipc::history_get,
            ipc::projects_list,
            ipc::projects_save,
            ipc::projects_remove,
            ipc::projects_icon_candidates,
            ipc::vm_status,
            ipc::vm_action,
            ipc::teleport_status,
            ipc::teleport_nodes,
            ipc::teleport_login,
            ipc::usage_session,
            ipc::session_set_pin,
            ipc::session_rename,
            ipc::session_kill,
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
            ipc::voice_set_bluetooth_only,
            ipc::session_reply,
            ipc::session_terminal_snapshot,
            ipc::session_terminal_key,
            ipc::session_terminal_action,
            ipc::session_save_image,
            attachments::session_save_attachment,
            windows::file_dialog_state,
            artifacts::artifact_read,
            artifacts::artifact_notes,
            ipc::session_continue,
            ipc::agent_confirm,
            ipc::voice_pick_resolve,
            ipc::voice_stage_cancel,
            ipc::voice_audio_state,
            ipc::voice_confirm_resolve,
            ipc::voice_abort,
            ipc::agent_chat_window,
            ipc::agent_chat_open,
            ipc::agent_chat_state,
            ipc::agent_chat_history,
            ipc::agent_chat_reset,
            ipc::agent_chats_list,
            ipc::agent_chat_switch,
            ipc::agent_chat_create,
            ipc::agent_chat_rename,
            ipc::agent_chat_delete,
            ipc::agent_chat_reorder,
            ipc::agent_drafts_get,
            ipc::agent_draft_set,
            ipc::agent_history_hide,
            ipc::agent_history_unhide_all,
            ipc::agent_history_forget,
            ipc::agent_chain_state,
            ipc::agent_chain_mode,
            ipc::agent_chain_stop,
            ipc::agent_chain_resume,
            ipc::agent_stop,
            ipc::agent_chain_watch,
            ipc::agent_chain_send,
            ipc::terminal_focus,
            ipc::session_launch,
            ipc::session_continue_managed,
            ipc::remotes_list,
            ipc::remotes_add,
            ipc::remotes_remove,
            ipc::remotes_test,
            ipc::remotes_preflight,
            ipc::remotes_install,
            ipc::remotes_ssh_key,
            ipc::remotes_ssh_authorize,
            ipc::machines_list,
            ipc::toast_resize,
            ipc::toast_ready,
            ipc::toast_click,
            onboarding::onboarding_status,
            onboarding::onboarding_get,
            onboarding::onboarding_run,
            onboarding::onboarding_open,
            onboarding::onboarding_close,
            onboarding::onboarding_open_panel,
            onboarding::onboarding_open_settings,
            onboarding::integration_get,
            onboarding::integration_remove,
            onboarding::model_delete,
            onboarding::quiet_set,
            ipc::agent_send,
            ipc::agent_hosts,
            ipc::stt_get,
            ipc::models_get,
            ipc::transcripts_get,
            ipc::transcripts_clear,
            ipc::transcript_delete,
            ipc::transcript_update,
            ipc::transcript_retranscribe,
            ipc::transcript_enhance,
            ipc::prompts_get_settings,
            ipc::prompts_set_smart,
            ipc::prompts_get,
            ipc::stt_set_engine,
            ipc::stt_set_hotkey,
            ipc::hotkey_bindings,
            ipc::hotkey_assign,
            ipc::hotkeys_suspend,
            ipc::stt_set_noise_gate,
            ipc::stt_test,
            ipc::voice_history_open,
            ipc::stt_input_devices,
            ipc::stt_set_input_device,
            ipc::service_get,
            ipc::service_set_backend,
            ipc::service_set_model,
            ipc::service_set_effort,
            ipc::service_set_proxy,
            ipc::service_test,
            ipc::claude_auth_get,
            ipc::claude_auth_connect,
            ipc::claude_auth_disconnect,
            onboarding::codex_install_sidecar,
            onboarding::stt_install_whisper,
            onboarding::stt_install_sidecar,
            onboarding::stt_install_qwen,
            ipc::wake_get,
            ipc::wake_set_enabled,
            ipc::wake_set_threshold,
            ipc::audio_set_mute,
            onboarding::wake_install_models,
            onboarding::voice_install_silero,
            onboarding::models_install,
            stt::voice_data::dictionary_get,
            stt::voice_data::dictionary_add,
            stt::voice_data::dictionary_remove,
            stt::voice_data::scratchpad_get,
            stt::voice_data::scratchpad_set,
        ])
        .setup(|app| {
            // Профильный lock ДО Daemon::new: второй процесс того же JARVIS_DIR
            // не стартует, но prod/dev и чужие listeners больше не убиваются.
            install::prepare_clean_start().map_err(|err| {
                io::Error::new(io::ErrorKind::AlreadyExists, format!("Jarvis profile already running: {err}"))
            })?;

            // миграция схемы settings.json ДО первого чтения настроек (Daemon::new
            // их читает). Сейчас no-op v0→v1; задел под ломающие изменения формата.
            settings::Store::new().migrate_on_startup();

            let d = Arc::new(Daemon::new(app.handle().clone()));
            app.manage(d.clone());
            app.manage(meetings::Meetings::new());

            // Real windows and commands, isolated from the user's integrations.
            // Only an explicitly validated debug launch can enter this branch.
            if native_smoke::enabled() {
                #[cfg(target_os = "macos")]
                app.set_activation_policy(tauri::ActivationPolicy::Regular);
                let panel = windows::create_panel(app.handle())?;
                windows::create_toast(app.handle())?;
                windows::show_panel(&d);
                panel.set_focus()?;
                // The validated temporary socket also exercises real MCP and
                // grants. It cannot receive the user's production hooks.
                tauri::async_runtime::spawn(server::serve(d.clone()));
                return Ok(());
            }

            // Накладка ⌘J — чистое меню-бар приложение без иконки в доке.
            // Оконный режим (макет 14h) — обычное приложение: док, ⌘Tab, меню.
            // Стартуем всегда как Accessory (LSUIElement в Info.plist), поэтому
            // здесь бывает только повышение до Regular; смена режима на лету
            // делает то же самое сама (windows::apply_mode).
            // Понятие политики активации есть только у AppKit: на Linux место
            // приложения в панели задач решает сам оконный менеджер по
            // skip_taskbar, который выставляется при создании окна.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(if d.settings.string("mode") == "window" {
                tauri::ActivationPolicy::Regular
            } else {
                tauri::ActivationPolicy::Accessory
            });
            shutdown::install(app.handle().clone());

            // Конфиг служебного LLM («Под капотом»: Claude/Codex + модель) — из
            // настроек в процесс-глобал, чтобы run_service_llm сразу его видел.
            crate::claude_bin::set_service_config(
                crate::claude_bin::ServiceConfig::from_settings(&d.settings.load()),
            );

            d.restore_state(); // реестр переживает перезапуск
            d.start_remotes(); // ssh-туннели к удалённым узлам, если они настроены
            windows::create_panel(app.handle())?;
            windows::create_toast(app.handle())?;
            tray::init(&d)?;

            // Первый запуск без интеграции — онбординг. Иначе показываем панель,
            // чтобы запуск был видимым (а не «ничего не открылось»), — но только
            // когда приложение запустил человек. При включённом автозапуске старт
            // случается на КАЖДОМ логине, и панель поверх всего в момент, когда
            // человек уже что-то печатает, — не приветствие, а помеха; признак
            // жизни там — иконка в меню-баре.
            if !install::integration_health().ok() {
                let _ = windows::create_onboarding(app.handle());
            } else {
                windows::show_application(&d);
            }

            // unix-сокет — канал событий от хуков
            tauri::async_runtime::spawn(server::serve(d.clone()));

            // Плагины — после трея: их статусы обновляют его заголовок.
            // Сначала общий кэш процессов power (им пользуется трей «Не спать»),
            // потом хост поднимает всё, что включено тумблером.
            power::Power::init(&d);
            d.plugins.init(&d); // плагины: поднять всё, что включено
            power::Power::sweep_stale_lid(&d); // выключенная «Крышка» — не повод не спать

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
            // Шимы своих агентов — к настройкам: их могли поправить руками
            // в settings.json, пока приложение не работало.
            let cfg = d.settings.load();
            std::thread::spawn(move || {
                crate::install::sync_custom_shims(&crate::agents::shim_specs(&crate::agents::parse(&cfg)));
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
            if windows::is_workspace(window.label()) {
                if matches!(event, tauri::WindowEvent::Destroyed) {
                    Daemon::get(window.app_handle()).tail.remove_window(window.label());
                }
                if let tauri::WindowEvent::CloseRequested { .. } = event {
                    if let (Ok(size), Ok(scale)) = (window.inner_size(), window.scale_factor()) {
                        let size = size.to_logical::<f64>(scale);
                        windows::remember_window_size(&Daemon::get(window.app_handle()), size.width, size.height);
                    }
                }
                return;
            }
            if !matches!(window.label(), "main" | "agent-chat") {
                return;
            }
            match event {
                // ⌘W и крестик — просто прячем, демон живёт
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    windows::remember_geometry(&Daemon::get(window.app_handle()), window.label());
                    let _ = window.hide();
                }
                // клик вне панели — спрятать. Но с задержкой и перепроверкой:
                // навигация стрелками перерисовывает DOM (render() пересоздаёт и
                // рефокусит queryEl), отчего WKWebView даёт ложный blur→focus за
                // один кадр. Гасим только если фокус реально ушёл из приложения и
                // не вернулся за 120 мс — иначе панель моргала бы на каждой стрелке.
                tauri::WindowEvent::Focused(false) => {
                    // обычное окно не исчезает от клика мимо — это поведение
                    // накладки; чат с агентом — тоже обычное окно
                    if window.label() != "main" {
                        return;
                    }
                    let w = window.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(120));
                        if !windows::file_dialog_open(w.label()) && !w.is_focused().unwrap_or(false) && w.is_visible().unwrap_or(false) {
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
            #[cfg(target_os = "macos")]
            if matches!(&event, tauri::RunEvent::Reopen { has_visible_windows: false, .. }) {
                windows::show_application(&Daemon::get(app));
            }
            if let tauri::RunEvent::Exit = event {
                if native_smoke::enabled() { return; }
                let d = Daemon::get(app);
                d.write_state_now(); // реестр переживает перезапуск
                // размер окна тоже: снимаем один раз здесь, а не на каждом кадре ресайза
                if let Some(w) = app.get_webview_window("workspace") {
                    if let (Ok(sz), Ok(sf)) = (w.inner_size(), w.scale_factor()) {
                        let l = sz.to_logical::<f64>(sf);
                        windows::remember_window_size(&d, l.width, l.height);
                    }
                }
                windows::remember_geometry(&d, "workspace");
                windows::remember_geometry(&d, "agent-chat");
                // ssh-дети не должны пережить приложение: без этого туннели
                // висят до конца сессии терминала и держат порты
                d.remotes.stop_all_now();
                // Погасить плагины: «Не спать» снимет assertion, «Крышка» вернёт
                // disablesleep, сайдкары получат SIGTERM и лишатся токенов.
                d.plugins.dispose(&d);
                d.voice.dispose(); // погасить Silero-сайдкар, если был поднят
                d.stt.dispose(); // погасить Qwen3-MLX-сайдкар, если был поднят
                d.wake.dispose(); // остановить wake-word consumer-поток
                app.state::<Arc<meetings::Meetings>>().dispose();
                d.audio.dispose(); // остановить общий аудио-захват (drop cpal Stream)
                let _ = std::fs::remove_file(util::sock_path());
            }
        });
}

/// Все периодические задачи демона — расписание из Electron-версии.
fn spawn_timers(d: &Arc<Daemon>) {
    codex_live::start(d.clone());
    remote_observer::start(d.clone());
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

    // Кто именно запущен. Без этой строки любой отчёт о поломке начинается с
    // выяснения, та ли сборка на руках, — а выяснить это по логу было нечем.
    // Отпечаток панели считается по содержимому `ui/`: он же показывает, что
    // ассеты пересобрались, а не остались от прошлого раза.
    log::line(&format!(
        "[jarvis] сборка {} · панель {} · v{}",
        env!("JARVIS_BUILD_REF"),
        env!("JARVIS_UI_FINGERPRINT"),
        env!("CARGO_PKG_VERSION"),
    ));

    // Сторож главного потока: если окно встанет, в логе останется след с
    // длительностью — иначе от «зависло» нет ни места, ни времени.
    watchdog::start(d.app.clone());

    // такт связки: следит за руками, ребейзит готовых, гоняет гейты. Чаще
    // семи секунд незачем — он ходит в git, а руки работают минутами.
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(7)).await;
            bundle::ipc::tick(&dd).await;
        }
    });

    // расписание циклов: раз в 30с смотрим, чьё время пришло. Чаще незачем —
    // самое частое расписание меряется минутами, а запуск всё равно один за раз.
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            loops::ipc::tick(&dd);
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

    // Бюджет лимитов: ОДИН опросчик на всё приложение, частота по надобности
    // (см. budget.rs). Прежний безусловный опрос раз в 5 минут через мёртвый
    // скрейпинг `claude -p /usage` заменён им целиком.
    budget::spawn_poller(d);

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
                platform::poll_toast_hover(&toast);
            }
        }
    });
}
