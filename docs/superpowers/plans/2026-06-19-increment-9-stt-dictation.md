# Инкремент 9 · STT-сервис + диктовка по клавише — план реализации

> **Для исполнителя:** superpowers:subagent-driven-development / executing-plans.
> Скоуп v1 (указание пользователя): **только push-to-talk диктовка**, но правильным
> модулем (свой API, сменные движки, проекция в капабилити). Оба движка сразу.
> Спека: `docs/superpowers/specs/2026-06-19-increment-9-stt-service-design.md`.

**Goal:** зажал хоткей → наговорил → отпустил → распознанный текст вставлен в активное
поле ОС. STT — переиспользуемый сервис (`stt/`) со сменным движком (Whisper-native +
Qwen3-MLX-сайдкар), к которому обращаются и приложение, и (через гейт) плагины.

**Architecture:** зеркалим модуль `voice/` (инкр. 7): трейт движка + `build_engine`,
сайдкар-супервизор, fail-safe. Новое: cpal-захват + ресемпл, вставка текста (буфер+⌘V),
проекция `stt.transcribe` в капабилити (инкр. 8).

**Tech (проверено ресёрчем 2026):** `whisper-rs 0.16` (feature `metal`, модель
`ggml-large-v3-turbo-q5_0.bin` ~574МБ, `set_language("ru")`+`set_translate(false)`);
`qwen3-asr-mlx` (PyPI, принимает float32 PCM 16к; модели `mlx-community/Qwen3-ASR-0.6B/1.7B`);
`cpal 0.18` + `rubato 3` (48к→16к); вставка — `core-graphics` CGEvent ⌘V + clipboard
(крейты уже есть); хоткей-hold — `tauri-plugin-global-shortcut` (Pressed/Released).

**Дисциплина:** при расхождении формы (API qwen3-asr-mlx, whisper-rs, cpal) с эскизом —
остановиться и описать, не выдумывать обход. Внешние API доводятся по компилятору.

---

## Зависимости (Cargo.toml + Info.plist)

- **Cargo:** `cpal = "0.18"`, `rubato = "0.16"` (или текущая 3.x ветка — сверить), `whisper-rs = { version = "0.16", features = ["metal"] }`, `arboard = "3"` (буфер; либо reuse `tauri-plugin-clipboard-manager`). `core-graphics`/`core-foundation`/`objc2` уже есть.
- **Info.plist (бандл):** `NSMicrophoneUsageDescription` ("Jarvis использует микрофон для диктовки"). **Entitlement** `com.apple.security.device.audio-input` в `entitlements.plist`.
- **Разрешения — рантайм пользователя:** микрофон (промпт при первом захвате в подписанном .app) + **Accessibility** (на синтетический ⌘V; промпт/ручное включение в System Settings). **Без подписанного .app капабилити микрофона даёт тихие нули** — финальная проверка только в собранном приложении.

---

## Структура файлов

- Create `src-tauri/src/stt/mod.rs` — `SttService`: владеет движком + конфигом + сайдкаром; API `transcribe(pcm, opts)`, `capture_session()`; fail-safe lifecycle (`tick`/`dispose`).
- Create `src-tauri/src/stt/engine.rs` — трейт `SttEngine` + типы `SttOptions`/`SttResult`/`SttSeg` + `build_engine(cfg)`.
- Create `src-tauri/src/stt/engine_whisper.rs` — Whisper (whisper-rs, Metal).
- Create `src-tauri/src/stt/engine_qwen3.rs` — клиент к Qwen3-сайдкару (HTTP localhost, как voice Silero-клиент).
- Create `src-tauri/src/stt/sidecar.rs` — супервизор Qwen3-сайдкара (зеркало `voice/sidecar.rs`, порт 8732).
- Create `src-tauri/src/stt/audio.rs` — cpal-захват + rubato-ресемпл → 16к моно f32; `CaptureSession` (старт/стоп→буфер).
- Create `src-tauri/src/stt/insert.rs` — вставка текста: clipboard set + CGEvent ⌘V (+ snapshot/restore буфера).
- Create `src-tauri/src/stt/config.rs` — `SttConfig` (engine, per-engine, audio device, hotkey, default ru/transcribe).
- Create `src-tauri/src/stt/dictation.rs` — потребитель: hold-хоткей → capture → transcribe → insert.
- Create `bin/stt-server.py` — FastAPI-сайдкар Qwen3 (POST PCM → текст), зеркало `silero-server.py`.
- Create `src-tauri/src/capability/native/stt_cap.rs` — капабилити `stt.transcribe` (грант на микрофон для плагинов).
- Modify `daemon.rs` — поле `stt: Arc<SttService>`; init в `new`; tick/dispose в таймерах/exit.
- Modify `main.rs` — PTT-хоткей в global_shortcut handler (Pressed/Released); регистрация.
- Modify `install/mod.rs` — `install_whisper` (модель) + `install_stt_sidecar` (venv+MLX+модели Qwen3); `status` по движкам.
- Modify `settings.rs`/`ui` — выбор движка + тест распознавания (по образцу voice).
- Modify `Cargo.toml`, `Info.plist`/`entitlements.plist`.

