//! Проигрывание WAV на системный output. Прерываемое: stop текущего sink.
//! Трейт `Play` — чтобы очередь тестировалась без звуковой карты.

use std::io::Cursor;
use std::sync::Mutex;

pub trait Play: Send + Sync {
    /// Сыграть WAV-байты СИНХРОННО (блокирует до конца или прерывания). true — доиграло.
    fn play_blocking(&self, wav: Vec<u8>) -> bool;
    /// Прервать текущее проигрывание (для высокоприоритетной реплики).
    fn stop(&self);
}

pub struct RodioPlayer {
    current: Mutex<Option<rodio::Sink>>,
}

impl RodioPlayer {
    pub fn new() -> Self {
        RodioPlayer { current: Mutex::new(None) }
    }
}

impl Play for RodioPlayer {
    fn play_blocking(&self, wav: Vec<u8>) -> bool {
        // OutputStream is !Send in rodio 0.19, so we keep it local (not in a field).
        // It must stay alive for the duration of playback.
        let (_stream, handle) = match rodio::OutputStream::try_default() {
            Ok(x) => x,
            Err(e) => {
                crate::log::line(&format!("[voice] нет аудио-выхода: {e}"));
                return false;
            }
        };
        let sink = match rodio::Sink::try_new(&handle) {
            Ok(s) => s,
            Err(e) => {
                crate::log::line(&format!("[voice] sink: {e}"));
                return false;
            }
        };
        let src = match rodio::Decoder::new(Cursor::new(wav)) {
            Ok(s) => s,
            Err(e) => {
                crate::log::line(&format!("[voice] декод WAV: {e}"));
                return false;
            }
        };
        sink.append(src);
        *self.current.lock().unwrap() = Some(sink);
        loop {
            let done = {
                let g = self.current.lock().unwrap();
                g.as_ref().map(|s| s.empty()).unwrap_or(true)
            };
            if done {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        let interrupted = self.current.lock().unwrap().is_none();
        *self.current.lock().unwrap() = None;
        !interrupted
    }

    fn stop(&self) {
        if let Some(s) = self.current.lock().unwrap().take() {
            s.stop();
        }
    }
}
