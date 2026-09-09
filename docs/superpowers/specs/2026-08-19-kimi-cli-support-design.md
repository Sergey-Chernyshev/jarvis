# Spec: Kimi Code CLI как первоклассный бэкенд Jarvis (наравне с Claude Code и Codex)

> Дата: 2026-08-19 · Статус: v1 · Цель: обернуть **Kimi Code CLI** (`kimi`, Moonshot AI) так же полно,
> как уже обёрнуты **Claude Code** и **Codex CLI**, через существующий шов `enum Agent` + `trait Backend`,
> без дублирования фич и без изменения поведения Claude и Codex.
>
> Предшественник: `2026-06-26-codex-cli-support-design.md`. Этот документ следует его структуре
> намеренно — третий агент обязан идти той же дорогой, что второй.

## 0. Дисциплина фактов

Всё в §2 проверено **эмпирически на живой машине** (Kimi Code CLI 0.37.0, macOS 15, Apple Silicon):
проба хуков с записью payload, разбор 29 сессий и 201 файла `wire.jsonl`, сверка документации
с исходниками `MoonshotAI/kimi-code`. Факты, взятые только из документации, помечены `[doc]`.

**Ловушка источников.** Существуют два разных продукта:

| | `kimi-cli` (legacy) | **Kimi Code CLI** (актуальный) |
|---|---|---|
| Репозиторий | `MoonshotAI/kimi-cli` (Python) | `MoonshotAI/kimi-code` (TypeScript) |
| Дом | `~/.kimi/` | **`~/.kimi-code/`** |
| Env дома | — | **`KIMI_CODE_HOME`** |
| Событий хуков | 13 | **20** |
| `/hooks` | есть | **нет** (вместо неё skill `/update-config`) |
| Хуки помечены Beta | да | нет |

Документация на `www.kimi-cli.com` и `moonshotai.github.io/kimi-cli` описывает **legacy** и даёт
неверные пути. Канон — `moonshotai.github.io/kimi-code` и репозиторий `MoonshotAI/kimi-code`.

## 1. Цели и не-цели

**Цели:**
1. **Мониторинг** интерактивных Kimi-TUI сессий: панель, тосты, голос, чат, статус, живость.
2. **Контроль**: reply, смена модели и effort, resume.
3. **Usage-статистика** Kimi: токены по моделям из `wire.jsonl`.
4. **Ожидание разрешения** как первоклассный статус — у Kimi для этого есть выделенные события.

**Не-цели:**
- Никаких изменений поведения Claude и Codex — байт-в-байт прежнее, инвариант на каждом инкременте.
- Лимиты/квоты Kimi (нет аналога `rate_limits` в логе) — вне scope.
- Внутренний Kimi как service-LLM и agent-host — отдельная спека, после этой.
- Git-ветка сессии — Kimi её нигде не сохраняет (§2, факт 12).

## 2. Ключевые факты-ограничения (проверены эмпирически)

