//! Потоковый захват реплики с VAD-эндпойнтингом поверх `AudioHub::subscribe_wake`.
//! 80мс-кадры (FRAME_LEN=1280 @16к). ПОЛУДУПЛЕКС: звать, только когда Джарвис
//! молчит (иначе VAD услышит его собственный TTS — эха-подавления нет, это веха 2c).

use std::sync::Arc;
use std::time::Duration;

use crate::convo::vad::{rms, Endpointer, Step};
use crate::stt::hub::AudioHub;

pub enum ListenResult {
    /// Накопленный PCM реплики (16к моно) — на STT.
    Utterance(Vec<f32>),
    /// Никто не заговорил за окно ожидания / источник закрылся → конец разговора.
    Silence,
}

/// Слушать одну реплику. `max_wait_frames` — сколько 80мс-кадров ждать НАЧАЛА
/// речи (≈ секунды × 12.5) перед тем как считать тишиной. Калибровка шумового
/// пола и трейлинг-тишина — внутри `Endpointer` (дефолты тут).
pub fn listen(hub: &Arc<AudioHub>, max_wait_frames: u32) -> ListenResult {
    let tap = hub.subscribe_wake();
    // calib 5 кадров (~400мс), трейлинг 10 (~800мс) — старт-дефолты, тюнинг по мику.
    let mut ep = Endpointer::new(5, 10, max_wait_frames);
    let mut buf: Vec<f32> = Vec::new();
    loop {
        // recv_timeout, чтобы не зависнуть навсегда, если источник заглох
        let Some(frame) = tap.recv_timeout(Duration::from_millis(500)) else {
            return ListenResult::Silence;
        };
        match ep.push(rms(&frame)) {
            Step::Speaking => buf.extend_from_slice(&frame),
            Step::Done => return ListenResult::Utterance(buf),
            Step::Timeout => return ListenResult::Silence,
            Step::Calibrating | Step::Waiting => {}
        }
    }
    // tap дропается здесь (Drop отписывает от хаба)
}
