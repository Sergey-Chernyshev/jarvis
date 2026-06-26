# Дизайн: раздел «Модели» + горячая смена движка

- **Дата:** 2026-06-26
- **Ветка:** `feat/model-management`
- **Статус:** согласование дизайна
- **Источник требований:** «Добавь в настройки возможность скачать и управлять всеми моделями + перезагрузку и смену модели без перезапуска джарвиса». Scope подтверждён пользователем: **все модели, единый раздел**.

## Проблема

1. Модели нельзя **скачать** в сети пользователя: установщик гонит всё через `curl --proxy` одним каналом, а `huggingface.co` доступен только через прокси, тогда как CDN с весами (`us.aws.cdn.hf.co`) прокси **обрывает**. Whisper и веса Qwen скачаны вручную «гибридом» в обход — в продукте этого нет.
2. Моделями нельзя **управлять**: нет удаления, нет единого статуса/размера, выбор STT-движка разбросан, голос/wake — в отдельных карточках.
3. Смена STT-движка **требует перезапуска демона** (`stt_set_engine` → `{restart:true}`).

## Цель

Единый раздел настроек «Модели»: для каждой модели — статус, размер на диске, скачать, удалить, сделать активной, прогресс. Скачивание работает в прокси-сети пользователя. Смена активного STT-движка — без перезапуска демона.

## Модели в обороте

| Ключ | Вид | Источник | Где на диске | Размер |
|---|---|---|---|---|
| `whisper-turbo` | STT | `ggerganov/whisper.cpp` (HF, 1 файл) | `~/.jarvis/stt/ggml-large-v3-turbo-q5_0.bin` | ~574 МБ |
| `qwen3-0.6b` | STT | `mlx-community/Qwen3-ASR-0.6B-8bit` (HF repo) | `~/.jarvis/stt-mlx/models/qwen3-0.6b/` | ~1 ГБ |
| `qwen3-1.7b` | STT | `mlx-community/Qwen3-ASR-1.7B-4bit` (HF repo) | `~/.jarvis/stt-mlx/models/qwen3-1.7b/` | ~1 ГБ |
| qwen venv | STT runtime | PyPI | `~/.jarvis/stt-mlx/venv` | ~2.6 ГБ |
| `silero` | Голос | torch.hub | `~/.jarvis/silero` + `~/.cache/torch/hub` | сотни МБ |
| `hey_jarvis` | Wake | `dscripka/openWakeWord` (GitHub releases) | `~/.jarvis/wakeword/*.onnx` | ~3.5 МБ |

## Scope

**В составе:** единый статус+размер всех моделей; гибридная загрузка (HF и GitHub); скачивание весов Qwen в локальную папку сайдкара; удаление; смена STT-движка с рестартом, затем горячая; маскирование/защита пароля прокси.

**Вне состава (YAGNI):** «сделать активной» для голоса/wake (один движок/одна модель — бессмысленно; у голоса смена спикера уже горячая); реконструкция HF-кэша (сайдкар грузит плоскую папку); закачка моделей на онбординге (остаётся on-demand).

## Архитектура

### A. Гибридная загрузка (на `reqwest`, не curl)

Низкий уровень `fetch_to_file(url, dst, proxy, progress, expected_size)` в `install/mod.rs`:
- Два клиента: `Client::builder().proxy(Proxy::all(p))` и `…​.no_proxy()` (образец — `voice/engine.rs:42`).
- `redirect(Policy::none())`, ручная проходка хопов. Выбор канала **по хосту**: пока хост ∈ {`huggingface.co`} → прокси-клиент; как только Location ведёт на CDN-хост из allowlist (`*.cdn.hf.co`, `cdn-lfs*.huggingface.co`, `*.xethub.hf.co`, `objects.githubusercontent.com`) → прямой клиент без прокси.
- Резюм: при наличии `.tmp` слать `Range: bytes=<n>-`, принимать только `206`. На `403`/истёкшей подписи — **пере-резолв** свежего Location и продолжить с `Range`. Целостность — по `expected_size` (из HF tree API) до `rename`.
- Атомарность tmp→rename — как сейчас. Имя tmp с pid-суффиксом против гонок.

Поверх: `hf_tree(repo, proxy)` (GET `huggingface.co/api/models/<repo>/tree/main` через прокси) и `hf_download_repo(repo, dst_dir, proxy, progress)` — плоско качает все файлы в `dst_dir`. Маппинг ключ→репо дублируется из `stt-server.py:42-45`.

Переключить на это: `install_whisper` (1 файл), `install_wakeword` (GitHub, через общий `fetch_to_file` без HF-слоя), новый `preload_qwen(key)` → `models/<key>/`. Идемпотентность Qwen — по наличию `models/<key>/config.json` (тот же признак, что у сайдкара).

### B. Безопасность пароля прокси

- `chmod 0600` на `settings.json` (как `tokens.json`, `install/mod.rs:165`).
- Маскировать `proxy` (`http://user:***@host`) в `settings_get` (`ipc.rs:57`) и `IntegrationInfo` (`onboarding.rs:79`); сырой использовать только в Rust.
- Никогда не логировать/эмитить прокси-URL и подписанный CDN-URL. `reqwest` убирает утечку через `ps aux` (нет `--proxy` в argv).