---

## Фазы (порядок исполнения)

### Фаза 1 — Контракты сервиса + трейт движка + конфиг (тестируемо без модели)
- `stt/engine.rs`: `SttOptions { dominant_lang: String /*"ru"*/, task: SttTask /*Transcribe*/, hints: Vec<String> }`; `SttResult { text: String, segments: Vec<SttSeg> }`; `SttSeg { text, lang: Option<String> }`; трейт `SttEngine { fn name(&self)->&'static str; fn transcribe(&self, pcm:&[f32], opts:&SttOptions)->Result<SttResult,String>; fn available(&self)->bool; }`.
- `stt/config.rs`: `SttConfig { engine: String /*"whisper-turbo"|"qwen3-0.6b"|"qwen3-1.7b"*/, dominant_lang, task, audio_device: Option<String>, hotkey: String }`; `from_settings(&Value)`; дефолты под mixed RU/EN (`dominant_lang="ru"`, `task=Transcribe`, `engine` — из выбора пользователя, дефолт `qwen3-0.6b` по спеке).
- `stt/engine.rs::build_engine(cfg) -> Box<dyn SttEngine>` по `cfg.engine`.
- **Тесты:** выбор движка по конфигу (мок-движки); дефолт-опции (ru/transcribe); `from_settings` парсит.
- **Commit.**

### Фаза 2 — Whisper-движок (whisper-rs, Metal)
- `engine_whisper.rs`: грузит `ggml-large-v3-turbo-q5_0.bin` (путь из конфига/`~/.jarvis/stt/`); `transcribe`: `FullParams` greedy, `set_language(Some(&opts.dominant_lang))`, `set_translate(false)`, `state.full(pcm)`, собрать сегменты. `available()` = модель на месте.
- **Тесты (без инференса):** маппинг опций (ru-пин + translate=false выставлены — проверить через обёртку, если whisper-rs даёт интроспекцию, иначе юнит на построение FullParams-параметров в нашей логике); `available()` по наличию файла.
- **Прогон с моделью** (после Фазы 8 установки) — отложенный live-чек.
- **Commit.**

### Фаза 3 — Qwen3-сайдкар (Python MLX) + клиент-движок
- `bin/stt-server.py`: FastAPI/uvicorn 127.0.0.1, GET `/health` → {ok,model}; POST `/transcribe` (тело: raw f32 little-endian PCM 16к моно ИЛИ base64) + query/header `lang` → `{text, segments?}`. Грузит `qwen3-asr-mlx`: `Qwen3ASR.from_pretrained("mlx-community/Qwen3-ASR-<size>-…")`, `model.transcribe(np.frombuffer(body, '<f4'))`. Модель тёплая (грузится на старте). certifi-CA как в silero.
- `engine_qwen3.rs`: HTTP-клиент к сайдкару (reqwest blocking, как Silero-клиент): POST PCM → текст. `available()` = health ok.
- `sidecar.rs`: зеркало `voice/sidecar.rs` (py/script/port=8732, ensure_started/restart_if_dead/stop/pid/installed).
- **Тесты:** супервизор (`not_installed_when_paths_missing` как у voice); клиент мапит ответ; парс PCM. **Сайдкар live** — после установки (Фаза 8).
- **Commit.**

### Фаза 4 — Аудио-фронтенд (cpal + rubato)
- `audio.rs`: `CaptureSession`: на старте — `default_host().default_input_device()`, `default_input_config()`; `build_input_stream` (match `sample_format`, конверт в f32), накопление в `Arc<Mutex<Vec<f32>>>` (или lock-free ring); на стопе — взять буфер, ресемпл `rubato` исходный_sr→16к, свести в моно. Дроп `Stream` останавливает захват. Не блокировать callback.
- **Тесты:** ресемпл (синтетический буфер N→16к, длина корректна); сведение моно; конверт i16→f32. (Захват микрофона — live.)
- **Commit.**

### Фаза 5 — Вставка текста (clipboard + ⌘V)
- `insert.rs`: `insert_text(text)`: snapshot текущего буфера → set буфер = text → CGEvent keyDown/keyUp keycode 9 ('v') с флагом Command, post `HID` → задержки (~50мс до, ~120мс после) → restore буфера. Использовать `core-graphics` CGEvent (есть) + clipboard.
- **Тесты:** построение CGEvent-последовательности (юнит на нашу логику/последовательность шагов; сам post — live). Snapshot/restore — логика.
- **Commit.**

