# Increment 7 — Voice (Phase 1: core + Piper) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Jarvis speaks a short Russian sentence after a session event (Stop / Notification / StopFailure), using a local Piper TTS engine behind a swappable `TtsEngine` trait, with a serialized priority speech queue and a template phrase composer.

**Architecture:** New `src-tauri/src/voice/` module. Engine-agnostic core (composer → queue → player) is decoupled from the engine via the `TtsEngine` trait. Phase 1 ships `PiperEngine` (subprocess) + a `SileroEngine` stub (so config `engine="silero"` fails safe). The daemon builds `SpeechSignals` from existing session state and calls `Voice::speak`. All TTS is a fail-safe side effect — never on the event critical path.

**Tech Stack:** Rust, Tauri 2, `rodio` (audio playback), `hound` (WAV decode if needed), `serde_json` (config in existing `settings.json`), Piper (`piper` binary subprocess).

**Spec:** `docs/superpowers/specs/2026-06-15-increment-7-voice-tts-design.md`

---

## File Structure

- Create `src-tauri/src/voice/mod.rs` — `Voice` service: owns config+engine+composer+queue+player; `speak(signals)`, `warmup()`, `set_mute()`, `test_voice()`.
- Create `src-tauri/src/voice/numerals.rs` — Russian numeral/units agreement (pure).
- Create `src-tauri/src/voice/composer.rs` — `SpeechSignals`, `Utterance`, `Priority`, `Composer` trait, `TemplateComposer` (pure).
- Create `src-tauri/src/voice/queue.rs` — serialized priority speech queue (serialize, dedup, coalesce, interrupt).
- Create `src-tauri/src/voice/engine.rs` — `TtsEngine` trait, `TtsError`, `VoiceSel`, `PiperEngine`, `SileroEngine` stub, `build_engine(cfg)`.
- Create `src-tauri/src/voice/player.rs` — `rodio` playback, interruptible.
- Create `src-tauri/src/voice/config.rs` — `VoiceConfig` read from `settings.json` `voice` block + defaults.
- Modify `src-tauri/src/main.rs` — `mod voice;`.
- Modify `src-tauri/src/daemon.rs` — `Voice` field on `Daemon`, build `SpeechSignals` in stop/notification/stop-failure handling, warmup on startup.
- Modify `src-tauri/src/tray.rs` — "Без звука" check item + "Тест голоса" item.
- Modify `src-tauri/src/bin/setup.rs` — install `piper` + ru voice; `status` reports voice engines.
- Modify `src-tauri/Cargo.toml` — add `rodio`, `hound`.
- Modify `README.md` — voice section.

---

### Task 1: Add audio dependencies

**Files:**
- Modify: `src-tauri/Cargo.toml`

- [ ] **Step 1: Add deps**

In `[dependencies]` of `src-tauri/Cargo.toml`, add:

```toml
rodio = { version = "0.19", default-features = false, features = ["wav"] }
hound = "3"
```

- [ ] **Step 2: Verify it resolves/builds**

Run: `cargo build --manifest-path src-tauri/Cargo.toml --bin jarvis`
Expected: PASS (compiles; new crates fetched). If `rodio` 0.19 API differs at use-site, the player task pins the actual version found.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "build: add rodio + hound for TTS playback"
```

---

### Task 2: Russian numerals & units (`numerals.rs`)

Pure functions, fully unit-tested. Composer depends on these.

**Files:**
- Create: `src-tauri/src/voice/numerals.rs`

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/voice/numerals.rs`:

```rust
//! Русское согласование числительных и разворот единиц в слова — для речи.
//! TTS не умеет «2 задачи» прочитать правильно, поэтому числа разворачиваем сами.

/// Форма существительного по числу: (1, 2-4, 5+). Пример: ("задача","задачи","задач").
pub fn plural(n: i64, one: &str, few: &str, many: &str) -> &str {
    let n = n.abs() % 100;
    let d = n % 10;
    if (11..=14).contains(&n) { many } else if d == 1 { one } else if (2..=4).contains(&d) { few } else { many }
}

/// Число словами 0..=999 (этого хватает для задач/файлов/часов/минут).
pub fn number_words(n: i64) -> String {
    // реализация в Step 3
    let _ = n; String::new()
}

/// «одна задача» / «две задачи» / «пять задач» (число словами + согласование).
pub fn count_phrase(n: i64, one: &str, few: &str, many: &str) -> String {
    format!("{} {}", number_words(n), plural(n, one, few, many))
}

/// Длительность из минут словами: «два часа четырнадцать минут», «пять минут».
pub fn duration_words(total_min: i64) -> String {
    // реализация в Step 3
    let _ = total_min; String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plural_agreement() {
        assert_eq!(plural(1, "задача", "задачи", "задач"), "задача");
        assert_eq!(plural(2, "задача", "задачи", "задач"), "задачи");
        assert_eq!(plural(5, "задача", "задачи", "задач"), "задач");
        assert_eq!(plural(11, "задача", "задачи", "задач"), "задач");
        assert_eq!(plural(21, "задача", "задачи", "задач"), "задача");
    }

    #[test]
    fn numbers_to_words() {
        assert_eq!(number_words(0), "ноль");
        assert_eq!(number_words(1), "один");
        assert_eq!(number_words(2), "два");
        assert_eq!(number_words(14), "четырнадцать");
        assert_eq!(number_words(21), "двадцать один");
        assert_eq!(number_words(100), "сто");
        assert_eq!(number_words(246), "двести сорок шесть");
    }

    #[test]
    fn count_phrases() {
        assert_eq!(count_phrase(1, "задача", "задачи", "задач"), "одна задача");
        assert_eq!(count_phrase(3, "файл", "файла", "файлов"), "три файла");
        assert_eq!(count_phrase(5, "задача", "задачи", "задач"), "пять задач");
    }

    #[test]
    fn durations() {
        assert_eq!(duration_words(5), "пять минут");
        assert_eq!(duration_words(60), "один час");
        assert_eq!(duration_words(134), "два часа четырнадцать минут");
        assert_eq!(duration_words(120), "два часа");
    }
}
```