| # | Факт | Следствие |
|---|---|---|
| 1 | Хуки живут в `[[hooks]]` внутри `~/.kimi-code/config.toml`; допустимы ровно 4 поля (`event`,`command`,`matcher`,`timeout`), **лишнее поле роняет загрузку конфига** (zod `.strict()`) | Нужен **TOML-writer**, а не JSON. Бэкап обязателен: битый конфиг = Kimi не стартует |
| 2 | Проектных хуков нет — только пользовательский уровень | Один файл, без мержа уровней |
| 3 | **Хуки стреляют и в headless `kimi -p`** | Отличие от Codex (`exec` хуков не шлёт). Шим не обязан глушить `-p`, но такие сессии будут «вне tmux» |
| 4 | **`$PPID` хука = процесс `kimi-code`** | `bin/jarvis-hook` работает без изменений; `pid_alive()` в `reconcile_sessions` судит верно |
| 5 | Базовый payload: `hook_event_name`, `session_id`, `cwd`, `client_type`, `session_title?`. **НЕТ `transcript_path`, НЕТ `pid`, НЕТ pane** | Путь к транскрипту Jarvis обязан вычислять сам (факт 8) |
| 6 | `client_type` = `"kimi_code_cli"` у CLI; у web/desktop — своё | **Фильтр обязателен**: иначе `kimi web` и ACP наплодят сессий без паны |
| 7 | `SessionStart` несёт `source` (`startup`/`resume`), **`model`** (алиас) и `profile`. `SessionEnd` несёт `reason` (`exit`/`archive`). `SessionHeartbeat` — раз в 60 с, и **только если на него повешен хук** | `s.model` из payload как у Codex; heartbeat — дешёвый liveness |
| 8 | Каталог сессии вычисляется детерминированно: `sessions/wd_<slug>_<sha256(abs_cwd)[..12]>/<session_id>/agents/main/wire.jsonl`, где slug = basename в нижнем регистре, всё кроме `[a-z0-9._-]` → `-`, схлопывание, обрезка до 40. Проверено 6/6 воркспейсов | `transcript_dir_for(cwd)` — чистая функция, **без чтения индекса** |
| 9 | `session_index.jsonl` существует, но **ненадёжен**: сам CLI его для поиска сессий не читает, а в логе пользователя висит `WARN session index reconciliation failed` | Индекс — только фолбэк, основной путь — факт 8 |
| 10 | `Stop`-хук **НЕ несёт** финальный ответ (у Codex есть `last_assistant_message`) | Финал берём из `wire.jsonl` (`content.part` с `part.type=="text"`) |
| 11 | Ответ ассистента разложен по событиям `content.part` (`text` / `think`); `context.append_message` — **всегда `role:"user"`** | Роль выводится из типа события, не из поля |
| 12 | Git-ветки нет нигде в данных Kimi | `extract_branch()` → `None`; ветку добывать из `cwd` штатным фолбэком по `.git/HEAD` (он уже есть для Codex) |
| 13 | `usage.record{scope:"turn"}` — **побайтовый дубль** `step.end.usage` (сверено 201 файл, расхождений 0) | Считать **только** `usage.record`, иначе двойной учёт |
| 14 | Схема usage: `inputOther`, `output`, `inputCacheRead`, `inputCacheCreation`. Полей `total`/`reasoning` нет | input = сумма трёх |
| 15 | Ожидание разрешения: `interaction.request{kind:"approval"}` без парного `interaction.resolved` с тем же `id`. Плюс события хуков `PermissionRequest`/`PermissionResult` | Waiting — **из хука**, лог нужен только для дорасследования |
| 16 | Effort: `llm.request.thinkingEffort`; допустимые для K3 — `["low","high","max"]` (`support_efforts` в конфиге), дефолт `high`. Задаётся **отдельно** от модели | `has_separate_effort()` → `true` (как Claude, не как Codex) |
| 17 | Модель: последний `llm.request.modelAlias` (полный, `kimi-code/k3`); фолбэк — `config.update.modelAlias`, затем `profile.bind.modelAlias` | `extract_model()` |
| 18 | `state.json` бывает **двух версий**: v1 (`workDir`, ISO-строки) и v2 (`cwd`, epoch ms) | Парсер обязан знать обе |
| 19 | `turnId` — то строка (loop-события), то число (`turn.ended`) | `#[serde(untagged)]` |
| 20 | MCP-инструменты именуются `mcp__<server>__<tool>` — **как в Claude Code** `[doc]` | Гейт INV-TOOLS в `agent/mod.rs` применим без изменений (для будущей спеки про agent-host) |
| 21 | `--output-format stream-json` даёт **OpenAI-подобные** записи с ключом `role` (`assistant`/`tool`/`meta`), а не Claude-конверты с `type` | Свой парсер, если делать agent-host. Вне scope этой спеки |
| 22 | Подкоманды `kimi mcp` не существует; MCP настраивается записью `~/.kimi-code/mcp.json` `[doc]` | Для будущего agent-host |
| 23 | `SessionEnd` в headless `-p` **не наблюдался** | Уборка таких сессий — через `pid_alive`, штатным `reconcile_sessions` |

## 3. Архитектура

### 3.1 Принцип: третий вариант в существующем шве

Шов уже построен под Codex и агент-нейтрален: ключ реестра — голый `session_id`, `Session.agent`
хранится строкой, `trait Backend` диспатчится через `backend(a)`. Kimi добавляется как
`Agent::Kimi` + `backend/kimi.rs` + `backend/kimi_transcript.rs`. **Новых абстракций не вводим.**

### 3.2 Что чиним по дороге (и почему это не рефакторинг ради рефакторинга)

Шов недостроен в четырёх местах: способность агента выражена сравнением с `Agent::Codex`,
а не флагом. Пока агентов два, `!= Codex` и `== Claude` — синонимы; с третьим они расходятся,
и Kimi молча получает чужое поведение. Поэтому это не косметика, а **условие корректности**:

| Место | Сейчас | Станет |
|---|---|---|
| `ipc.rs:1682-1698` | `if agent == Codex → «effort через /model»` | `if !backend(agent).has_separate_effort()` — флаг **уже есть в трейте** |
| `ipc.rs:1808-1813`, `tmux.rs:456`, `ui/question-answer.js:11` | «свой ответ» запрещён через `== Codex` / `!== 'codex'` в трёх местах | новый `fn supports_custom_answer(&self) -> bool`, одна точка правды |
| `ipc.rs:1646-1665` | `if agent != Codex → validate_model()` (Claude-аллоулист) | аллоулист берётся из `backend(agent).models()` |
| `daemon.rs:1635-1651`, `1704-1805` | `is_claude` / `is_codex` — бинарные развилки парсера финала и меты | `fn final_reply(&self, ...)`; `extract_title`/`extract_model` подключаются к call-site (это и есть «инкремент 3» из спеки Codex, который так и не доделали) |

`Agent::all()` меняем с `[Agent; 2]` на `&'static [Agent]` и **даём ему первого потребителя** —
`loops/ipc.rs:48-64`, где сейчас захардкожены ровно два ключа `{"claude","codex"}`.

### 3.3 Что остаётся общим (НЕ форкается)

Редьюсер и реестр `daemon.rs`; `Session`/`Status`; `ChatItem`; tmux-транспорт целиком
(`reply`, `list_panes_meta`, `capture_pane`, `focus`); голос/STT/wakeword; capability/MCP;
settings-store; рендер панели; агрегация usage; `bin/jarvis-hook` (уже принимает метку `$1`).

## 4. Детальный дизайн

### 4.1 Provisioning (install/mod.rs, bin/agent-shim)

**События.** Третья таблица `KIMI_EVENTS`, маппинг в те же внутренние арги, что у Claude/Codex:

```
SessionStart      → session-start     UserPromptSubmit  → prompt
PreToolUse        → pre-tool          PostToolUse       → post-tool
PermissionRequest → permission        Stop              → stop
SessionEnd        → session-end       SubagentStart     → subagent-start
SubagentStop      → subagent-stop     SessionHeartbeat  → heartbeat
```

`heartbeat` — новое внутреннее событие (у Claude/Codex аналога нет): обновляет `updated_at`,
не меняя `status`. Даёт корректный liveness без опроса pid.

**TOML-writer.** Дополнительной зависимости (`toml`/`toml_edit`) **не вводим** — в проекте уже
есть идиом managed-блока между маркерами для не-JSON конфига (`merge_block` для `~/.zshrc`,
`install/mod.rs`). Применяем тот же: блок `[[hooks]]` в конце `config.toml` между
`# >>> jarvis` / `# <<< jarvis`. Это валидно всегда: array-of-tables в конце файла не может
попасть в чужую секцию. Идемпотентность, бэкап и atomic-write переиспользуем существующие.
Обоснование против зависимости: `jarvis-setup` компилируется отдельным бинарём без остального
крейта, и лишний ход зависимостей там нежелателен.

**Шим.** `bin/agent-shim` получает третью ветку по basename. Passthrough-подкоманды Kimi:
`login`, `acp`, `web`, `server`, `doctor`, `export`, `migrate`, `upgrade`, `update`, `vis`,
`provider`. `NET_VARS` — без `ANTHROPIC_BASE_URL`. Флаг «опасного режима» — `--yolo`.

**`bin/jarvis-hook:16`** — `case "$AGENT" in claude|codex)` расширяется на `kimi`: без этого
Kimi получит непустой stdout на `Stop` и попытается интерпретировать его как инструкцию.

**Детект и здоровье.** `kimi_found()`, `kimi_home()` (учитывая `KIMI_CODE_HOME`),
`kimi_hooks_path()`, `kimi_shim_dst()`; `IntegrationHealth` +3 поля; `install_core`,
`reconcile_hooks`, `install_tmux_transport`, `uninstall`, `status_report`, `repair` — по образцу
Codex-блоков.

### 4.2 Ингест (daemon.rs)

- **Фильтр `client_type`**: конверт хука несёт его дальше; сессии с `client_type != "kimi_code_cli"`
  в реестр **не заводим** (факт 6). Это единственное новое правило приёма.