### Фаза 6 — Потребитель: диктовка по клавише + хоткей
- `dictation.rs`: `Dictation` держит `Arc<SttService>` + флаг `capturing`; `on_press()` (идемпотентно — гард от double-fire): старт `CaptureSession`; `on_release()`: стоп→буфер→`stt.transcribe`→`insert_text`. Fail-safe: ошибки в лог, демон жив.
- `daemon.rs`: `stt: Arc<SttService>` (init в `new` с конфигом), `dictation` держатель.
- `main.rs`: PTT-хоткей (из конфига, дефолт напр. `F8`; не голый общий клавиш) в global_shortcut handler: `Pressed`→`dictation.on_press()`, `Released`→`dictation.on_release()`. Идемпотентность (баг double-fire #10025).
- **Тесты:** state machine диктовки (press→capturing, release→transcribe вызван) с мок-сервисом. Live — в .app.
- **Commit.**

### Фаза 7 — Проекция в капабилити (грант на микрофон для плагинов)
- `capability/native/stt_cap.rs`: капабилити `stt.transcribe` (вход: pcm/опции или ссылка на capture-сессию) — делегирует в `SttService`. Класс — приватно-чувствительный: **не входит в дефолт-грант агента** (как `audit.query` исключён), плагину нужен явный STT-грант (поле гранта `stt: bool` или `denied_ids`/allow-список). Внутренние потребители (диктовка) зовут `SttService` напрямую, не через капабилити.
- `grant.rs`: механизм STT-гранта (плагин с грантом видит `stt.transcribe`, без — нет).
- **Тесты:** плагин с грантом → `stt.transcribe` в проекции/проходит гейт; без гранта → не виден/denied. Внутренний вызов прямой.
- **Commit.**

### Фаза 8 — Установщик + Info.plist/entitlements
- `install/mod.rs`: `install_whisper` — скачать `ggml-large-v3-turbo-q5_0.bin` в `~/.jarvis/stt/` (с прокси/прогрессом, atomic, идемпотентно). `install_stt_sidecar` — `~/.jarvis/stt-mlx/`: venv + `pip install qwen3-asr-mlx mlx-audio fastapi uvicorn numpy certifi` + прелоад моделей `Qwen3-ASR-0.6B`/`1.7B` (через HF cache); `stt-server.py` через include_str!. **Ставить оба**, fail-safe (нет одного → `status` помечает, остальное живо). Залогировать вес.
- `status` по движкам: whisper (модель есть), qwen3 (venv+сайдкар отвечает), активный; + разрешения mic/Accessibility (по возможности детектить `AXIsProcessTrusted`).
- `Info.plist` + `entitlements.plist`: mic usage + audio-input entitlement.
- **Тесты:** пути/идемпотентность (как silero-тесты). Реальная установка (вес!) — отдельным прогоном.
- **Commit.**

### Фаза 9 — Конфиг-панель: выбор движка + тест
- UI: секция STT — выбор активного движка (`whisper-turbo`/`qwen3-0.6b`/`qwen3-1.7b`), индикатор статуса (движок доступен/сайдкар поднят), кнопка «Тест распознавания» (зажать-наговорить-увидеть результат), хоткей диктовки, разрешения (mic/Accessibility). По образцу voice-секции. Смена движка → подсказка «нужен рестарт».
- IPC: `stt_get`/`stt_set_engine`/`stt_test`. Не трогать `renderer.js` пользователя без нужды — отдельная секция/файл, аккуратно.
- **Тесты:** IPC-контракты. UI — визуально.
- **Commit.**

---

## Тесты без живого микрофона (сводка, спека §18)
Выбор движка по конфигу; маппинг опций (transcribe/пин ru) в каждом движке; ресемпл/моно/конверт; state machine диктовки; проекция в капабилити с проверкой STT-гранта; супервизор сайдкара (нет файлов → не падает); установщик (пути/идемпотентность). Полный «наговорил→текст», RU/EN code-switching, латентность — **live в подписанном .app + микрофон + разрешения** (требует пользователя).

## Приёмка (спека §16) — что проверяем live в .app
1. Зажал хоткей, наговорил по-русски → кириллица вставлена в активное поле.
2. Code-switching: «запушим в main, проверим CI» → `main`/`CI` латиницей, фраза не флипается в английский (на каждом движке).
3. Смена движка в конфиге + рестарт → тот же путь, другой движок, без правок потребителей.
6. Qwen3-сайдкар недоступен → диктовка мягко падает, лог; Whisper работает; демон жив.
7. Офлайн → распознавание работает (локальность).

## Риски / границы
- **MLX Qwen3 RU/EN code-switching не бенчмаркнут** + нет context-prompt-параметра в пакете → проверить эмпирически рано (Фаза 3 live); если плохо — описать, не выдумывать (дефолт можно увести на Whisper).
- **Разрешения тихо ломают** (mic→нули, Accessibility→no-op) → тестировать подписанный .app, не `cargo run`.
- **Веса ~2.6ГБ** (Whisper q5 ~574МБ + Qwen3 0.6B/1.7B) — установка долгая, залогировать.
- `qwen3-asr-mlx` v0.1.x — пин версии.
