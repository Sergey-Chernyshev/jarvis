//! Чистый VAD-эндпойнтер: энергия кадра (RMS) + автомат начала/конца реплики.
//! Без I/O — кадры подаёт listen.rs из потока AudioHub. Порог адаптивный
//! (калибровка по шумовому полу первых кадров), т.к. в пайплайне нет AGC.

/// Среднеквадратичная энергия кадра (0.0 на пустом).
pub fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum: f32 = frame.iter().map(|x| x * x).sum();
    (sum / frame.len() as f32).sqrt()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Calibrating, // копим шумовой пол
    Waiting,     // ждём начало речи
    Speaking,    // идёт реплика (кадр писать в буфер)
    Done,        // реплика закончилась (трейлинг-тишина)
    Timeout,     // речь так и не началась за max_wait
}

/// Автомат эндпойнтинга. Кадры (их RMS) подаются по одному через `push`.
pub struct Endpointer {
    calib_left: u32,
    noise: f32,
    trailing_need: u32,
    trailing: u32,
    max_wait: u32,
    waited: u32,
    started: bool,
}

impl Endpointer {
    pub fn new(calib_frames: u32, trailing_silence_frames: u32, max_wait_frames: u32) -> Self {
        Self {
            calib_left: calib_frames.max(1),
            noise: 0.0,
            trailing_need: trailing_silence_frames.max(1),
            trailing: 0,
            max_wait: max_wait_frames.max(1),
            waited: 0,
            started: false,
        }
    }

    /// Порог старта речи над шумовым полом (3× пол, но не ниже 0.01).
    fn threshold(&self) -> f32 {
        (self.noise * 3.0).max(0.01)
    }

    pub fn push(&mut self, energy: f32) -> Step {
        if self.calib_left > 0 {
            self.noise = (self.noise + energy) / 2.0;
            self.calib_left -= 1;
            return Step::Calibrating;
        }
        let thr = self.threshold();
        if !self.started {
            if energy >= thr {
                self.started = true;
                return Step::Speaking;
            }
            self.waited += 1;
            return if self.waited >= self.max_wait { Step::Timeout } else { Step::Waiting };
        }
        // в речи: считаем трейлинг-тишину для эндпойнта
        if energy < thr {
            self.trailing += 1;
            if self.trailing >= self.trailing_need {
                return Step::Done;
            }
        } else {
            self.trailing = 0;
        }
        Step::Speaking
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(level: f32, n: usize) -> Vec<f32> {
        vec![level; n]
    }

    #[test]
    fn rms_of_constant_is_level() {
        assert!((rms(&frame(0.5, 100)) - 0.5).abs() < 1e-6);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn endpoints_after_speech_then_trailing_silence() {
        let mut ep = Endpointer::new(2, 3, 50);
        assert_eq!(ep.push(rms(&frame(0.001, 10))), Step::Calibrating);
        assert_eq!(ep.push(rms(&frame(0.001, 10))), Step::Calibrating);
        // громкая речь — старт
        assert_eq!(ep.push(rms(&frame(0.3, 10))), Step::Speaking);
        assert_eq!(ep.push(rms(&frame(0.3, 10))), Step::Speaking);
        // тишина: 3 кадра трейлинга → Done
        assert_eq!(ep.push(rms(&frame(0.001, 10))), Step::Speaking);
        assert_eq!(ep.push(rms(&frame(0.001, 10))), Step::Speaking);
        assert_eq!(ep.push(rms(&frame(0.001, 10))), Step::Done);
    }

    #[test]
    fn times_out_if_no_speech() {
        let mut ep = Endpointer::new(1, 3, 4);
        assert_eq!(ep.push(0.001), Step::Calibrating);
        for _ in 0..3 {
            assert_eq!(ep.push(0.001), Step::Waiting);
        }
        assert_eq!(ep.push(0.001), Step::Timeout);
    }

    #[test]
    fn trailing_resets_on_renewed_speech() {
        let mut ep = Endpointer::new(1, 3, 50);
        ep.push(0.001); // calib
        assert_eq!(ep.push(0.3), Step::Speaking); // старт
        assert_eq!(ep.push(0.001), Step::Speaking); // trailing 1
        assert_eq!(ep.push(0.3), Step::Speaking); // снова речь → reset
        assert_eq!(ep.push(0.001), Step::Speaking); // trailing 1 заново
        assert_eq!(ep.push(0.001), Step::Speaking); // 2
        assert_eq!(ep.push(0.001), Step::Done); // 3 → конец
    }
}
