//! Проигрывание WAV на системный output. Прерываемое: stop текущего sink.
//! Трейт `Play` — чтобы очередь тестировалась без звуковой карты.

use std::cell::RefCell;
use std::io::Cursor;
use std::sync::Mutex;
use std::time::Duration;

pub trait Play: Send + Sync {
    /// Сыграть WAV-байты СИНХРОННО (блокирует до конца или прерывания). true — доиграло.
    fn play_blocking(&self, wav: Vec<u8>) -> bool;
    /// Прервать текущее проигрывание (для высокоприоритетной реплики).
    fn stop(&self);
}

thread_local! {
    // OutputStream в rodio 0.19 — !Send, поэтому держим его в thread-local на
    // потоке-воркере (play_blocking всегда зовётся оттуда). Стрим живёт МЕЖДУ
    // репликами: не пере-захватываем аудио-устройство на каждую фразу — это и
    // давало щелчки, паузы между репликами и обрыв хвоста.
    static OUT: RefCell<Option<(rodio::OutputStream, rodio::OutputStreamHandle)>> =
        const { RefCell::new(None) };
}

pub struct RodioPlayer {
    // Sink — Send+Sync, держим в общем мьютексе, чтобы stop() мог прервать с
    // другого потока (высокоприоритетная реплика).
    current: Mutex<Option<rodio::Sink>>,
}

impl RodioPlayer {
    pub fn new() -> Self {
        RodioPlayer { current: Mutex::new(None) }
    }
}

impl Play for RodioPlayer {
    fn play_blocking(&self, wav: Vec<u8>) -> bool {
        let queued = OUT.with(|cell| {
            let mut out = cell.borrow_mut();
            if out.is_none() {
                match rodio::OutputStream::try_default() {
                    Ok(x) => *out = Some(x),
                    Err(e) => {
                        crate::log::line(&format!("[voice] нет аудио-выхода: {e}"));
                        return false;
                    }
                }
            }
            let handle = &out.as_ref().unwrap().1;
            let sink = match rodio::Sink::try_new(handle) {
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
            true
        });
        if !queued {
            return false;
        }
        loop {
            let done = {
                let g = self.current.lock().unwrap();
                g.as_ref().map(|s| s.empty()).unwrap_or(true)
            };
            if done {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let interrupted = self.current.lock().unwrap().is_none();
        *self.current.lock().unwrap() = None;
        // дать железу доиграть последние сэмплы — иначе хвост слова обрывается
        if !interrupted {
            std::thread::sleep(Duration::from_millis(80));
        }
        !interrupted
    }

    fn stop(&self) {
        if let Some(s) = self.current.lock().unwrap().take() {
            s.stop();
        }
    }
}
