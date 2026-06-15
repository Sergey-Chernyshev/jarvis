# Increment 7 — Voice (Phase 2: Silero sidecar) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** Add the **Silero** TTS engine behind the existing `TtsEngine` trait — a local Python sidecar (FastAPI, Silero model in memory, CPU PyTorch) that the daemon supervises and talks to over localhost HTTP. Switching `voice.engine` between `"piper"` and `"silero"` (+ daemon restart) changes the voice with no code change.

**Architecture:** A Python FastAPI sidecar (`POST /tts {text,speaker,sample_rate} → WAV`, `GET /health`) loads the Silero `ru` model once and stays resident. The daemon owns a **supervisor** that spawns the venv'd sidecar on startup (only when `engine="silero"` and installed), health-checks it, and restarts on crash. A Rust `SileroEngine` (blocking `reqwest` client) replaces the Phase-1 `SileroStub`. Everything else (composer, queue, player, config, tray) is unchanged — symmetry was the point.

**Tech Stack:** Rust (`reqwest` blocking), Python (FastAPI + uvicorn + torch + numpy + Silero via `torch.hub`), the existing `~/.jarvis/` install conventions.

**Spec:** `docs/superpowers/specs/2026-06-15-increment-7-voice-tts-design.md` (Silero section).

**Phase-1 reuse:** `TtsEngine` trait, `VoiceSel`, `TtsError`, `build_engine`, the `Voice` service + worker, config (`voice.engine/speaker/sampleRate`), tray, fail-safe contract.

> **Verification reality:** The CI/sandbox cannot reach PyPI / `torch.hub` / model hosts, so a live audible test happens on the user's machine via `npm run setup` + restart. Rust parts are unit-tested with the sidecar **mocked** (closed port / fake server). Python sidecar + installer are verified by review + the user's run. **Live-verify steps are marked; per the spec, stop and report if Silero's real API diverges.**

---

## File Structure

- Modify `src-tauri/Cargo.toml` — add `reqwest` (blocking, rustls).
- Create `bin/silero-server.py` — the FastAPI sidecar (shipped in repo, installed to `~/.jarvis/silero/`).
- Modify `src-tauri/src/voice/engine.rs` — replace `SileroStub` with `SileroEngine` (HTTP client); `build_engine` wires it.
- Create `src-tauri/src/voice/sidecar.rs` — supervisor: spawn/health/restart/kill the Python process.
- Modify `src-tauri/src/voice/mod.rs` — `Voice` owns the sidecar supervisor; start on `engine="silero"`, stop on dispose.
- Modify `src-tauri/src/daemon.rs` — dispose sidecar on exit (hook into existing `Power::dispose`-style teardown in `main.rs`).
- Modify `src-tauri/src/bin/setup.rs` — `install_silero()` (venv + pip + model warm-download), extend `status`.
- Modify `README.md` — Silero now real; install weight; speakers.

---

### Task 1: Add `reqwest` (blocking)

**Files:** `src-tauri/Cargo.toml`

- [ ] **Step 1:** Add to `[dependencies]`:
```toml
reqwest = { version = "0.12", default-features = false, features = ["blocking", "json", "rustls-tls"] }
```
- [ ] **Step 2:** `cargo build --manifest-path src-tauri/Cargo.toml --bin jarvis` → PASS (rustls avoids OpenSSL).
- [ ] **Step 3:** Commit: `git add src-tauri/Cargo.toml src-tauri/Cargo.lock && git commit -m "build: add reqwest (blocking) for Silero sidecar client"`

---

### Task 2: Silero sidecar (`bin/silero-server.py`)

**Files:** Create `bin/silero-server.py`

- [ ] **Step 0 (LIVE-VERIFY at implementation, but write best-known now):** Confirm the Silero `torch.hub` load + `apply_tts` signature against current `snakers4/silero-models`. Expected: `model, _ = torch.hub.load('snakers4/silero-models','silero_tts',language='ru',speaker='v4_ru')`; `audio = model.apply_tts(text=..., speaker='baya', sample_rate=24000)` → 1-D float tensor in [-1,1]; v4_ru speakers: `aidar baya kseniya xenia eugene random`. **If the real API differs (v5 naming, return shape), stop and report — do not guess.**

