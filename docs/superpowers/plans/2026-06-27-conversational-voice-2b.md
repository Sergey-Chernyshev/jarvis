# Conversational Voice — Milestone 2b (multi-turn, half-duplex) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`).

**Goal:** После «Hey Jarvis» — полноценный многоходовый разговор: Джарвис слушает реплику (VAD-эндпойнтинг, не фикс-окно), отвечает голосом, помнит контекст, и слушает следующую реплику без повторного wake; конец по тишине ~8-10с или стоп-фразе. **Полудуплекс** (микрофон закрыт, пока Джарвис говорит — барж-ин это веха 2c).

**Architecture:** Новый разговорный цикл держит single-flight на ВЕСЬ диалог (wake-действие подавлено в это время). Каждый ход: VAD-захват (`convo/listen.rs` поверх `AudioHub::subscribe_wake` per-frame) → STT → `converse_turn` (снапшот+память+Haiku-план+скилы+голос, БЕЗ потребления conversation-lock) → `Voice::speak_blocking` (знаем, когда речь кончилась → открываем мик). Память — скользящее окно ходов без сырого untrusted. route внутри хода использует свой stage-токен, отдельный от conversation-lock.

**Tech Stack:** Rust (Tauri 2, tokio), `cargo test`; `AudioHub::subscribe_wake`/`WakeTap::recv_timeout` (80мс кадры, FRAME_LEN=1280); `voice` (Silero); reuse `convo::{plan,snapshot,skills}` + `route::*` из 2a.

**Спека:** `docs/superpowers/specs/2026-06-27-conversational-voice-design.md` (рев.2), §2b.

---

## Структура файлов

- `convo/vad.rs` — **чистый** VAD: `rms(frame)` + `Endpointer` (автомат Idle→Speech→Trailing→Done / NoSpeechTimeout), адаптивный порог. Юнит-тесты.
- `convo/listen.rs` — потоковый захват: WakeTap → `Endpointer` → `ListenResult{Utterance(Vec<f32>)|Silence|Empty}`. Тонкий.
- `convo/memory.rs` — **чистая** `Memory` (кольцо ходов, лимит, без сырого untrusted) + рендер в промпт. Юнит-тесты.
- Правки: `voice/mod.rs` (+`speak_blocking` с сигналом завершения), `voice/queue.rs`/`composer.rs` (опц. поле сигнала), `convo/mod.rs` (`converse_turn` guard-free + `start_conversation` цикл; `plan::build_plan_prompt` получает память), `convo/plan.rs` (память в промпт), `wakeword/action.rs` (on_wake → start_conversation).

Порядок: чистые ядра (vad, memory) → speak_blocking → listen → цикл/память-проводка → wiring. До последнего шага прод не меняется (start_conversation не подключён).

**Команды:** `cargo test --manifest-path src-tauri/Cargo.toml convo::vad` ; сборка как в 2a.

---

## Task 1: VAD-эндпойнтер (`convo/vad.rs`, чистый)

**Files:** Create `src-tauri/src/convo/vad.rs`; Modify `convo/mod.rs` (`pub mod vad;`).

- [ ] **Step 1: тесты**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn frame(level: f32, n: usize) -> Vec<f32> { vec![level; n] }

    #[test]
    fn rms_of_constant_is_level() {
        assert!((rms(&frame(0.5, 100)) - 0.5).abs() < 1e-6);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn endpoints_after_speech_then_trailing_silence() {
        // порог калибруется по тихим кадрам, затем речь, затем тишина
        let mut ep = Endpointer::new(2 /*calib frames*/, 3 /*trailing silence frames*/, 50 /*max wait frames*/);
        assert_eq!(ep.push(rms(&frame(0.001, 10))), Step::Calibrating);
        assert_eq!(ep.push(rms(&frame(0.001, 10))), Step::Waiting);
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
        ep.push(rms(&frame(0.001, 10))); // calib
        for _ in 0..3 { assert_eq!(ep.push(0.001), Step::Waiting); }
        assert_eq!(ep.push(0.001), Step::Timeout); // превышен max wait без речи
    }
}
```

- [ ] **Step 2: FAIL.** `cargo test … convo::vad` → FAIL.

- [ ] **Step 3: реализация**

```rust
//! Чистый VAD-эндпойнтер: энергия кадра (RMS) + автомат начала/конца реплики.
//! Без I/O — кадры подаёт listen.rs из потока AudioHub. Порог адаптивный
//! (калибровка по шумовому полу первых кадров), т.к. в пайплайне нет AGC.

