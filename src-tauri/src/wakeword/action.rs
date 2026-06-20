//! Действие при подтверждённом wake (после стадий 1+2). За интерфейсом
//! `WakeAction`, чтобы сервис wake тестировался моками, а прод-цепочка
//! (видимая реакция → STT-захват с прероллом → агент) была сменной.
//!
//! v1 (см. дизайн §4, спека §16): wake лишь «дёргает» существующий STT-захват;
//! полный голосовой цикл (TTS-ответ) — следующая фаза. Эндпойнтинг — фикс-окно
//! (VAD отложен), всё fail-safe.

use std::sync::Arc;

use serde_json::json;

use crate::stt::hub::AudioHub;
use crate::stt::SttService;

/// Реакция на подтверждённый wake. `preroll` — снимок аудио ДО срабатывания
/// (16к моно), чтобы не потерять начало реплики.
pub trait WakeAction: Send + Sync {
    fn on_wake(&self, preroll: Vec<f32>);
}

/// Прод-действие: emit «wake» в панель + STT-захват(преролл+окно) + транскрипт → агент.
pub struct AgentWakeAction {
    hub: Arc<AudioHub>,
    stt: Arc<SttService>,
    app: tauri::AppHandle,
    /// Длина фикс-окна записи после wake, мс (нет VAD в v1).
    window_ms: u64,
}

impl AgentWakeAction {
    pub fn new(hub: Arc<AudioHub>, stt: Arc<SttService>, app: tauri::AppHandle) -> Arc<Self> {
        Arc::new(AgentWakeAction { hub, stt, app, window_ms: 4000 })
    }
}

impl WakeAction for AgentWakeAction {
    fn on_wake(&self, preroll: Vec<f32>) {
        // 1. видимая реакция (индикатор/тост панели)
        crate::windows::emit_to_panel(&self.app, "wake", &json!({ "phase": "detected" }));

        let hub = self.hub.clone();
        let stt = self.stt.clone();
        let app = self.app.clone();
        let window_ms = self.window_ms;

        // Тяжёлая часть — в отдельном потоке (захват+транскрипт+агент). Fail-safe.
        std::thread::spawn(move || {
            // 2. STT-захват реплики (преролл уже снят сервисом — добавляем его сами)
            let cap = hub.open_capture(false);
            std::thread::sleep(std::time::Duration::from_millis(window_ms));
            let live = match cap.finish() {
                Ok(p) => p,
                Err(e) => {
                    crate::log::line(&format!("[wake] capture finish: {e}"));
                    return;
                }
            };
            let mut pcm = preroll;
            pcm.extend_from_slice(&live);
            if pcm.is_empty() {
                crate::log::line("[wake] пустой PCM после wake, пропуск");
                return;
            }

            // 3. транскрипция активным STT-движком
            let opts = stt.options();
            let text = match stt.transcribe(&pcm, &opts) {
                Ok(r) => r.text.trim().to_string(),
                Err(e) => {
                    crate::log::line(&format!("[wake] transcribe: {e}"));
                    return;
                }
            };
            if text.is_empty() {
                crate::log::line("[wake] пустой транскрипт, пропуск");
                return;
            }
            crate::log::line(&format!("[wake] реплика: «{}»", crate::util::ellipsize(&text, 80)));
            crate::windows::emit_to_panel(&app, "wake", &json!({ "phase": "transcript", "text": text }));

            // 4. передать реплику агенту (как хоткей/IPC-вход)
            trigger_agent(app, text);
        });
    }
}

/// Запустить агент-сессию с текстовым входом (та же сборка инструментов, что
/// `ipc::agent_send`). Спавнит async-таску в рантайме демона.
fn trigger_agent(app: tauri::AppHandle, message: String) {
    use crate::agent::ClaudeCliHost;
    use crate::capability::{build_registry, grant::Consumer};
    use crate::util::jarvis_dir;

    let mcp_config = jarvis_dir().join("jarvis-mcp.json").to_string_lossy().to_string();
    let reg = build_registry();
    let agent = Consumer::agent();
    let tools: Vec<String> = reg
        .list_for(&agent.grant)
        .into_iter()
        .map(|m| format!("mcp__jarvis__{}", m.id.replace('.', "_")))
        .collect();
    let host = ClaudeCliHost { app, mcp_config };
    tauri::async_runtime::spawn(async move {
        host.run(&message, &tools, None).await;
    });
}

#[cfg(test)]
pub mod test_support {
    use super::WakeAction;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Считающее действие — для тестов сервиса wake.
    pub struct CountingAction {
        pub count: AtomicUsize,
        pub last_preroll_len: AtomicUsize,
    }
    impl CountingAction {
        pub fn new() -> Self {
            CountingAction { count: AtomicUsize::new(0), last_preroll_len: AtomicUsize::new(0) }
        }
    }
    impl WakeAction for CountingAction {
        fn on_wake(&self, preroll: Vec<f32>) {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.last_preroll_len.store(preroll.len(), Ordering::SeqCst);
        }
    }
}
