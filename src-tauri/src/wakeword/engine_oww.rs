//! openWakeWord через `ort` (ONNX Runtime, нативно) — реальный детектор фразы.
//! За фичей `wakeword-ort` (по умолчанию ВЫКЛ): дефолтная сборка не тянет
//! onnxruntime и компилируется офлайн.
//!
//! Конвейер из 3 ступеней (ресёрч openwakeword-pipeline; шаги на 80мс-кадр @16к):
//!   1) melspectrogram.onnx: вход 'input' f32 [1,N] (int16-значения как f32) на
//!      последних 1760=1280+3·160 сэмплах → [8,32]; применяем x/10+2.
//!   2) embedding_model.onnx: вход 'input_1' f32 [1,76,32,1] на mel[-76:] → [96].
//!   3) <word>.onnx (hey_jarvis): вход [1,16,96] на emb[-16:] → скаляр 0..1.
//! Прогрев первых 5 кадров — на стороне сервиса (`Detector::WARMUP_FRAMES`).
//!
//! ВНИМАНИЕ (версии): `ort` запинен `=2.0.0-rc.10` (совместимость с будущим
//! `voice_activity_detector`, у которого `links=onnxruntime` — одна версия ort на
//! бинарь). API rc-линии меняется между rc; при сборке с фичей валидировать
//! сигнатуры `Session`/`inputs!`/`try_extract_tensor` против запиненной версии.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use ort::session::Session;
use ort::value::Tensor;

use super::engine::WakeWordEngine;

const SAMPLES_PER_CHUNK: usize = 1280; // 80 мс @16к
const MEL_LOOKBACK: usize = 3 * 160; // дополнительные 480 сэмплов под стабильные 8 mel-кадров
const MEL_BINS: usize = 32;
const MEL_WINDOW: usize = 76; // кадров mel на одно окно эмбеддера
const EMB_DIM: usize = 96;
const CLF_WINDOW: usize = 16; // эмбеддингов на классификатор
const MEL_BUF_CAP: usize = 970;
const EMB_BUF_CAP: usize = 120;

pub struct OwwEngine {
    melspec: Session,
    embed: Session,
    clf: Session,
    clf_input_name: String,
    raw_tail: Vec<f32>,        // хвост сырых сэмплов (для lookback мелспека)
    mel: VecDeque<[f32; MEL_BINS]>, // кольцо mel-кадров
    emb: VecDeque<[f32; EMB_DIM]>,  // кольцо эмбеддингов
}

impl OwwEngine {
    /// Загрузить 3 модели. `model` — "hey_jarvis" (бандл) или абсолютный путь к
    /// классификатору; мел/эмбеддер берутся из той же папки (общие для всех слов).
    pub fn load(model: &str) -> Result<OwwEngine, String> {
        let dir = super::models_dir();
        let clf_path = resolve_classifier(&dir, model)?;
        let mel_path = dir.join("melspectrogram.onnx");
        let emb_path = dir.join("embedding_model.onnx");
        for p in [&mel_path, &emb_path, &clf_path] {
            if !p.exists() {
                return Err(format!("нет модели: {}", p.display()));
            }
        }
        let melspec = build_session(&mel_path)?;
        let embed = build_session(&emb_path)?;
        let clf = build_session(&clf_path)?;
        let clf_input_name = clf
            .inputs
            .first()
            .map(|i| i.name.clone())
            .unwrap_or_else(|| "input".to_string());
        Ok(OwwEngine {
            melspec,
            embed,
            clf,
            clf_input_name,
            raw_tail: Vec::with_capacity(SAMPLES_PER_CHUNK + MEL_LOOKBACK),
            mel: VecDeque::with_capacity(MEL_BUF_CAP + 8),
            emb: VecDeque::with_capacity(EMB_BUF_CAP + 1),
        })
    }