- [ ] **Step 1:** Write `bin/silero-server.py`:
```python
#!/usr/bin/env python3
"""Silero TTS сайдкар Jarvis. Только localhost. Текст → WAV, модель в памяти.
Запуск: python silero-server.py --port N --speaker baya --model v4_ru"""
import argparse, io, wave, sys
import numpy as np
import torch
from fastapi import FastAPI, Response
from pydantic import BaseModel
import uvicorn

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, required=True)
ap.add_argument("--model", default="v4_ru")     # v4_ru | v5_ru (свериться на живой)
ap.add_argument("--speaker", default="baya")
args = ap.parse_args()

torch.set_num_threads(2)
device = torch.device("cpu")
model, _ = torch.hub.load("snakers4/silero-models", "silero_tts", language="ru", speaker=args.model)
model.to(device)
DEFAULT_SPEAKER = args.speaker

app = FastAPI()

class Req(BaseModel):
    text: str
    speaker: str | None = None
    sample_rate: int = 24000

@app.get("/health")
def health():
    return {"ok": True, "model": args.model}

@app.post("/tts")
def tts(r: Req):
    text = (r.text or "").strip() or "."
    spk = r.speaker or DEFAULT_SPEAKER
    sr = r.sample_rate if r.sample_rate in (8000, 24000, 48000) else 24000
    audio = model.apply_tts(text=text, speaker=spk, sample_rate=sr)
    pcm = (np.clip(audio.numpy(), -1.0, 1.0) * 32767).astype("<i2").tobytes()
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1); w.setsampwidth(2); w.setframerate(sr); w.writeframes(pcm)
    return Response(content=buf.getvalue(), media_type="audio/wav")

if __name__ == "__main__":
    uvicorn.run(app, host="127.0.0.1", port=args.port, log_level="warning")
```
- [ ] **Step 2:** Syntax check: `python3 -m py_compile bin/silero-server.py` → PASS (does not require torch installed).
- [ ] **Step 3:** Commit: `git add bin/silero-server.py && git commit -m "feat(voice): Silero FastAPI sidecar (text→WAV, localhost)"`

---

### Task 3: `SileroEngine` HTTP client (`engine.rs`)

Replace the `SileroStub` with a real client. Tests use a closed port (unreachable → fail-safe error) — no Python needed.

**Files:** Modify `src-tauri/src/voice/engine.rs`