### C. Единый статус и UI

- Rust: `models_get` — **только filesystem** (без `d.stt.available()`, который блокирует до 3с). Возвращает `[{id, kind, label, bytes, status, active}]`, расширяя `install::status()`/`model_artifacts()`/`dir_size()`.
- UI: новая карточка «Модели» (хост — по образцу `renderModelsCard`, `renderer.js:2727`), группировка по STT/Голос/Wake. Строка модели — общий `modelRow()` (поднять из `sttModelRow`, `renderer.js:3044`): `.dot`/`.istat[.on|.warn]` + label + `.sz` + хвостовая кнопка `.abtn`. Состояния: не скачана / скачивается / скачана / активная / ошибка — существующими классами.
- Состояние тянуть **один раз** за `loadSettings` и передавать в рендереры аргументом (избежать двойного `stt_get`/рассинхрона). Прогресс роутить по строке (по `kind`), не в общий `#stt-install-progress`.
- Runtime-ручки (хоткей, спикер/темп, порог/mute, Тест) остаются в фича-карточках.
- Опасные действия — двойной клик «Точно?» (идиома `renderModelsCard:2752`), `.abtn danger`. Запрет удаления активной модели; авто-выключение wake при удалении его пака.

### D. Горячая смена STT-движка

- `build_engine`/`build_qwen3_engine` (`stt/engine.rs`) → возвращают `Arc<dyn SttEngine>` (трейт уже `Send+Sync`).
- `SttService`: `engine: Mutex<Arc<dyn SttEngine>>`, `sidecar: Mutex<Option<Arc<SttSidecar>>>`, `config: Mutex<SttConfig>`, `+ transition: Mutex<()>` (калька `WakeWord`, `wakeword/mod.rs:159`).
- `transcribe`: под коротким локом склонировать `Arc` движка и сайдкара, **отпустить локи**, работать на клонах (clone-out — лок держится микросекунды, конкурентность сохранена).
- `set_engine(cfg)`: `transition.lock()` → `stop()` старого сайдкара (`active=false`, kill+wait, освободить порт 8732) → `build_from_cfg(cfg)` (общий с `new`) → swap engine/sidecar/config → ленивый `warm` (**без** блокирующего `wait_ready`).
- `tick()` супервизора (`main.rs:315`) обернуть в тот же `transition` — иначе воскресит остановленный сайдкар.
- `stt_set_engine` (`ipc.rs:848`) → после записи settings звать `set_engine`, вернуть `{restart:false}`. Образец — `wake_set_threshold` (`ipc.rs:910`).
- `config(&self) -> &SttConfig` → `-> SttConfig` (клон; вернуть ссылку из-под лока нельзя).
- **Гейт:** запрет переключения на Qwen без локальных весов (анти-«:8732 висит»).

## Инкременты (порядок реализации, каждый проверяем)

1. **Единый статус (read-only).** `install::status()`+`models_get`+карточка «Модели». Аддитивно. *Тест:* сериализация статуса/артефактов, детект активного движка; ручная проверка раздела.
2. **Гибридный загрузчик (reqwest).** `fetch_to_file`+`hf_tree`+`hf_download_repo`; переключить whisper/wakeword/preload_qwen. *Тест:* парсинг Location→хост, выбор канала, маппинг ключ→путь, skip-if-config; ручной смоук на сети пользователя.
3. **Удаление + место.** Расширить `delete_model` (whisper, qwen веса/venv, wake); запрет удаления активного. *Тест:* id→path, неизвестный id→Err, guard активного.
4. **Смена движка с рестартом + гейт.** UI-действие; запрет Qwen без весов. *Тест:* валидация allow-list + предикат «веса есть».
5. **Безопасность прокси.** chmod 0600 + маскирование. *Тест:* маскирование строки.
6. **Горячая смена STT (без рестарта).** Рефактор `SttService` под локи + `transition`; `set_engine`; `transcribe` clone-out; `tick` под `transition`; `stt_set_engine`→`restart:false`. *Тест:* `MockEngine` — reconfigure меняет `engine_name`, transcribe после смены работает, `in_use` не разъезжается, `sidecar=None`→no-op. Чинить юнит-тесты, строящие `SttService` литералом (`stt/mod.rs:188`), в том же коммите.

Поставка ценности — после 1–4. 5 сквозной. 6 — отдельной итерацией (самый регрессоопасный).

## Риски

- **Подмена `Arc<SttService>` ломает диктовку/wake** (захваченные клоны) — только внутренняя мутация. Самая опасная точка.
- **Гонка `set_engine`↔супервизор** на порту 8732 — обязателен `transition`-лок.
- **Юнит-тесты падут** от рефактора полей `SttService` — править синхронно.
- **GitHub-редирект wake** (`objects.githubusercontent.com`) — не гнать через HF-резолвер, общий redirect-слой.
- **Qwen 8bit не грузится** (баг `qwen3-asr-mlx` quant-loading) — разбирается отдельно; не блокирует фичу, дефолт на Whisper.
- **Не влить крупные загрузки в `install::install()`** — только on-demand.

## Открытые вопросы

- Готовность Qwen-инференса зависит от исхода debug-агента; если не решится — `qwen3-*` в разделе показываются с пометкой «движок недоступен», активировать нельзя.