    /// Прогнать один 1280-сэмпловый кадр через конвейер; вернуть скор или None,
    /// пока окна не набрались.
    fn step(&mut self, frame: &[f32]) -> Result<Option<f32>, String> {
        // 1) мелспектр на последних 1760 сэмплах (lookback из raw_tail).
        let mut buf = std::mem::take(&mut self.raw_tail);
        buf.extend_from_slice(frame);
        let need = SAMPLES_PER_CHUNK + MEL_LOOKBACK;
        let window: Vec<f32> = if buf.len() >= need {
            buf[buf.len() - need..].to_vec()
        } else {
            buf.clone()
        };
        // сохранить хвост (последние need-1280 = 480) под следующий кадр
        let keep = MEL_LOOKBACK.min(buf.len());
        self.raw_tail = buf[buf.len() - keep..].to_vec();

        // int16-значения как f32 (модель обучена на int16 PCM, кастованном в float).
        // Масштаб 32767 (i16::MAX) — симметричный инверс нормализации [-1,1].
        // ВРЕМЕННО: входной гейн (JARVIS_WAKE_GAIN) для проверки гипотезы «тихий вход».
        let gain: f32 = std::env::var("JARVIS_WAKE_GAIN").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
        let mel_in: Vec<f32> = window
            .iter()
            .map(|&s| (s * gain).clamp(-1.0, 1.0) * i16::MAX as f32)
            .collect();
        let n = mel_in.len() as i64;
        let mel_out = run_flat(&mut self.melspec, "input", vec![1, n], mel_in)?;
        // [frames,32], трансформ x/10+2
        let frames = mel_out.len() / MEL_BINS;
        if std::env::var_os("JARVIS_WAKE_DEBUG").is_some() {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 13 == 0 {
                crate::log::line(&format!(
                    "[oww:dbg] окно_сэмплов={} mel_out_len={} кадров/чанк={} mel_buf={} emb_buf={}",
                    window.len(), mel_out.len(), frames, self.mel.len(), self.emb.len()
                ));
            }
        }
        for fr in 0..frames {
            let mut row = [0f32; MEL_BINS];
            for b in 0..MEL_BINS {
                row[b] = mel_out[fr * MEL_BINS + b] / 10.0 + 2.0;
            }
            self.mel.push_back(row);
        }
        while self.mel.len() > MEL_BUF_CAP {
            self.mel.pop_front();
        }
        if self.mel.len() < MEL_WINDOW {
            return Ok(None);
        }

        // 2) эмбеддер на mel[-76:] → [96]
        let start = self.mel.len() - MEL_WINDOW;
        let mut emb_in = Vec::with_capacity(MEL_WINDOW * MEL_BINS);
        for i in start..self.mel.len() {
            emb_in.extend_from_slice(&self.mel[i]);
        }
        let emb_out = run_flat(
            &mut self.embed,
            "input_1",
            vec![1, MEL_WINDOW as i64, MEL_BINS as i64, 1],
            emb_in,
        )?;
        if emb_out.len() < EMB_DIM {
            return Err("эмбеддер вернул < 96".into());
        }
        let mut e = [0f32; EMB_DIM];
        e.copy_from_slice(&emb_out[emb_out.len() - EMB_DIM..]);
        self.emb.push_back(e);
        while self.emb.len() > EMB_BUF_CAP {
            self.emb.pop_front();
        }
        if self.emb.len() < CLF_WINDOW {
            return Ok(None);
        }

        // 3) классификатор на emb[-16:] → скаляр
        let start = self.emb.len() - CLF_WINDOW;
        let mut clf_in = Vec::with_capacity(CLF_WINDOW * EMB_DIM);
        for i in start..self.emb.len() {
            clf_in.extend_from_slice(&self.emb[i]);
        }
        let name = self.clf_input_name.clone();
        let out = run_flat(&mut self.clf, &name, vec![1, CLF_WINDOW as i64, EMB_DIM as i64], clf_in)?;
        if std::env::var_os("JARVIS_WAKE_DEBUG").is_some() {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            static MAX_PK: AtomicU32 = AtomicU32::new(0);
            static MAX_SC: AtomicU32 = AtomicU32::new(0);
            let fpk = frame.iter().fold(0f32, |a, &s| a.max(s.abs()));
            let sc = out.first().copied().unwrap_or(0.0);
            // running max over окно (peak/score), b/c per-call snapshot misses loud moments
            if fpk > f32::from_bits(MAX_PK.load(Ordering::Relaxed)) {
                MAX_PK.store(fpk.to_bits(), Ordering::Relaxed);
            }
            if sc > f32::from_bits(MAX_SC.load(Ordering::Relaxed)) {
                MAX_SC.store(sc.to_bits(), Ordering::Relaxed);
            }
            let n = N.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 13 == 0 {
                let mpk = f32::from_bits(MAX_PK.swap(0, Ordering::Relaxed));
                let msc = f32::from_bits(MAX_SC.swap(0, Ordering::Relaxed));
                let (elo, ehi) = self.emb.back().map(|e| {
                    e.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)))
                }).unwrap_or((0.0, 0.0));
                crate::log::line(&format!(
                    "[oww:num] МАКС_peak за 1с={mpk:.4} МАКС_score={msc:.5} emb=[{elo:.2}..{ehi:.2}]"
                ));
            }
        }
        Ok(out.first().copied())
    }
}

impl WakeWordEngine for OwwEngine {
    fn name(&self) -> &str {
        "openwakeword"
    }
    fn push_frame(&mut self, frame: &[f32]) -> Option<f32> {
        match self.step(frame) {
            Ok(s) => s,
            Err(e) => {
                crate::log::line(&format!("[wake] инференс: {e}"));
                Some(0.0) // fail-safe: ошибка инференса не будит
            }
        }
    }
    fn reset(&mut self) {
        self.raw_tail.clear();
        self.mel.clear();
        self.emb.clear();
    }
}

fn resolve_classifier(dir: &Path, model: &str) -> Result<PathBuf, String> {
    if model == "hey_jarvis" || model.is_empty() {
        Ok(dir.join("hey_jarvis_v0.1.onnx"))
    } else {
        let p = PathBuf::from(model);
        if p.is_absolute() {
            Ok(p)
        } else {
            Ok(dir.join(model))
        }
    }
}

fn build_session(path: &Path) -> Result<Session, String> {
    Session::builder()
        .map_err(|e| format!("session builder: {e}"))?
        .with_intra_threads(1)
        .map_err(|e| format!("intra threads: {e}"))?
        .commit_from_file(path)
        .map_err(|e| format!("commit {}: {e}", path.display()))
}

/// Прогнать сессию с одним входом (динамическая форма) и вернуть плоский f32-выход.
fn run_flat(
    session: &mut Session,
    input_name: &str,
    shape: Vec<i64>,
    data: Vec<f32>,
) -> Result<Vec<f32>, String> {
    let in_shape_dbg = if std::env::var_os("JARVIS_WAKE_DEBUG").is_some() { shape.clone() } else { Vec::new() };
    let tensor = Tensor::from_array((shape, data)).map_err(|e| format!("tensor: {e}"))?;
    let outputs = session
        .run(ort::inputs![input_name => tensor])
        .map_err(|e| format!("run: {e}"))?;
    let (out_shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("extract: {e}"))?;
    if std::env::var_os("JARVIS_WAKE_DEBUG").is_some() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        if N.fetch_add(1, Ordering::Relaxed) < 40 {
            crate::log::line(&format!(
                "[oww:shape] in='{input_name}' in_shape={in_shape_dbg:?} out_shape={out_shape:?} out_len={}",
                data.len()
            ));
        }
    }
    Ok(data.to_vec())
}