- `s.model` из payload — расширяем существующий Codex-guard на Kimi (`SessionStart.model`).
- `permission` → `Status::Waiting` (переиспользуем ветку Codex).
- `heartbeat` → только `updated_at`.
- `session-end` → сессия исчезает (у Codex этого события нет, у Kimi есть — путь уже реализован для Claude).
- Ключ реестра не меняется, `evict_pane`/`restore_state` не трогаем.

### 4.3 Транскрипт (backend/kimi_transcript.rs)

`wire.jsonl` → `ChatItem`. Лог линейный, `chain_from_entries` не нужен (как у Codex).

- `context.append_message` с `origin.kind == "user"` → реплика пользователя.
  **Остальные `origin.kind` (`injection`, `system_trigger`, `background_task`, `task`,
  `skill_activation`, `cron_job`) отбрасываем** — это служебные впрыски, в чат им нельзя.
- `content.part{part.type=="text"}` → реплика ассистента; `think` — скрываем (аналог `reasoning`).
- `tool.call` → строка-метка через общий `short_tool_label`, рендер из `event.display`
  (8 видов: `file_io`, `command`, `search`, `url_fetch`, `todo_list`, `agent_call`,
  `plan_review`, `skill_call`).
- `tool.result` — показываем только при `result.isError == true`.
- `extract_model` — факт 17; `extract_title` — из `state.json` соседней папки, фолбэк — первая
  пользовательская реплика; `extract_branch` → `None`.
- Битые строки пропускаем молча — так делает и сам CLI.
- Blobs (`blobref:<mime>;<sha256>` → `blobs/<sha256>`) — вне scope, картинки в панели не рендерим.

Диспетчеризовать по агенту **все** call-sites парсера — тот же список, что в спеке Codex §4.3
(`ipc.rs` chat_open, `tail.rs`, `daemon.rs` ai_toast_summary / refresh_meta / dialog summary,
`capability/native/chats.rs`), плюс `turns.rs::collect_facts` (новая `facts_kimi`).

### 4.4 Контроль (ipc.rs, tmux.rs)

- **Reply** — без изменений, tmux нейтрален.
- `resume_cmd` → `kimi -S {sid}` (`sid` уже вида `session_<uuid>`; `-r` — скрытый алиас).
- `models()` → четыре алиаса из `config.toml` с человеческими именами
  (`kimi-code/k3` → «K3» и т. д.); `effort_levels()` → `["low","high","max"]`;
  `has_separate_effort()` → `true`.
- `answer_keys(Agent::Kimi, …)` — раскладка пикера калибруется вживую (см. §6, риск 1).
  До калибровки — консервативная ветка «цифра + Enter», как у Codex.
- `supports_custom_answer()` → определяется при калибровке; по умолчанию `false`.

### 4.5 Usage (usage.rs)

Сканер `~/.kimi-code/sessions/**/agents/*/wire.jsonl`, pre-filter по подстроке `usage.record`.
Считаем **только** `usage.record` (факт 13), суммируя по всем агентам сессии, включая сабагентов.
`billing = "kimi"`. Прайсинг — оценка/конфиг, помечаем как оценку: официальных цен в конфиге нет.
Лимитов не делаем (факт: аналога `rate_limits` в логе нет), `limits.rs` **не трогаем вовсе**.

### 4.6 Вторичные поверхности

`history.rs` — третий сканер каталога; `commands_catalog.rs` — набор слэш-команд Kimi
(полный список известен из исходников: `yolo`, `auto`, `permission`, `model`, `effort`, `plan`,
`compact`, `usage`, `status`, `sessions`, `tasks`, `mcp`, `plugins`, `fork`, `undo`, …);
`onboarding.rs::readiness()` — третий `ReadinessItem`; `agents.rs:38` — `kimi` в `RESERVED`;
`launch.rs:51` и `loops/runner.rs:102` — третья ветка запуска.

### 4.7 UI

`MODELS_BY_AGENT` +ключ; `ui/loops.js:431` — список агентов из каталога, а не литерал
`['claude','codex']`; `ui/loops.js:519-523` — чинится заодно (там `modelsFor('claude')` вместо
`modelsFor(d.agent)` — существующий баг); `ui/renderer.js:1048` — `modelsFor` через каталог;
`ui/renderer.js:1941` — скрытие effort-селектора по capability, а не по id;
`ui/question-answer.js:11` — `customAllowed` из capability;
`ui/renderer.js:3572/3600` — кнопки запуска собираются из списка агентов;
`ui/settings2.js:2171` — текст «ни Claude, ни Codex» становится общим.