/// Среднеквадратичная энергия кадра (0.0 на пустом).
pub fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() { return 0.0; }
    let sum: f32 = frame.iter().map(|x| x * x).sum();
    (sum / frame.len() as f32).sqrt()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Calibrating, // копим шумовой пол
    Waiting,     // ждём начало речи
    Speaking,    // идёт реплика
    Done,        // реплика закончилась (трейлинг-тишина)
    Timeout,     // речь так и не началась за max_wait
}

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

    /// Множитель порога над шумовым полом (старт речи). Фикс. дефолт; настройка позже.
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
        // в речи: считаем трейлинг-тишину
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
```

- [ ] **Step 4: PASS + commit.**
```bash
cargo test --manifest-path src-tauri/Cargo.toml convo::vad
git add src-tauri/src/convo/ && git commit -m "feat(convo): чистый VAD-эндпойнтер (RMS + автомат начала/конца) — TDD"
```

> Дефолты (calib≈5 кадров=400мс, trailing≈10=800мс, max_wait≈125=10с) выставит listen.rs; здесь — чистый автомат.

---

## Task 2: Память разговора (`convo/memory.rs`, чистая)

**Files:** Create `src-tauri/src/convo/memory.rs`; Modify `convo/mod.rs` (`pub mod memory;`).

- [ ] **Step 1: тесты**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_last_n_turns() {
        let mut m = Memory::new(2);
        m.push("a", "ra", None);
        m.push("b", "rb", None);
        m.push("c", "rc", None);
        let r = m.render();
        assert!(!r.contains("a:") && !r.contains("ra"), "старый ход вытеснен");
        assert!(r.contains("b") && r.contains("c"));
    }

    #[test]
    fn render_includes_short_action_result_not_raw() {
        let mut m = Memory::new(4);
        m.push("сколько ждут", "две сессии", Some("sessions_status: 2 waiting"));
        let r = m.render();
        assert!(r.contains("сколько ждут"));
        assert!(r.contains("две сессии"));
        assert!(r.contains("sessions_status"));
    }

    #[test]
    fn empty_render_is_empty() {
        assert_eq!(Memory::new(3).render(), "");
    }
}
```

- [ ] **Step 2: FAIL.** `cargo test … convo::memory` → FAIL.

- [ ] **Step 3: реализация**

```rust
//! Скользящая память разговора. Хранит ТОЛЬКО реплику юзера, ответ ассистента и
//! КОРОТКУЮ структурную сводку действия (skill + санитизированные args + код) —
//! НИКОГДА сырой untrusted-текст (chats.read/документы), чтобы инъекция из одного
//! хода не переезжала во все следующие промпты (см. спеку §Безопасность).

use std::collections::VecDeque;

struct Turn {
    user: String,
    assistant: String,
    action_result: Option<String>,
}

pub struct Memory {
    turns: VecDeque<Turn>,
    max: usize,
}

impl Memory {
    pub fn new(max_turns: usize) -> Self {
        Self { turns: VecDeque::new(), max: max_turns.max(1) }
    }

    /// Добавить ход. `action_result` — короткая сводка (НЕ сырой контент).
    pub fn push(&mut self, user: &str, assistant: &str, action_result: Option<&str>) {
        self.turns.push_back(Turn {
            user: user.to_string(),
            assistant: assistant.to_string(),
            action_result: action_result.map(str::to_string),
        });
        while self.turns.len() > self.max {
            self.turns.pop_front();
        }
    }

    /// Рендер для промпта (пусто, если ходов нет).
    pub fn render(&self) -> String {
        if self.turns.is_empty() {
            return String::new();
        }
        let mut out = String::from("Контекст разговора (старые→новые):\n");
        for t in &self.turns {
            out.push_str(&format!("Юзер: {}\nДжарвис: {}\n", t.user, t.assistant));
            if let Some(ar) = &t.action_result {
                out.push_str(&format!("(действие: {ar})\n"));
            }
        }
        out
    }
}
```

- [ ] **Step 4: PASS + commit.**
```bash
cargo test --manifest-path src-tauri/Cargo.toml convo::memory
git add src-tauri/src/convo/ && git commit -m "feat(convo): память разговора (кольцо ходов, без сырого untrusted) — TDD"
```

---

## Task 3: `Voice::speak_blocking` (полудуплекс-сигнал) (`voice/`)

Цикл должен знать, КОГДА речь кончилась, чтобы открыть мик (иначе слушает свой голос).

**Files:** Modify `src-tauri/src/voice/composer.rs` (поле сигнала в Utterance), `voice/mod.rs` (`speak_blocking`), `voice/queue.rs` (не ломать дедуп).