Note: `count_phrase(1, "задача", …)` must yield **«одна задача»** (feminine «одна», not «один»). `number_words` returns masculine forms by default; `count_phrase` needs gendered one/two. Handle in Step 3 via a feminine override: pass gender through, or special-case 1→«одна»/2→«две» when the noun is feminine. Implement `count_phrase` to accept the feminine variant.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis numerals`
Expected: FAIL (stubs return empty strings).

- [ ] **Step 3: Implement**

Replace the stubs with real implementations. `number_words` masculine units; `count_phrase` handles feminine «одна/две» by detecting the `one` form ending (or take an explicit `fem: bool`). Concrete approach — change signature to carry gender:

```rust
pub enum Gender { M, F }

pub fn number_words_gender(n: i64, g: Gender) -> String {
    let units_m = ["ноль","один","два","три","четыре","пять","шесть","семь","восемь","девять"];
    let units_f = ["ноль","одна","две","три","четыре","пять","шесть","семь","восемь","девять"];
    let teens = ["десять","одиннадцать","двенадцать","тринадцать","четырнадцать","пятнадцать","шестнадцать","семнадцать","восемнадцать","девятнадцать"];
    let tens = ["","","двадцать","тридцать","сорок","пятьдесят","шестьдесят","семьдесят","восемьдесят","девяносто"];
    let hundreds = ["","сто","двести","триста","четыреста","пятьсот","шестьсот","семьсот","восемьсот","девятьсот"];
    if n < 0 { return number_words_gender(-n, g); }
    if n == 0 { return "ноль".into(); }
    let mut parts: Vec<String> = Vec::new();
    let h = (n / 100) % 10;
    let t = (n / 10) % 10;
    let u = n % 10;
    if h > 0 { parts.push(hundreds[h as usize].into()); }
    if t == 1 {
        parts.push(teens[u as usize].into());
    } else {
        if t > 1 { parts.push(tens[t as usize].into()); }
        if u > 0 {
            let units = match g { Gender::M => &units_m, Gender::F => &units_f };
            parts.push(units[u as usize].into());
        }
    }
    parts.join(" ")
}

pub fn number_words(n: i64) -> String { number_words_gender(n, Gender::M) }
```

Update `count_phrase` to take gender (feminine for «задача», masculine for «файл»):

```rust
pub fn count_phrase(n: i64, g: Gender, one: &str, few: &str, many: &str) -> String {
    format!("{} {}", number_words_gender(n, g), plural(n, one, few, many))
}
```

Adjust the tests accordingly (pass `Gender::F` for «задача», `Gender::M` for «файл»).

`duration_words`:

```rust
pub fn duration_words(total_min: i64) -> String {
    let total_min = total_min.max(0);
    let h = total_min / 60;
    let m = total_min % 60;
    let mut parts = Vec::new();
    if h > 0 { parts.push(count_phrase(h, Gender::M, "час", "часа", "часов")); }
    if m > 0 || h == 0 { parts.push(count_phrase(m, Gender::F, "минута", "минуты", "минут")); }
    parts.join(" ")
}
```

Wait — «минута» is feminine → «одна минута/две минуты/пять минут», and `duration_words(5)` ⇒ «пять минут» ✓; `duration_words(134)` ⇒ «два часа четырнадцать минут» ✓. Update the durations test to use these forms.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis numerals`
Expected: PASS (all numerals tests).

- [ ] **Step 5: Register module & commit**

Add to `src-tauri/src/main.rs` (near other `mod` lines): the `voice` module is registered in Task 3; for now add `mod voice;` and create `src-tauri/src/voice/mod.rs` with `pub mod numerals;`.

```bash
git add src-tauri/src/voice/numerals.rs src-tauri/src/voice/mod.rs src-tauri/src/main.rs
git commit -m "feat(voice): russian numeral & duration agreement"
```

---

### Task 3: Phrase composer (`composer.rs`)

Pure `signals → Option<Utterance>`. The LLM seam = the `Composer` trait.

**Files:**
- Create: `src-tauri/src/voice/composer.rs`
- Modify: `src-tauri/src/voice/mod.rs` (add `pub mod composer;`)

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/voice/composer.rs`:

```rust
//! Композитор фразы: структурные сигналы → короткая русская строка.
//! Чистая функция за трейтом `Composer` — шов под будущую LLM-реализацию.

use crate::voice::numerals::{count_phrase, duration_words, Gender};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority { Done = 0, NeedHuman = 1 } // выше = важнее (NeedHuman вперёд Done)

#[derive(Debug, Clone)]
pub struct Utterance {
    pub text: String,
    pub priority: Priority,
    pub dedup_key: String,
    pub coalesce_group: Option<String>, // Some → сливается при заторе
}

#[derive(Debug, Clone, Copy)]
pub enum Event { Stop, Notification, StopFailure }

/// Всё, что композитор может использовать. Все поля — defensive Option.
#[derive(Debug, Clone, Default)]
pub struct SpeechSignals {
    pub event: Option<Event>,
    pub sid: String,
    pub project: String,
    pub board_done: Option<i64>,
    pub board_total: Option<i64>,
    pub board_active: Option<String>,   // текст активной задачи
    pub diff_files: Option<i64>,        // сколько файлов тронуто
    pub notification_text: Option<String>,
    pub limit_reset_min: Option<i64>,   // минут до сброса лимита
}

pub trait Composer: Send + Sync {
    fn compose(&self, s: &SpeechSignals) -> Option<Utterance>;
}

pub struct TemplateComposer;