- [ ] **Step 1: Write failing tests** (add to `engine.rs` test module):
```rust
#[test]
fn silero_unreachable_fails_safe() {
    // порт заведомо закрыт → синтез возвращает ошибку, не паникует
    let e = SileroEngine::new("http://127.0.0.1:0".into());
    assert!(!e.available());
    assert!(e.synthesize("привет", &VoiceSel{speaker:"baya".into(),voice_path:String::new(),sample_rate:24000}).is_err());
}

#[test]
fn build_engine_silero_is_silero() {
    assert_eq!(build_engine("silero", std::path::PathBuf::from("/x")).name(), "silero");
}
```
- [ ] **Step 2:** Run → FAIL (`SileroEngine` doesn't exist; `build_engine("silero")` still returns the stub).
- [ ] **Step 3: Implement.** Remove `SileroStub`; add:
```rust
use std::time::Duration;

/// Клиент к Silero-сайдкару (localhost HTTP). Fail-safe при недоступности.
pub struct SileroEngine { base: String }
impl SileroEngine {
    pub fn new(base: String) -> Self { SileroEngine { base } }
    fn client(timeout: Duration) -> Result<reqwest::blocking::Client, TtsError> {
        reqwest::blocking::Client::builder().timeout(timeout).build()
            .map_err(|e| TtsError::Synthesis(format!("http client: {e}")))
    }
}
impl TtsEngine for SileroEngine {
    fn synthesize(&self, text: &str, voice: &VoiceSel) -> Result<Vec<u8>, TtsError> {
        let client = Self::client(Duration::from_secs(20))?;
        let resp = client.post(format!("{}/tts", self.base))
            .json(&serde_json::json!({ "text": text, "speaker": voice.speaker, "sample_rate": voice.sample_rate }))
            .send().map_err(|e| TtsError::Synthesis(format!("сайдкар недоступен: {e}")))?;
        if !resp.status().is_success() {
            return Err(TtsError::Synthesis(format!("сайдкар rc={}", resp.status())));
        }
        let bytes = resp.bytes().map_err(|e| TtsError::Synthesis(format!("чтение WAV: {e}")))?;
        if bytes.is_empty() { return Err(TtsError::Synthesis("пустой WAV".into())); }
        Ok(bytes.to_vec())
    }
    fn warmup(&self, voice: &VoiceSel) { let _ = self.synthesize("Готово.", voice); }
    fn available(&self) -> bool {
        Self::client(Duration::from_millis(800))
            .and_then(|c| c.get(format!("{}/health", self.base)).send()
                .map_err(|e| TtsError::Synthesis(e.to_string())))
            .map(|r| r.status().is_success()).unwrap_or(false)
    }
    fn name(&self) -> &'static str { "silero" }
}
```
Update `build_engine` to take the sidecar base URL for silero. Since `build_engine(engine, piper_bin)` currently only has the piper path, **change its signature** to `build_engine(engine: &str, piper_bin: PathBuf, silero_base: String) -> Box<dyn TtsEngine>`:
```rust
pub fn build_engine(engine: &str, piper_bin: PathBuf, silero_base: String) -> Box<dyn TtsEngine> {
    match engine {
        "silero" => Box::new(SileroEngine::new(silero_base)),
        _ => Box::new(PiperEngine::new(piper_bin)),
    }
}
```
Update the test `build_engine_selects_by_name` and the `mod.rs` caller accordingly (Task 5). The silero base is `http://127.0.0.1:<port>` (port from Task 5).
- [ ] **Step 4:** Run `cargo test ... voice::engine` → PASS.
- [ ] **Step 5:** Commit: `git add src-tauri/src/voice/engine.rs && git commit -m "feat(voice): SileroEngine HTTP client replaces stub"`

---

### Task 4: Sidecar supervisor (`sidecar.rs`)

Owns the Python child process: spawn, health-wait, restart-on-crash, kill on dispose. No live Python in tests — test the pure bits (port pick, command build) and the "binary missing → no spawn, fail-safe" path.

**Files:** Create `src-tauri/src/voice/sidecar.rs`; modify `mod.rs` (`pub mod sidecar;`)

- [ ] **Step 1:** Implement:
```rust
//! Супервизор Silero-сайдкара: запускает venv-python с server.py, ждёт /health,
//! перезапускает при падении, гасит на выходе. Всё fail-safe.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

pub struct Sidecar {
    py: PathBuf,        // ~/.jarvis/silero/venv/bin/python
    script: PathBuf,    // ~/.jarvis/silero/silero-server.py
    speaker: String,
    model: String,
    pub port: u16,
    child: Mutex<Option<Child>>,
}

impl Sidecar {
    pub fn new(dir: PathBuf, speaker: String, model: String, port: u16) -> Self {
        Sidecar {
            py: dir.join("venv").join("bin").join("python"),
            script: dir.join("silero-server.py"),
            speaker, model, port, child: Mutex::new(None),
        }
    }
    pub fn installed(&self) -> bool { self.py.exists() && self.script.exists() }
    pub fn base(&self) -> String { format!("http://127.0.0.1:{}", self.port) }

    /// Запустить, если установлен и ещё не запущен. Не блокирует на загрузке модели.
    pub fn ensure_started(&self) {
        if !self.installed() { crate::log::line("[voice] silero: сайдкар не установлен"); return; }
        let mut g = self.child.lock().unwrap();
        if g.as_mut().map(|c| c.try_wait().ok().flatten().is_none()).unwrap_or(false) { return; } // жив
        match Command::new(&self.py).arg(&self.script)
            .arg("--port").arg(self.port.to_string())
            .arg("--speaker").arg(&self.speaker)
            .arg("--model").arg(&self.model)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => { *g = Some(c); crate::log::line(&format!("[voice] silero: сайдкар запущен на :{}", self.port)); }
            Err(e) => crate::log::line(&format!("[voice] silero: не запустился: {e}")),
        }
    }

    /// Перезапуск, если процесс умер (вызывается тиком супервизора).
    pub fn restart_if_dead(&self) {
        let dead = { let mut g = self.child.lock().unwrap();
            g.as_mut().map(|c| c.try_wait().ok().flatten().is_some()).unwrap_or(true) };
        if dead { self.ensure_started(); }
    }

    pub fn stop(&self) {
        if let Some(mut c) = self.child.lock().unwrap().take() { let _ = c.kill(); let _ = c.wait(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn not_installed_when_paths_missing() {
        let s = Sidecar::new(PathBuf::from("/nope"), "baya".into(), "v4_ru".into(), 8731);
        assert!(!s.installed());
        s.ensure_started(); // не паникует, просто лог
        assert_eq!(s.base(), "http://127.0.0.1:8731");
    }
}
```
- [ ] **Step 2:** `cargo test ... voice::sidecar` → PASS.
- [ ] **Step 3:** Commit: `git add src-tauri/src/voice/sidecar.rs src-tauri/src/voice/mod.rs && git commit -m "feat(voice): Silero sidecar supervisor (spawn/health/restart/kill)"`

---

### Task 5: Wire sidecar into `Voice` + supervisor tick

**Files:** Modify `src-tauri/src/voice/mod.rs`, `src-tauri/src/main.rs`

- [ ] **Step 1:** In `Voice`, add `sidecar: Option<Arc<sidecar::Sidecar>>`. In `Voice::new(cfg, piper_bin)`: pick a fixed port (e.g. `8731`); if `cfg.engine == "silero"`, build a `Sidecar` from `jarvis_dir/silero` + `cfg.speaker` (default "baya") + model (`"v4_ru"`), call `ensure_started()`, set `silero_base = sidecar.base()`. Pass `silero_base` into `build_engine(&cfg.engine, piper_bin, silero_base)`. Store the `Arc<Sidecar>` so the supervisor tick and `dispose` can reach it. Expose `pub fn dispose(&self)` → `sidecar.stop()` and `pub fn tick(&self)` → `sidecar.restart_if_dead()` (no-ops when `sidecar` is None).
   - Note: `jarvis_dir()` isn't in `voice/`; pass the sidecar dir in from the daemon (the daemon already passes `piper_bin`). Change `Voice::new` to also accept `silero_dir: PathBuf`. Update the daemon caller (it has `jarvis_dir()`): `Voice::new(&vcfg, jarvis_dir().join("piper").join("piper"), jarvis_dir().join("silero"))`.
- [ ] **Step 2:** Supervisor tick: in `main.rs::spawn_timers`, add a loop (every 5s) calling `d.voice.tick()` (restart sidecar if it died). Dispose on exit: in `main.rs` `RunEvent::Exit` handler (next to `power::Power::dispose(&d)`), add `d.voice.dispose()`.
- [ ] **Step 3:** `cargo build ... --bin jarvis` → PASS; `cargo test ... voice` → PASS.
- [ ] **Step 4:** Commit: `git add src-tauri/src/voice/mod.rs src-tauri/src/main.rs && git commit -m "feat(voice): supervise Silero sidecar from Voice (tick + dispose)"`

---

### Task 6: Installer — venv + torch + Silero + model (`setup.rs`)

**Files:** Modify `src-tauri/src/bin/setup.rs`

- [ ] **Step 0 (LIVE-VERIFY, spec-mandated):** Confirm on the target machine: `python3` ≥3.9 available; `python3 -m venv` works; pip can install `torch` (CPU wheel), `fastapi`, `uvicorn`, `numpy`. **If torch has no CPU wheel for this Python/arch or pip is blocked, stop and report** with the exact failure and options (pin Python version, use `--index-url https://download.pytorch.org/whl/cpu`, etc.). Do not fabricate a working install.

- [ ] **Step 1:** Add `fn install_silero() -> Result<(), String>`: create `~/.jarvis/silero/`; copy the shipped `silero-server.py` (via `include_str!("../../../bin/silero-server.py")`, atomic write) into it; create venv `~/.jarvis/silero/venv` (`python3 -m venv`); `pip install --upgrade pip` then `pip install torch --index-url https://download.pytorch.org/whl/cpu fastapi uvicorn numpy` (idempotent — skip if `venv/bin/python` already imports torch); **explicitly print the install weight** ("Silero: ставлю PyTorch CPU — это сотни МБ–ГБ, один раз"). Warm-download the model once: run `venv/bin/python -c "import torch; torch.hub.load('snakers4/silero-models','silero_tts',language='ru',speaker='v4_ru')"`. Return `Err` (non-fatal) on any failure.
- [ ] **Step 2:** Hook into `install` non-fatally (after Piper):
```rust
match install_silero() {
    Ok(()) => println!("✓ Silero установлен (~/.jarvis/silero/venv + модель)"),
    Err(e) => eprintln!("⚠ Silero не установлен ({e}); engine=\"silero\" будет молчать, демон не затронут"),
}
```
- [ ] **Step 3:** Extend `status`: silero line → installed = `venv/bin/python` + `silero-server.py` present; active = `voice.engine=="silero"`.
- [ ] **Step 4:** `cargo build ... --bin jarvis-setup` → PASS; `jarvis-setup -- status` prints silero line without panic.
- [ ] **Step 5:** Commit: `git add src-tauri/src/bin/setup.rs && git commit -m "feat(setup): install Silero sidecar (venv + torch + model)"`

---

### Task 7: README + full sweep

**Files:** Modify `README.md`

- [ ] **Step 1:** `cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis` and `--bin jarvis-setup` → PASS.
- [ ] **Step 2:** Update the README "Голос (TTS)" section: Silero is now real (engine="silero" + restart); speakers (aidar/baya/kseniya/xenia/eugene); install weight warning; how to switch; that the sidecar is auto-started/supervised by the daemon and only listens on localhost.
- [ ] **Step 3:** Commit: `git add README.md && git commit -m "docs: Silero engine — install weight, speakers, switching"`

---

## Self-Review

**Spec coverage (Silero items):** sidecar (Python, localhost, model in memory) → Task 2 ✓; SileroEngine client + timeout + fail-safe → Task 3 ✓; managed process (start/stop with daemon, health, restart) → Tasks 4–5 ✓; installer (venv + torch + model, explicit weight log, non-fatal) → Task 6 ✓; status per engine → Task 6 ✓; config switch piper↔silero → Task 5 (build_engine) ✓; symmetry (shared composer/queue/player untouched) ✓; README tradeoff → Task 7 ✓.

**Acceptance scenarios (now reachable):** 1 (silero board phrase), 2 (switch piper→silero, no code change), 7 (sidecar down → daemon alive, switch to piper restores), 8 (A/B quality). Scenarios 3/4/5/6/9/10 already covered in Phase 1 (engine-agnostic).

**Placeholder scan:** the two Step-0 LIVE-VERIFY markers (Silero `torch.hub` API; torch CPU-wheel install) are spec-mandated stop-on-divergence checks, not hand-waving — the best-known commands are written out.

**Type consistency:** `build_engine` signature changes to `(engine, piper_bin, silero_base)` in Task 3 and the `mod.rs` caller is updated in Task 5; `Voice::new` gains a `silero_dir` param in Task 5 with the daemon caller updated. `Sidecar::{new,installed,base,ensure_started,restart_if_dead,stop}` used consistently across Tasks 4–5. `SileroEngine::new(base)` matches the `build_engine` wiring.