- [ ] **Step 1:** добавить в `Utterance` опц. канал завершения:
```rust
    /// Some → воркер сигналит сюда после play_blocking (для speak_blocking).
    pub done: Option<std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>>,
```
Обновить ВСЕ конструкторы Utterance (speak/speak_text/test_phrase/say) полем `done: None`. (`Utterance` уже не Default — заполняем явно.)

- [ ] **Step 2:** в воркере (`voice/mod.rs` spawn_worker, после `play_blocking`/прерывания) — сигналить:
```rust
            // (после play_blocking и веток ошибки/мьюта — на ВСЕХ путях, где утта «отыграна»)
            if let Some(done) = &u.done {
                let (m, cv) = &**done;
                *m.lock().unwrap() = true;
                cv.notify_all();
            }
```
Важно: сигналить и при `is_muted()` (continue) — иначе вызывающий зависнет. Вынести сигнал так, чтобы он срабатывал на любом исходе утты.

- [ ] **Step 3:** метод:
```rust
    /// Озвучить и ДОЖДАТЬСЯ конца (для полудуплексного цикла). Возвращает, когда
    /// речь отыграна/прервана/смьючена. Таймаут-страховка ~30с.
    pub fn speak_blocking(&self, text: &str) {
        use std::sync::{Arc, Condvar, Mutex};
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        let (m, cv) = &*self.queue;
        {
            let mut q = m.lock().unwrap();
            q.enqueue(Utterance {
                text: text.to_string(), priority: crate::voice::composer::Priority::Done,
                dedup_key: format!("blk:{}", crate::util::now_ms()), coalesce_group: None,
                toast_id: None, done: Some(done.clone()),
            });
        }
        cv.notify_one();
        let (lock, c) = &*done;
        let mut g = lock.lock().unwrap();
        let start = std::time::Instant::now();
        while !*g {
            let (ng, to) = c.wait_timeout(g, std::time::Duration::from_millis(500)).unwrap();
            g = ng;
            if *g || start.elapsed() > std::time::Duration::from_secs(30) { break; }
        }
    }
```
(Сверь `crate::util::now_ms`/`Instant` доступность; `now_ms` есть в util.)

- [ ] **Step 4: сборка + commit.**
```bash
cargo build --manifest-path src-tauri/Cargo.toml --features wakeword-ort --bin jarvis
git add src-tauri/src/voice/ && git commit -m "feat(voice): speak_blocking — сигнал завершения речи для полудуплексного цикла"
```

---

## Task 4: Потоковый VAD-захват (`convo/listen.rs`)

**Files:** Create `src-tauri/src/convo/listen.rs`; Modify `convo/mod.rs` (`pub mod listen;`).

- [ ] **Step 1: реализация** (тонкая обёртка; чистая логика уже в vad.rs):
```rust
//! Потоковый захват реплики с VAD-эндпойнтингом поверх AudioHub::subscribe_wake.
//! 80мс-кадры (FRAME_LEN=1280 @16к). Полудуплекс: звать, когда Джарвис молчит.

use std::sync::Arc;
use std::time::Duration;

use crate::convo::vad::{rms, Endpointer, Step};
use crate::stt::hub::AudioHub;

pub enum ListenResult {
    Utterance(Vec<f32>), // накопленный PCM реплики
    Silence,             // никто не заговорил (таймаут) → конец разговора
}

/// Дефолты: калибровка 5 кадров (~400мс), трейлинг 10 (~800мс), ожидание старта
/// max_wait кадров (~9с при 80мс). preroll — кадры ДО старта (контекст начала).
pub fn listen(hub: &Arc<AudioHub>, max_wait_frames: u32) -> ListenResult {
    let tap = hub.subscribe_wake();
    let mut ep = Endpointer::new(5, 10, max_wait_frames);
    let mut buf: Vec<f32> = Vec::new();
    loop {
        let Some(frame) = tap.recv_timeout(Duration::from_millis(500)) else {
            // источник молчит/закрылся — считаем тишиной
            return ListenResult::Silence;
        };
        let e = rms(&frame);
        match ep.push(e) {
            Step::Speaking => buf.extend_from_slice(&frame),
            Step::Done => return ListenResult::Utterance(buf),
            Step::Timeout => return ListenResult::Silence,
            Step::Calibrating | Step::Waiting => {}
        }
    }
}
```
> Тап дропается на выходе (Drop отписывает). Тест — ручной/смоук (поток); чистый автомат покрыт vad.rs.

