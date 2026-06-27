# План: полноценный голосовой ассистент

Дата: 2026-06-27
Статус: в работе
Цель пользователя: Jarvis должен стать **полноценным голосовым ассистентом** —
внутренний API (агенты/чаты: читать, искать, слать, управлять), внешний мир
(веб-поиск, ответы на вопросы, «думать»), запуск приложений, базовое управление
OS (звук, музыка), и **сквозной контекст разговора**. Всё голосом, свободно.

Строится поверх `convo/` (2a/2b) — Rust-оркестратор + структурный Haiku-план.
Добавляем СКИЛЫ в меню планировщика + расширяем триаж + один внешний агент-хост.

## Модель доверия (три яруса)

- **Free (auto, без карточки):** все reads + **benign-reversible** OS-действия
  (медиа play/pause/next/prev, системная громкость, запуск известного приложения).
  Аргументы строго валидируются (без shell/пробелов/control, без путей). Эффект
  обратимый. Джарвис озвучивает короткое подтверждение («Ставлю на паузу»).
- **Confirm (карточка Да/Отмена):** impactful/менее-обратимое — set_model/set_effort,
  keep_awake, mute (глушит аудит-след). Как сейчас.
- **Deny (fail-closed):** неизвестный скил / грязные аргументы → Rejected.

Микрофон недоверен: веб-контент и реплика — ДАННЫЕ, не команды (фенсинг в промптах).

## Инкременты (TDD, рабочий коммит на этап)

### Инкремент 1 — OS-control скилы (`convo/os.rs` + `macos.rs`)
- `macos.rs`: `media_next/media_prev/media_toggle` (коды MediaRemote 4/5/2 через
  существующий `mra_run`).
- `convo/os.rs` (чистое ядро + тонкий exec):
  - `media_command_code(action) -> Option<i32>` (play/pause/toggle/next/prev).
  - `validate_app_name(name) -> Result` (буквы/цифры/пробел/.&-, без `/`, без shell).
  - `volume_plan(args) -> Result<VolumeOp>` (set 0..100 / mute / unmute), + applescript.
- Скилы `media{action}`, `open_app{name}`, `system_volume{...}` → auto → `Controlled`.
- Тесты: коды команд, валидация имени (инъекции отклонены), кламп громкости.

### Инкремент 2 — расширенные read-скилы (`convo/skills.rs`)
- `session_detail{id}` — ветка/модель/effort/last_prompt/статус из `d.session(id)`.
- `search_chats{query}` — поиск по транскриптам живых сессий → совпадения (сниппеты).
- `metrics` — сводка использования (`ipc::usage_summary`).
- `limits` — статус лимита (`ipc::limit_get`).
- Все reads → `Data` → `followup_phrase` озвучивает. Тесты: детектор совпадений
  (чистый), форма Data.

### Инкремент 3 — внешний ассистент (`agent/assistant.rs` + скил `assistant`)
- `build_assistant_args(query, cwd, model)` (чистая, тест): `-p`, stream-json,
  `--allowedTools WebSearch WebFetch Read Grep Glob`, `--strict-mcp-config`,
  `--setting-sources project,local`, изолированный scratch-cwd, voice-friendly
  system-prompt (коротко, по-русски).
- `AssistantHost::run(query) -> Option<String>` — спавн `claude`, стрим →
  финальный текст (`Done.result` или склейка `Delta`), переиспользует
  `agent::parse_stream_line`. Env как у run_claude (прокси, JARVIS_IGNORE).
- Скил `assistant{query}` → `SkillOutcome::Answer(text)` (озвучка verbatim).
- Триаж в `plan.rs`: внешние/общие вопросы и «найди/поищи» → `assistant`.

### Инкремент 4 — сквозной контекст (`convo/memory.rs` + персист)
- `Memory::persisted_path()`, `load_persisted(max)`, `save()` —
  `~/.jarvis[-dev]/convo-memory.json`. Хранит ТОЛЬКО санированные ходы (как сейчас:
  user/assistant/короткая сводка) — инвариант «нет сырого untrusted» сохранён.
- `start_conversation` загружает прошлые ходы; после хода/в конце — сохраняет.
  → «свободно говорить о проблеме, он хранит контекст» между разговорами.
- Тесты: round-trip сериализации; клампы; инвариант (нет сырого untrusted).

## Вне scope (этой итерации)
- 4b: выполнение произвольных команд агентом через permission-tool (отдельный риск-слой).
- Акустический барж-ин 2c (нативный AEC) — отдельная research-веха.
- Полный OS-automation (клики по UI, Show Numbers) — нужен accessibility-слой.

## Тестирование
Каждый инкремент — чистые юнит-тесты ядра + smoke на форму Outcome. Полный
`cargo test` зелёный на каждом коммите. UI вживую не запускается (как и прежде).