## 5. Изменения модели данных

Новых обязательных полей нет. `Session.agent` — уже свободная строка, старые `state.json`
читаются без миграции (`from_opt(None) → Claude`). Единственный риск обратной совместимости —
**обратный**: пока `from_label` не знает метку `kimi`, такие сессии молча считались бы Claude.
Поэтому `from_label`/`label`/`backend()` идут в первом же коммите, **до** включения хуков.

## 6. План инкрементов

0. **Скелет:** `Agent::Kimi`, `all() → &'static [Agent]`, `backend/kimi.rs` с полным `impl Backend`
   на консервативных заглушках. Компилятор укажет на `tmux.rs:408` и `turns.rs:132`.
1. **Capabilities:** четыре развилки-по-id из §3.2 → флаги трейта. Claude и Codex — байт-в-байт.
2. **Provisioning:** `KIMI_EVENTS`, TOML-writer с managed-блоком, шим, `jarvis-hook`, health,
   onboarding. → Kimi-сессии появляются в панели.
3. **Ингест:** фильтр `client_type`, `heartbeat`, `permission`, `model` из payload.
4. **Транскрипт:** `kimi_transcript.rs` + все call-sites. → чат, тосты, голос, саммари.
5. **Контроль и UI:** resume, модели, effort, пикеры, бейджи, каталог агентов во фронте.
6. **Usage:** сканер `wire.jsonl`, агрегация, прайсинг-оценка.
7. **Вторичные поверхности:** history, commands_catalog, launch, loops.
8. **Docs+тесты:** README RU→EN зеркалом в том же коммите.

Каждый инкремент компилируется; Claude и Codex неизменны на каждом.

## 7. Тестирование

- **Юнит:** `encode_workdir_key` на реальных путях (6 проверенных пар); парсер `wire.jsonl` на
  фикстурах реальных строк → `ChatItem`, включая фильтр `origin.kind`, `think`-скрытие и
  `untagged turnId`; `state.json` v1 и v2; usage без двойного учёта; TOML merge-блок
  (идемпотентность, снятие, сохранность чужих `[[hooks]]`); `Agent::from_label("kimi")`;
  фильтр `client_type`.
- **Регресс-инвариант:** все 880 существующих тестов зелёные на каждом инкременте.
- **Smoke (ручной):** интерактивный `kimi` под dev-профилем (`~/.jarvis-dev`): метка `kimi`,
  тост на Stop, чат рендерится, reply доходит, смена модели и effort, usage растёт,
  `SessionEnd` убирает сессию.

## 8. Риски и открытые вопросы

1. **TUI-хореография Kimi** (пикеры `AskUserQuestion`, подтверждение `/model`) — калибруется
   вживую; регэксп подтверждения в `tmux.rs:207` под Claude/Codex может не совпасть.
2. **Правка чужого `config.toml`.** Лишнее поле роняет загрузку конфига целиком. Managed-блок,
   бэкап и `kimi doctor` после записи — обязательны. Проверено: наш блок конфиг не ломает.
3. **Headless `-p` шлёт хуки** — в панели появятся короткоживущие сессии «вне tmux».
   Убираются `pid_alive` за ≤30 с. Если станет шумно — фильтровать по отсутствию `SessionEnd`.
4. **Версионность.** Хуки `TurnStarted`/`SessionHeartbeat`/`UserPromptQueued`/`TaskStarted`
   появились в 0.32.0 (2026-08-04). На более старых Kimi их не будет — таблица событий должна
   деградировать мягко. Под `KIMI_CODE_LEGACY_FLAG=1` движок откатывается на 16 событий.
5. **`session_index.jsonl` частично сломан** у реальных пользователей — не полагаться (факт 9).
6. **Прайсинг Kimi — оценка.**

## 9. Обратная совместимость

- Ключ реестра не меняется, миграции состояния не требуется.
- `~/.claude/settings.json` и `~/.codex/hooks.json` — не трогаются.
- `~/.kimi-code/config.toml` — дописывается managed-блок с бэкапом; `teardown` снимает его.
- `npm run setup` начинает ставить и Kimi-интеграцию (если `kimi` найден); `teardown` снимает все три.