- [ ] **Step 2: сборка + commit.**
```bash
cargo build --manifest-path src-tauri/Cargo.toml --features wakeword-ort --bin jarvis
git add src-tauri/src/convo/ && git commit -m "feat(convo): потоковый VAD-захват реплики (listen.rs поверх WakeTap)"
```

---

## Task 5: Многоходовый цикл + память (`convo/mod.rs`)

**Files:** Modify `convo/mod.rs` (`converse_turn` guard-free + `start_conversation`), `convo/plan.rs` (память в промпт), `convo/skills.rs` (route-в-ходе со своим stage-токеном).

- [ ] **Step 1: `converse_turn`** — версия `converse_once` БЕЗ потребления conversation-lock и С памятью:
  - сигнатура `async fn converse_turn(d: &Arc<Daemon>, transcript: &str, mem: &mut Memory) -> bool` (возвращает `end`).
  - промпт: `plan::build_plan_prompt_mem(snap, menu, &mem.render(), transcript)` (добавить вариант с памятью; или расширить build_plan_prompt 4-м арг `memory: &str`).
  - route внутри хода: НЕ потребляет conversation-lock. Зовём `route::route_transcript(d.clone(), prompt, dummy_guard)` где `dummy_guard` — отдельный `SingleFlight::default().try_enter().unwrap()` (свой токен) ИЛИ рефактор route на приём `Box<dyn Send>`-hold. Простейшее: завести локальный `let sf = route::SingleFlight::default(); let g = sf.try_enter().unwrap();` и отдать g (отдельный от диалога).
  - после ответа: `mem.push(transcript, &spoken, action_summary)`; озвучка через `Voice::speak_blocking` (полудуплекс) вместо `say`.
  - стоп-фраза: если `p.end` → вернуть true.

- [ ] **Step 2: `start_conversation`** — цикл:
```rust
pub fn start_conversation(d: Arc<Daemon>, _preroll: Vec<f32>, guard: SfGuard) {
    std::thread::spawn(move || {
        let _conv = guard; // single-flight на ВЕСЬ диалог → wake-действие подавлено
        let mut mem = memory::Memory::new(6);
        let hub = d.audio.clone();
        loop {
            crate::route::hud::emit(&d, crate::route::hud::Phase::Listening { secs: 9 });
            let pcm = match listen::listen(&hub, 112 /*~9с*/) {
                listen::ListenResult::Utterance(p) => p,
                listen::ListenResult::Silence => break, // тишина → конец
            };
            let text = /* stt.transcribe(&pcm) */ String::new();
            if text.trim().is_empty() { continue; }
            if is_stop_phrase(&text) { d.voice.speak_blocking("Поняла, отключаюсь"); break; }
            let end = tauri::async_runtime::block_on(converse_turn(&d, &text, &mut mem));
            if end { break; }
        }
        crate::route::hud::emit(&d, crate::route::hud::Phase::Cancelled); // «разговор закрыт»
    });
}

fn is_stop_phrase(t: &str) -> bool {
    let t = t.to_lowercase();
    ["спасибо", "хватит", "всё", "отбой", "стоп"].iter().any(|s| t.contains(s))
}
```
(STT-вызов — как в текущем on_wake: `d.stt.transcribe(&pcm, &d.stt.options())`.)

- [ ] **Step 3:** `wakeword/action.rs` on_wake → `convo::start_conversation(d, preroll, guard)` (вместо `converse_once`). Первый ход тоже идёт через `listen` (preroll можно влить как первые кадры — опц.).

- [ ] **Step 4: сборка + смоук + commit.**
```bash
cargo build … --bin jarvis ; cargo test … convo::
git add -A && git commit -m "feat(convo): многоходовый разговор — VAD-цикл + память + полудуплекс + conversation-lock"
```

> Тест чистых частей (vad/memory) уже есть; цикл — ручная GUI-проверка.

---

## Верификация (ручная, мик)
`npm start`. «Hey Jarvis» → «сколько времени?» (ответ голосом) → БЕЗ повторного wake: «а сколько сессий ждёт?» (помнит контекст) → «переключи фронт на opus» (Да/Отмена) → «спасибо» (прощается, разговор закрыт). Проверить: пока Джарвис говорит, мик не слушает (полудуплекс); пауза ~9с без речи закрывает разговор; повторный «Hey Jarvis» в разговоре игнорируется.

## Соответствие спеке (self-review)
§2b VAD → Task 1,4. §Память (без сырого untrusted) → Task 2. §Полудуплекс/speak_blocking → Task 3,5. §Conversation-lock/подавление wake → Task 5. §Конец (тишина/стоп-фраза) → Task 5. Барж-ин/AEC → веха 2c (вне 2b).