impl Composer for TemplateComposer {
    fn compose(&self, s: &SpeechSignals) -> Option<Utterance> {
        // реализация в Step 3
        let _ = s; None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(event: Event, project: &str) -> SpeechSignals {
        SpeechSignals { event: Some(event), project: project.into(), sid: "s".into(), ..Default::default() }
    }

    #[test]
    fn stop_prefers_board() {
        let mut s = sig(Event::Stop, "Пиксела");
        s.board_done = Some(4); s.board_total = Some(6); s.board_active = Some("docker-compose".into());
        s.diff_files = Some(3); // доска должна победить diff
        let u = TemplateComposer.compose(&s).unwrap();
        assert!(u.text.contains("четыре из шести задач"), "{}", u.text);
        assert!(u.text.contains("docker-compose"), "{}", u.text);
        assert_eq!(u.priority, Priority::Done);
    }

    #[test]
    fn stop_falls_to_diff_then_fact() {
        let mut s = sig(Event::Stop, "Рекрю");
        s.diff_files = Some(3);
        assert_eq!(TemplateComposer.compose(&s).unwrap().text, "Рекрю готов, изменено три файла");
        let bare = sig(Event::Stop, "Тикетинг");
        assert_eq!(TemplateComposer.compose(&bare).unwrap().text, "Тикетинг закончил");
    }

    #[test]
    fn notification_is_need_human_and_highest() {
        let mut s = sig(Event::Notification, "Пиксела");
        s.notification_text = Some("нужно разрешение на Bash".into());
        let u = TemplateComposer.compose(&s).unwrap();
        assert_eq!(u.priority, Priority::NeedHuman);
        assert!(u.text.starts_with("Пиксела ждёт"), "{}", u.text);
        assert!(u.text.contains("Bash"), "{}", u.text);
        assert!(u.coalesce_group.is_none(), "ждёт не коалесцируется");
    }

    #[test]
    fn stop_failure_speaks_reset_in_words() {
        let mut s = sig(Event::StopFailure, "Пиксела");
        s.limit_reset_min = Some(134);
        let u = TemplateComposer.compose(&s).unwrap();
        assert_eq!(u.priority, Priority::NeedHuman);
        assert!(u.text.contains("упёрся в лимит"), "{}", u.text);
        assert!(u.text.contains("два часа четырнадцать минут"), "{}", u.text);
    }

    #[test]
    fn long_notification_truncated() {
        let mut s = sig(Event::Notification, "Пиксела");
        s.notification_text = Some("a".repeat(500));
        let u = TemplateComposer.compose(&s).unwrap();
        assert!(u.text.chars().count() <= 160, "len {}", u.text.chars().count());
    }

    #[test]
    fn stop_coalesces_done() {
        let s = sig(Event::Stop, "Рекрю");
        let u = TemplateComposer.compose(&s).unwrap();
        assert_eq!(u.coalesce_group.as_deref(), Some("stop-done"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis composer`
Expected: FAIL (`compose` returns `None`).

- [ ] **Step 3: Implement `TemplateComposer::compose`**

```rust
impl Composer for TemplateComposer {
    fn compose(&self, s: &SpeechSignals) -> Option<Utterance> {
        let project = if s.project.is_empty() { "Сессия" } else { &s.project };
        let trunc = |t: &str| -> String {
            let one = t.split(['.', '!', '?']).next().unwrap_or(t).trim();
            let chars: Vec<char> = one.chars().collect();
            if chars.len() > 140 { chars[..140].iter().collect::<String>() } else { one.to_string() }
        };
        match s.event? {
            Event::Notification => {
                let gist = s.notification_text.as_deref().map(trunc).filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "нужен ты".into());
                Some(Utterance {
                    text: format!("{project} ждёт — {gist}"),
                    priority: Priority::NeedHuman,
                    dedup_key: format!("notif:{}:{gist}", s.sid),
                    coalesce_group: None,
                })
            }
            Event::StopFailure => {
                let when = s.limit_reset_min.map(duration_words)
                    .map(|w| format!(", сброс через {w}")).unwrap_or_default();
                Some(Utterance {
                    text: format!("{project} упёрся в лимит{when}"),
                    priority: Priority::NeedHuman,
                    dedup_key: format!("limit:{}", s.sid),
                    coalesce_group: None,
                })
            }
            Event::Stop => {
                let text = match (s.board_done, s.board_total) {
                    (Some(done), Some(total)) if total > 0 => {
                        let head = format!("{project}: {} из {} задач",
                            count_phrase(done, Gender::F, "задача", "задачи", "задач").split(' ').next().unwrap_or(""),
                            crate::voice::numerals::number_words(total));
                        // «четыре из шести задач» — done словами + «из» + total словами + «задач»
                        let head = format!("{project}: {} из {} {}",
                            crate::voice::numerals::number_words(done),
                            crate::voice::numerals::number_words(total),
                            crate::voice::numerals::plural(total, "задача", "задачи", "задач"));
                        let _ = head; // (оставлен второй вариант)
                        match &s.board_active {
                            Some(a) if !a.is_empty() => format!("{project}: {} из {} {}, сейчас {a}",
                                crate::voice::numerals::number_words(done),
                                crate::voice::numerals::number_words(total),
                                crate::voice::numerals::plural(total, "задача", "задачи", "задач")),
                            _ => format!("{project}: {} из {} {}",
                                crate::voice::numerals::number_words(done),
                                crate::voice::numerals::number_words(total),
                                crate::voice::numerals::plural(total, "задача", "задачи", "задач")),
                        }
                    }
                    _ => match s.diff_files {
                        Some(n) if n > 0 => format!("{project} готов, изменено {}",
                            count_phrase(n, Gender::M, "файл", "файла", "файлов")),
                        _ => format!("{project} закончил"),
                    },
                };
                Some(Utterance { text, priority: Priority::Done, dedup_key: format!("stop:{}", s.sid), coalesce_group: Some("stop-done".into()) })
            }
        }
    }
}
```

Clean up the duplicated `head` lines above into the single working branch (the `let head … let _ = head;` scaffolding is just to show derivation — keep only the final `match &s.board_active` expression). Final `Stop`/board text format: `«{project}: {done словами} из {total словами} {задач}, сейчас {active}»`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis composer`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/voice/composer.rs src-tauri/src/voice/mod.rs
git commit -m "feat(voice): template phrase composer (board > diff > fact)"
```

---

### Task 4: Voice config (`config.rs`)

**Files:**
- Create: `src-tauri/src/voice/config.rs`
- Modify: `src-tauri/src/voice/mod.rs` (add `pub mod config;`)

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/voice/config.rs`:

```rust
//! voice-блок настроек из ~/.jarvis/settings.json. Битый/нет блока → дефолты.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct VoiceConfig {
    pub engine: String,         // "piper" | "silero"
    pub speaker: String,
    pub voice_path: String,
    pub sample_rate: u32,
    pub mute: bool,
    pub verbosity: String,      // "short" | "descriptive"
    pub ev_stop: bool,
    pub ev_notification: bool,
    pub ev_stop_failure: bool,
    pub ev_subagent_stop: bool,
    pub ev_session_end: bool,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        VoiceConfig {
            engine: "piper".into(), speaker: String::new(), voice_path: String::new(),
            sample_rate: 24000, mute: false, verbosity: "short".into(),
            ev_stop: true, ev_notification: true, ev_stop_failure: true,
            ev_subagent_stop: false, ev_session_end: false,
        }
    }
}

impl VoiceConfig {
    /// Распарсить из корневого settings-объекта (его поле "voice"). Дефолты на дыры.
    pub fn from_settings(root: &Value) -> Self {
        let d = VoiceConfig::default();
        let v = root.get("voice");
        let s = |k: &str, dv: &str| v.and_then(|v| v.get(k)).and_then(Value::as_str).unwrap_or(dv).to_string();
        let b = |k: &str, dv: bool| v.and_then(|v| v.get(k)).and_then(Value::as_bool).unwrap_or(dv);
        let ev = |k: &str, dv: bool| v.and_then(|v| v.get("events")).and_then(|e| e.get(k)).and_then(Value::as_bool).unwrap_or(dv);
        VoiceConfig {
            engine: s("engine", &d.engine),
            speaker: s("speaker", &d.speaker),
            voice_path: s("voicePath", &d.voice_path),
            sample_rate: v.and_then(|v| v.get("sampleRate")).and_then(Value::as_u64).unwrap_or(d.sample_rate as u64) as u32,
            mute: b("mute", d.mute),
            verbosity: s("verbosity", &d.verbosity),
            ev_stop: ev("stop", d.ev_stop),
            ev_notification: ev("notification", d.ev_notification),
            ev_stop_failure: ev("stopFailure", d.ev_stop_failure),
            ev_subagent_stop: ev("subagentStop", d.ev_subagent_stop),
            ev_session_end: ev("sessionEnd", d.ev_session_end),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_block_is_defaults() {
        assert_eq!(VoiceConfig::from_settings(&json!({})), VoiceConfig::default());
    }

    #[test]
    fn partial_block_merges_defaults() {
        let cfg = VoiceConfig::from_settings(&json!({ "voice": { "engine": "silero", "events": { "stop": false } } }));
        assert_eq!(cfg.engine, "silero");
        assert!(!cfg.ev_stop);
        assert!(cfg.ev_notification, "не заданное событие — дефолт вкл");
        assert_eq!(cfg.sample_rate, 24000);
    }

    #[test]
    fn garbage_types_fall_back() {
        let cfg = VoiceConfig::from_settings(&json!({ "voice": { "sampleRate": "oops", "mute": "yes" } }));
        assert_eq!(cfg.sample_rate, 24000);
        assert!(!cfg.mute);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis voice::config`
Expected: FAIL (until file compiles — it's complete here, so this is mostly a compile check; if it passes immediately that's fine, the value is the regression guard).

- [ ] **Step 3: Implement** — already complete in Step 1.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis voice::config`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/voice/config.rs src-tauri/src/voice/mod.rs
git commit -m "feat(voice): voice config block in settings.json"
```

---

### Task 5: Engine trait + Piper + Silero stub (`engine.rs`)

**Files:**
- Create: `src-tauri/src/voice/engine.rs`
- Modify: `src-tauri/src/voice/mod.rs` (add `pub mod engine;`)

- [ ] **Step 0 (VERIFY LIVE — spec-mandated): confirm Piper CLI before coding**

Run (if `piper` available): `echo "привет" | piper --model <ru.onnx> --output_file out.wav && file out.wav`
Expected: a WAV file. Confirm the real flags (`--model`, output to stdout via `--output_file -` or `--output-raw`). **If flags differ from below, stop and update the plan** (spec rule). If `piper` is not yet installed, defer exact-flag verification to Task 9 (installer) and keep the documented invocation; the engine must fail safe regardless.

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/voice/engine.rs`:

```rust
//! Трейт движка TTS + реализации. Piper — subprocess; Silero — заглушка (Фаза 2).
//! Любая ошибка движка — fail-safe: вернуть TtsError, демон не падает.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::io::Write;

#[derive(Debug)]
pub enum TtsError { NotInstalled(String), Synthesis(String) }

impl std::fmt::Display for TtsError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            TtsError::NotInstalled(s) => write!(f, "движок не установлен: {s}"),
            TtsError::Synthesis(s) => write!(f, "ошибка синтеза: {s}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct VoiceSel { pub speaker: String, pub voice_path: String, pub sample_rate: u32 }

pub trait TtsEngine: Send + Sync {
    fn synthesize(&self, text: &str, voice: &VoiceSel) -> Result<Vec<u8>, TtsError>;
    fn warmup(&self, _voice: &VoiceSel) {}
    fn available(&self) -> bool;
    fn name(&self) -> &'static str;
}

/// Piper: текст в stdin → WAV в stdout. Бинарь и модель — из ~/.jarvis/.
pub struct PiperEngine { pub bin: PathBuf }

impl PiperEngine {
    pub fn new(bin: PathBuf) -> Self { PiperEngine { bin } }
}

impl TtsEngine for PiperEngine {
    fn synthesize(&self, text: &str, voice: &VoiceSel) -> Result<Vec<u8>, TtsError> {
        if !self.available() { return Err(TtsError::NotInstalled("нет бинаря piper".into())); }
        if voice.voice_path.is_empty() || !PathBuf::from(&voice.voice_path).exists() {
            return Err(TtsError::NotInstalled(format!("нет модели голоса: {}", voice.voice_path)));
        }
        let mut child = Command::new(&self.bin)
            .arg("--model").arg(&voice.voice_path)
            .arg("--output_file").arg("-")   // WAV в stdout (подтвердить в Step 0)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn().map_err(|e| TtsError::Synthesis(format!("spawn piper: {e}")))?;
        child.stdin.take().unwrap().write_all(text.as_bytes())
            .map_err(|e| TtsError::Synthesis(format!("stdin: {e}")))?;
        let out = child.wait_with_output().map_err(|e| TtsError::Synthesis(format!("wait: {e}")))?;
        if !out.status.success() || out.stdout.is_empty() {
            return Err(TtsError::Synthesis(format!("piper rc={:?} bytes={}", out.status.code(), out.stdout.len())));
        }
        Ok(out.stdout)
    }
    fn available(&self) -> bool { self.bin.exists() }
    fn name(&self) -> &'static str { "piper" }
}

/// Silero — заглушка Фазы 1: всегда NotInstalled, чтобы engine="silero" молчал безопасно.
pub struct SileroStub;
impl TtsEngine for SileroStub {
    fn synthesize(&self, _t: &str, _v: &VoiceSel) -> Result<Vec<u8>, TtsError> {
        Err(TtsError::NotInstalled("Silero — Фаза 2, сайдкар ещё не реализован".into()))
    }
    fn available(&self) -> bool { false }
    fn name(&self) -> &'static str { "silero" }
}

/// Собрать движок по конфигу. Неизвестный engine → Piper (дефолт).
pub fn build_engine(engine: &str, piper_bin: PathBuf) -> Box<dyn TtsEngine> {
    match engine {
        "silero" => Box::new(SileroStub),
        _ => Box::new(PiperEngine::new(piper_bin)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silero_stub_fails_safe() {
        let e = SileroStub;
        assert!(!e.available());
        assert!(matches!(e.synthesize("x", &VoiceSel{speaker:String::new(),voice_path:String::new(),sample_rate:24000}), Err(TtsError::NotInstalled(_))));
    }

    #[test]
    fn piper_missing_binary_is_not_installed() {
        let e = PiperEngine::new(PathBuf::from("/nonexistent/piper"));
        assert!(!e.available());
        let r = e.synthesize("привет", &VoiceSel{speaker:String::new(),voice_path:String::new(),sample_rate:24000});
        assert!(matches!(r, Err(TtsError::NotInstalled(_))));
    }

    #[test]
    fn build_engine_selects_by_name() {
        assert_eq!(build_engine("silero", PathBuf::from("/x")).name(), "silero");
        assert_eq!(build_engine("piper", PathBuf::from("/x")).name(), "piper");
        assert_eq!(build_engine("???", PathBuf::from("/x")).name(), "piper");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail/compile**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis voice::engine`
Expected: PASS once compiled (logic is complete; tests are regression guards for fail-safe behavior).

- [ ] **Step 3: Implement** — complete in Step 1.

- [ ] **Step 4: Verify tests pass** — `cargo test … voice::engine` ⇒ PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/voice/engine.rs src-tauri/src/voice/mod.rs
git commit -m "feat(voice): TtsEngine trait + Piper subprocess + Silero stub"
```

---

### Task 6: Player (`player.rs`)

Audio playback isn't unit-tested (needs a device); keep the surface tiny and mockable. The queue (Task 7) depends only on a `Play` trait so it stays testable.

**Files:**
- Create: `src-tauri/src/voice/player.rs`
- Modify: `src-tauri/src/voice/mod.rs` (add `pub mod player;`)

- [ ] **Step 1: Define a `Play` trait + rodio impl**

Create `src-tauri/src/voice/player.rs`:

```rust
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
    // OutputStream нельзя Send между потоками безопасно во всех версиях — держим
    // sink под мьютексом и создаём stream лениво в потоке проигрывания.
    current: Mutex<Option<rodio::Sink>>,
    _keep: Mutex<Option<rodio::OutputStream>>,
}

impl RodioPlayer {
    pub fn new() -> Self { RodioPlayer { current: Mutex::new(None), _keep: Mutex::new(None) } }
}

impl Play for RodioPlayer {
    fn play_blocking(&self, wav: Vec<u8>) -> bool {
        let (stream, handle) = match rodio::OutputStream::try_default() {
            Ok(x) => x, Err(e) => { crate::log::line(&format!("[voice] нет аудио-выхода: {e}")); return false; }
        };
        let sink = match rodio::Sink::try_new(&handle) {
            Ok(s) => s, Err(e) => { crate::log::line(&format!("[voice] sink: {e}")); return false; }
        };
        let src = match rodio::Decoder::new(Cursor::new(wav)) {
            Ok(s) => s, Err(e) => { crate::log::line(&format!("[voice] декод WAV: {e}")); return false; }
        };
        sink.append(src);
        *self._keep.lock().unwrap() = Some(stream);
        *self.current.lock().unwrap() = Some(sink);
        // ждём конца, периодически проверяя, не прервали ли (sink.stop() выставит empty)
        loop {
            let done = { let g = self.current.lock().unwrap(); g.as_ref().map(|s| s.empty()).unwrap_or(true) };
            if done { break; }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        let interrupted = self.current.lock().unwrap().is_none();
        *self.current.lock().unwrap() = None;
        *self._keep.lock().unwrap() = None;
        !interrupted
    }
    fn stop(&self) {
        if let Some(s) = self.current.lock().unwrap().take() { s.stop(); }
    }
}
```

Note: exact rodio API (`OutputStream::try_default`, `Sink::try_new`, `Decoder`) must match the version resolved in Task 1. If rodio 0.19 changed these, adjust at this step (e.g. `rodio::play` helpers). The `Play` trait is what the queue uses — its shape is stable.

- [ ] **Step 2: Build check**

Run: `cargo build --manifest-path src-tauri/Cargo.toml --bin jarvis`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/voice/player.rs src-tauri/src/voice/mod.rs
git commit -m "feat(voice): rodio player behind Play trait (interruptible)"
```

---

### Task 7: Speech queue (`queue.rs`)

Serialized, priority, dedup, coalesce, interrupt — all unit-tested against a fake `Play` + fake engine.

**Files:**
- Create: `src-tauri/src/voice/queue.rs`
- Modify: `src-tauri/src/voice/mod.rs` (add `pub mod queue;`)

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/voice/queue.rs` with the queue API and tests. Core type:

```rust
//! Сериализованная очередь речи: одна реплика за раз, приоритет (нужен-человек >
//! готово), дедуп повторов, коалесцирование Stop-бэклога, прерывание текущей.

use crate::voice::composer::{Priority, Utterance};
use std::collections::VecDeque;

#[derive(Default)]
pub struct SpeechQueue {
    items: VecDeque<Utterance>,
    recent_dedup: VecDeque<String>, // последние dedup_key, чтобы не повторять
}

impl SpeechQueue {
    pub fn new() -> Self { Self::default() }

    /// Поставить реплику. Дедуп повторов. NeedHuman лезет вперёд Done.
    /// Возвращает true, если что-то реально добавлено.
    pub fn enqueue(&mut self, u: Utterance) -> bool {
        if self.recent_dedup.contains(&u.dedup_key) { return false; }
        self.recent_dedup.push_back(u.dedup_key.clone());
        if self.recent_dedup.len() > 16 { self.recent_dedup.pop_front(); }
        // вставка с учётом приоритета: NeedHuman перед первым Done
        let pos = self.items.iter().position(|x| x.priority < u.priority);
        match pos { Some(i) => self.items.insert(i, u), None => self.items.push_back(u) }
        true
    }

    /// Достать следующую реплику для проигрывания. При заторе Done-реплики с одной
    /// coalesce_group сливаются в одну («Пиксела и Рекрю закончили»).
    pub fn next(&mut self) -> Option<Utterance> {
        let first = self.items.pop_front()?;
        if let Some(group) = first.coalesce_group.clone() {
            // собрать все остальные той же группы и того же приоритета
            let mut projects = vec![first_project(&first.text)];
            self.items.retain(|x| {
                if x.coalesce_group.as_deref() == Some(&group) { projects.push(first_project(&x.text)); false } else { true }
            });
            if projects.len() > 1 {
                let joined = join_ru(&projects);
                return Some(Utterance { text: format!("{joined} закончили"), ..first });
            }
        }
        Some(first)
    }

    pub fn is_empty(&self) -> bool { self.items.is_empty() }
}

/// «Пиксела: …» → «Пиксела» (для коалесцирования). Грубо: до первого «:»/«,»/« готов».
fn first_project(text: &str) -> String {
    text.split([':', ',']).next().unwrap_or(text).split(" готов").next().unwrap_or(text)
        .split(" закончил").next().unwrap_or(text).trim().to_string()
}

/// «А», «А и Б», «А, Б и В».
fn join_ru(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [a] => a.clone(),
        [rest @ .., last] => format!("{} и {}", rest.join(", "), last),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::composer::Priority;

    fn done(project: &str) -> Utterance {
        Utterance { text: format!("{project} закончил"), priority: Priority::Done,
            dedup_key: format!("stop:{project}"), coalesce_group: Some("stop-done".into()) }
    }
    fn wait(project: &str) -> Utterance {
        Utterance { text: format!("{project} ждёт — нужно разрешение"), priority: Priority::NeedHuman,
            dedup_key: format!("notif:{project}"), coalesce_group: None }
    }

    #[test]
    fn need_human_jumps_ahead_of_done() {
        let mut q = SpeechQueue::new();
        q.enqueue(done("Пиксела"));
        q.enqueue(wait("Рекрю"));
        assert_eq!(q.next().unwrap().priority, Priority::NeedHuman);
    }

    #[test]
    fn dedup_repeated_notification() {
        let mut q = SpeechQueue::new();
        assert!(q.enqueue(wait("Пиксела")));
        assert!(!q.enqueue(wait("Пиксела")), "повтор того же dedup_key не добавляется");
    }

    #[test]
    fn coalesces_done_backlog() {
        let mut q = SpeechQueue::new();
        q.enqueue(done("Пиксела"));
        q.enqueue(done("Рекрю"));
        let u = q.next().unwrap();
        assert_eq!(u.text, "Пиксела и Рекрю закончили");
        assert!(q.is_empty(), "обе done-реплики ушли в одну");
    }

    #[test]
    fn single_done_not_coalesced() {
        let mut q = SpeechQueue::new();
        q.enqueue(done("Пиксела"));
        assert_eq!(q.next().unwrap().text, "Пиксела закончил");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis voice::queue`
Expected: FAIL initially if any helper stubbed; here code is complete → should PASS. The value is the regression guard. If `Priority` ordering is wrong (Done=0 < NeedHuman=1, and `position(|x| x.priority < u.priority)`), verify `need_human_jumps_ahead_of_done` passes.

- [ ] **Step 3: Implement** — complete in Step 1; fix any ordering bug surfaced by tests.

- [ ] **Step 4: Verify** — `cargo test … voice::queue` ⇒ PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/voice/queue.rs src-tauri/src/voice/mod.rs
git commit -m "feat(voice): serialized priority speech queue (dedup + coalesce)"
```

---

### Task 8: Voice service (`mod.rs`)

Wires config + engine + composer + queue + player on a background thread. Public API used by the daemon/tray.

**Files:**
- Modify: `src-tauri/src/voice/mod.rs`

- [ ] **Step 1: Implement the service**

`src-tauri/src/voice/mod.rs`:

```rust
pub mod numerals;
pub mod composer;
pub mod config;
pub mod engine;
pub mod player;
pub mod queue;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use composer::{Composer, SpeechSignals, TemplateComposer, Priority};
use config::VoiceConfig;
use engine::{build_engine, TtsEngine, VoiceSel};
use player::{Play, RodioPlayer};
use queue::SpeechQueue;

/// Голосовой сервис: композитор + очередь + движок + проигрыватель на фоне.
pub struct Voice {
    composer: Box<dyn Composer>,
    engine: Box<dyn TtsEngine>,
    player: Arc<dyn Play>,
    voice: VoiceSel,
    queue: Arc<(Mutex<SpeechQueue>, Condvar)>,
    mute: Arc<AtomicBool>,
}

impl Voice {
    pub fn new(cfg: &VoiceConfig, piper_bin: std::path::PathBuf) -> Arc<Self> {
        let engine = build_engine(&cfg.engine, piper_bin);
        let v = Arc::new(Voice {
            composer: Box::new(TemplateComposer),
            engine,
            player: Arc::new(RodioPlayer::new()),
            voice: VoiceSel { speaker: cfg.speaker.clone(), voice_path: cfg.voice_path.clone(), sample_rate: cfg.sample_rate },
            queue: Arc::new((Mutex::new(SpeechQueue::new()), Condvar::new())),
            mute: Arc::new(AtomicBool::new(cfg.mute)),
        });
        v.clone().spawn_worker();
        v
    }

    pub fn set_mute(&self, on: bool) {
        self.mute.store(on, Ordering::SeqCst);
        if on { self.player.stop(); } // мгновенно глушим текущую
    }
    pub fn is_muted(&self) -> bool { self.mute.load(Ordering::SeqCst) }

    /// Композирует сигналы в реплику и кладёт в очередь (fail-safe).
    pub fn speak(&self, signals: SpeechSignals) {
        if self.is_muted() { return; }
        let Some(u) = self.composer.compose(&signals) else { return; };
        let high = u.priority == Priority::NeedHuman;
        let (m, cv) = &*self.queue;
        let added = m.lock().unwrap().enqueue(u);
        if added {
            if high { self.player.stop(); } // прерываем текущую низкоприоритетную
            cv.notify_one();
        }
    }

    pub fn test_phrase(&self, text: &str) {
        let (m, cv) = &*self.queue;
        m.lock().unwrap().enqueue(composer::Utterance {
            text: text.to_string(), priority: Priority::Done,
            dedup_key: format!("test:{text}"), coalesce_group: None,
        });
        cv.notify_one();
    }

    pub fn warmup(&self) { self.engine.warmup(&self.voice); }
    pub fn engine_name(&self) -> &'static str { self.engine.name() }
    pub fn engine_available(&self) -> bool { self.engine.available() }

    fn spawn_worker(self: Arc<Self>) {
        std::thread::spawn(move || loop {
            let next = {
                let (m, cv) = &*self.queue;
                let mut g = m.lock().unwrap();
                while g.is_empty() { g = cv.wait(g).unwrap(); }
                g.next()
            };
            let Some(u) = next else { continue; };
            if self.is_muted() { continue; }
            match self.engine.synthesize(&u.text, &self.voice) {
                Ok(wav) => { self.player.play_blocking(wav); }
                Err(e) => crate::log::line(&format!("[voice] {} молчит: {e}", self.engine.name())),
            }
        });
    }
}
```

- [ ] **Step 2: Register module in main.rs**

Ensure `src-tauri/src/main.rs` has `mod voice;` (added in Task 2).

- [ ] **Step 3: Build check**

Run: `cargo build --manifest-path src-tauri/Cargo.toml --bin jarvis`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/voice/mod.rs src-tauri/src/main.rs
git commit -m "feat(voice): Voice service — compose→queue→engine→player worker"
```

---

### Task 9: Installer — Piper binary + Russian voice (`setup.rs`)

**Files:**
- Modify: `src-tauri/src/bin/setup.rs`

- [ ] **Step 0 (VERIFY LIVE): pick the real Piper asset URLs for this platform**

Determine the macOS (arm64/x64) `piper` release asset and one Russian voice (`.onnx` + `.onnx.json`) from the official Piper voices repo. Record the URLs/checksums. **If the asset layout differs from what this step assumes, stop and update the plan.** Target install dir: `~/.jarvis/piper/piper` (binary), `~/.jarvis/voices/ru.onnx` + `ru.onnx.json`.

- [ ] **Step 1: Add install function**

In `setup.rs`, add `fn install_piper() -> Result<(), String>` that: creates `~/.jarvis/piper/` and `~/.jarvis/voices/`; downloads the binary + voice if absent (idempotent — skip if present); `chmod +x` the binary; atomic write (download to `.tmp`, rename). On any failure return `Err` with a clear message — installer continues (voice is optional), prints a notice. Wire it into the existing `install` flow after the hooks/shim steps, non-fatal:

```rust
match install_piper() {
    Ok(()) => println!("✓ Piper установлен (~/.jarvis/piper, голос ~/.jarvis/voices)"),
    Err(e) => eprintln!("⚠ Piper не установлен ({e}); голос Piper будет недоступен, демон не затронут"),
}
```

Set defaults into `settings.json` `voice.voicePath` = `~/.jarvis/voices/ru.onnx` on first install if unset.

- [ ] **Step 2: Extend `status`**

In the `status` subcommand, print voice engines:

```
Голос:
  piper:  установлен=<да/нет> (бинарь + модель на месте), активен=<да/нет по voice.engine>
  silero: Фаза 2 — не установлен
```

- [ ] **Step 3: Build + manual run**

Run: `cargo run --release --manifest-path src-tauri/Cargo.toml --bin jarvis-setup -- status`
Expected: prints voice section without panicking whether or not Piper is present.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/bin/setup.rs
git commit -m "feat(setup): install Piper binary + ru voice; voice status"
```

---

### Task 10: Daemon wiring — build signals & speak

**Files:**
- Modify: `src-tauri/src/daemon.rs`

- [ ] **Step 1: Add `Voice` to `Daemon` + warmup on start**

Add a field `pub voice: std::sync::Arc<crate::voice::Voice>` to the `Daemon` struct. In `Daemon::new`, read settings, build `VoiceConfig::from_settings(&settings_root)`, construct `Voice::new(&cfg, piper_bin_path())` where `piper_bin_path()` = `~/.jarvis/piper/piper`. After construction, spawn warmup: `let v = d.voice.clone(); std::thread::spawn(move || v.warmup());`.

- [ ] **Step 2: Build `SpeechSignals` at the event sites**

In the event handler, after the `match event` mutates the session, add a helper `fn voice_signal(s: &Session, event: composer::Event, extra…)` that fills `SpeechSignals` from the session: `project` (s.project), `board_done`/`board_total`/`board_active` from `s.board`, `diff_files` from `s.touched.len()` (or diff-stat source), `notification_text` from the notification message, `limit_reset_min` from the StopFailure reset. Then, gated by config events:
  - `stop` arm: if `cfg.ev_stop` → `self.voice.speak(stop_signal)`.
  - `notification` arm (the `is_new && !redundant_idle` branch already dedups): if `cfg.ev_notification` → `self.voice.speak(notif_signal)`.
  - `stop-failure` effect (`Effect::StopFailure`): if `cfg.ev_stop_failure` → `self.voice.speak(limit_signal)`.

Read `cfg` once per call from `self.settings` (or cache `VoiceConfig` on `Daemon`, refreshed on settings change). Keep it off the lock: collect the needed fields under the reducer lock, call `self.voice.speak(...)` AFTER `self.push()` (consistent with effects running after the lock is released).

- [ ] **Step 3: Build check + smoke**

Run: `cargo build --release --manifest-path src-tauri/Cargo.toml --bin jarvis`
Expected: PASS. Manual: restart daemon, trigger a Stop on a real session → hear a phrase (if Piper installed) or see `[voice] … молчит: …` in `~/.jarvis` log (if not). Daemon must stay up either way.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/daemon.rs
git commit -m "feat(voice): speak on stop/notification/stop-failure from session signals"
```

---

### Task 11: Tray — mute toggle + test voice

**Files:**
- Modify: `src-tauri/src/tray.rs`

- [ ] **Step 1: Add menu items**

In `build_menu`, after the existing items, append a `CheckMenuItem` with id `voice-mute` (checked = `d.voice.is_muted()`), label "Без звука", and a `MenuItem` id `voice-test`, label "Тест голоса". In `on_menu`, handle:
  - `"voice-mute"` → `d.voice.set_mute(!d.voice.is_muted())`, then `refresh_menu(d)`.
  - `"voice-test"` → `d.voice.test_phrase("Проверка голоса. Пиксела: четыре из шести задач, сейчас docker-compose.")`.

Include `voice-mute` checked-state in `menu_signature` so the menu re-renders when toggled elsewhere.

- [ ] **Step 2: Build check**

Run: `cargo build --release --manifest-path src-tauri/Cargo.toml --bin jarvis`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/tray.rs
git commit -m "feat(voice): tray mute toggle + test-voice item"
```

---

### Task 12: Full test sweep + README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Run the whole suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis`
Expected: PASS (all prior tests + new voice tests).

- [ ] **Step 2: README voice section**

Add a "## Голос (TTS)" section to `README.md`: что делает (озвучка событий), как ставится Piper (установщик), как переключать движок (`voice.engine` в `settings.json` + перезапуск демона), как сменить/прослушать спикер (тест из меню-бара), **явный размен** Silero (Фаза 2: лучший русский, тяжёлый PyTorch-сайдкар, медленнее старт) vs Piper (лёгкий, быстрый, слабее русский и английские вставки), и граница: **вывод-only, войс-ввод — следующая фаза**.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: voice (TTS) section — Piper, engine switch, output-only boundary"
```

---

## Self-Review

**Spec coverage:**
- Engine trait + Piper + Silero stub → Task 5 ✓
- Composer (board>diff>fact, Notification, StopFailure, truncation, LLM seam=trait) → Task 3 ✓
- Russian numerals/units → Task 2 ✓
- Queue (serialize, priority, dedup, coalesce, interrupt) → Task 7 + interrupt in Task 8 `speak`/`set_mute` ✓
- Player (rodio, interruptible) → Task 6 ✓
- Config (`voice` block, defaults, per-event) → Task 4 ✓
- Tray (mute, test) → Task 11 ✓
- Installer (Piper + voice, status) → Task 9 ✓
- Daemon wiring (signals from existing state, warmup, fail-safe) → Task 10 ✓
- Fail-safe everywhere → engine errors logged not panicked (Tasks 5,8,10) ✓
- README → Task 12 ✓
- Silero full sidecar + Python installer → **Phase 2, out of this plan** (stub fails safe) ✓

**Acceptance scenarios (Phase 1):** 3 (priority — Task 7), 4 (coalesce — Task 7), 5 (dedup — Task 7), 6 (mute — Tasks 8/11), 9 (truncation — Task 3), 10 (numerals — Task 2), 2-partial & 7-failsafe (engine switch + silero stub — Tasks 5/8). Scenarios 1/7-full/8 require Silero → Phase 2.

**Placeholder scan:** composer Step 3 contains derivation scaffolding (`let _ = head;`) explicitly marked to be collapsed to the final branch — not a placeholder, a worked example. Installer/engine have explicit live-verify Step 0s per the spec's "stop on divergence" rule (real Piper flags / asset URLs), not hand-waving.

**Type consistency:** `Utterance`/`Priority`/`SpeechSignals`/`VoiceSel`/`VoiceConfig`/`Play`/`TtsEngine` names are used identically across Tasks 3–11. `count_phrase` carries `Gender` consistently after Task 2 Step 3. `speak`/`set_mute`/`test_phrase`/`warmup` on `Voice` match tray/daemon callers.
