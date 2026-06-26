# Spec: Codex CLI как первоклассный бэкенд Jarvis (наравне с Claude Code)

> Дата: 2026-06-26 · Статус: на ревью · Effort: ultracode
> Цель: обернуть **Codex CLI** (`codex`, OpenAI) так же полно, как сейчас обёрнут **Claude Code** (`claude`), через общую абстракцию, без дублирования фич.

## 1. Цели и не-цели

**Цели (выбранный объём — максимальный):**
1. **Мониторинг** интерактивных Codex-TUI сессий: панель, тосты, голосовые саммари, просмотр чата, статус/живость.
2. **Контроль**: вставка ответов (reply), смена модели/effort, ответ на вопрос, resume.
3. **Usage-статистика** для Codex: токены/квота из rollout `token_count`/`rate_limits`, прайсинг OpenAI.
4. **Внутренний Codex**: Jarvis может использовать `codex exec` для собственных саммари/переводов и для встроенного gated agent-chat.

**Не-цели:**
- Мониторинг headless `codex exec` запусков **невозможен через хуки** (эмпирически: `codex exec` не дёргает `hooks.json`; хуки только в интерактивном TUI). Headless-запуски вне scope для мониторинга (видны лишь тейлингом rollout — не делаем).
- Не трогаем рабочие Claude-пути, кроме рефакторинга в общий бэкенд-слой (поведение Claude должно остаться байт-в-байт прежним).
- Не реализуем Windows/Linux специфику (проект — macOS/Apple Silicon).

## 2. Ключевые факты-ограничения (эмпирически проверены)

| Факт | Следствие для дизайна |
|---|---|
| `codex exec` НЕ шлёт хуки; интерактивный `codex` (TUI) шлёт все 5 (10 событий поддерживается) | Мониторинг = только интерактивные сессии (= аналог обёртки интерактивного `claude`) |
| Хук-payload Codex **Claude-совместим**: `session_id`, `cwd`, `transcript_path`, `tool_name`, `tool_input` — те же имена; `$PPID` = pid codex | Ядро ингеста daemon.rs работает почти без изменений |
| Payload Codex несёт `model`, `turn_id`, `permission_mode`; Stop несёт `last_assistant_message` (НЕТ `payload.message`); `source` только на SessionStart | `s.model` ставим прямо из payload (проще Claude); waiting-детект — через `PermissionRequest`, не `message` |
| `~/.codex/hooks.json` уже существует, но написан вручную с меткой агента `claude` (баг) | Установщик должен переписать с меткой `codex`; иначе Codex-сессии маскируются под Claude и ломают парсинг транскрипта |
| Hook-trust: `config.toml [hooks.state]` sha256, прообраз не воспроизводится | Свежие машины: шим инжектит `--dangerously-bypass-hook-trust` (решение пользователя) |
| Rollout: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`, строки `{timestamp,type,payload}`, type ∈ session_meta/turn_context/response_item/event_msg | Нужен отдельный парсер транскрипта Codex за общим интерфейсом `ChatItem` |
| Контроль: `-m model`, `-c model_reasoning_effort=<minimal\|low\|medium\|high\|xhigh>`, `codex resume <id>`; `-p`=profile (НЕ print) | Команды/resume за бэкенд-слоем; осторожно с `-p` |
| Usage: нет аналога `/usage`; квота инлайн в `event_msg.token_count.rate_limits` | `official_info()` → `Option` на бэкенд; Codex синтезирует из rate_limits |
| `codex exec --json`: thread.started/turn.started/item.*/turn.completed; init **без** `tools[]` | INV-TOOLS-гейт не воспроизводим → превентивная изоляция `--ignore-user-config -s read-only` + только jarvis-MCP |

## 3. Архитектура

### 3.1 Принцип: один шов, не форк

Код уже на ~90% агент-нейтрален: сессии ключуются по `payload.session_id`; поле `Session.agent` существует, но никем не читается; бейдж UI (`renderer.js:239`) уже падает в `s.agent`; tmux-транспорт печатает в любой TUI. Вводим **один** `enum Agent { Claude, Codex }` и сосредотачиваем **всё** бэкенд-специфичное за **одним** трейтом `Backend`.

### 3.2 Что остаётся общим (НЕ форкается)

- Редьюсер/реестр/машина статусов daemon.rs (ключ, eviction, reconcile, эффекты, push/persist).
- `Session` + `Status` (model.rs) — чистое рантайм-состояние, уже нейтральное.
- Контракт чата `ChatItem` (transcript.rs) — выход обоих парсеров.
- `AgentEvent` (agent/mod.rs) — внутренний словарь событий agent-host.
- tmux-транспорт (tmux.rs): `reply`, буферы, send-keys, list_panes_meta, focus, ping.
- Голос/STT/wakeword, capability/MCP-слой (`jarvis-mcp` + токен), settings-store, рендер панели.
- Агрегация usage: `Tok/HourAgg/SessionAgg/WindowAgg`, 5h-окно, offsets, `stats()`-шейпинг.
- `parse_ts`, `squeeze_reply`, `jarvis-hook` (уже принимает `$1`=agent).

### 3.3 Что уходит за трейт `Backend`

Новый модуль `src-tauri/src/backend/` с трейтом и двумя ZST-реализациями (`ClaudeBackend`, `CodexBackend`) и диспетчером `fn backend(a: Agent) -> &'static dyn Backend`.

```rust
// backend/mod.rs (эскиз — финальные сигнатуры уточняются в плане)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent { Claude, Codex }
impl Agent {
    pub fn from_label(s: &str) -> Agent { if s == "codex" { Agent::Codex } else { Agent::Claude } }
    pub fn label(self) -> &'static str { match self { Agent::Claude => "claude", Agent::Codex => "codex" } }
    pub fn all() -> [Agent; 2] { [Agent::Claude, Agent::Codex] }
}

pub trait Backend: Send + Sync {
    fn agent(&self) -> Agent;

    // — Provisioning —
    /// [(имя-события-в-конфиге-агента, внутренний-arg)] для регистрации хуков.
    fn hook_events(&self) -> &'static [(&'static str, &'static str)];
    /// Путь к файлу регистрации хуков агента.
    fn hooks_path(&self) -> PathBuf;            // claude: ~/.claude/settings.json; codex: ~/.codex/hooks.json
    /// Записать/смержить регистрацию хуков (бэкенд знает формат своего файла).
    fn install_hooks(&self, hook_bin: &Path) -> io::Result<()>;
    fn uninstall_hooks(&self) -> io::Result<()>;
    fn hooks_status(&self) -> bool;
    /// Бинарь установлен?
    fn cli_found(&self) -> bool;
    /// Шим: пропустить обёртку (passthrough) для этого argv? claude:-p/print/auth; codex:exec/login/...
    fn shim_passthrough(&self, args: &[String]) -> bool;

    // — Transcript → ChatItem —
    fn read_chain(&self, file: &Path, max_bytes: u64) -> Vec<Value>;   // read+chain (claude) / read (codex)
    fn to_chat_items(&self, entry: &Value) -> Vec<ChatItem>;
    fn extract_title(&self, entries: &[Value]) -> Option<String>;
    fn extract_branch(&self, entries: &[Value]) -> Option<String>;
    fn extract_model(&self, entries: &[Value]) -> Option<String>;
    fn full_final_reply(&self, transcript_path: &str) -> Option<String>;
    fn transcript_dir_for(&self, cwd: &str) -> Option<PathBuf>;

    // — Control —
    fn resume_cmd(&self, session_id: &str) -> String;            // "claude --resume X" / "codex resume X"
    fn set_model(&self, d:&Arc<Daemon>, sid:&str, model:&str);   // slash (claude) / picker (codex)
    fn set_effort(&self, d:&Arc<Daemon>, sid:&str, level:&str);
    fn answer_question(&self, pane:&str, item:&Question, picks:&[usize]);
    fn friendly_model(&self, id: &str) -> String;
    fn models(&self) -> &'static [(&'static str, &'static str)];
    fn effort_levels(&self) -> Vec<String>;

    // — Usage / limits —
    fn scan_usage(&self, st:&mut UsageState);                     // claude: ~/.claude/projects glob; codex: ~/.codex/sessions glob
    fn price(&self, model: &str) -> (f64, f64);
    fn official_info(&self) -> Option<OfficialInfo>;             // claude: scrape /usage; codex: from rate_limits

    // — Service LLM + agent-host —
    async fn run_service_llm(&self, prompt:&str, timeout:Duration) -> Option<String>;
    fn build_agent_args(&self, cfg:&AgentRunCfg) -> Vec<String>;
    fn parse_agent_line(&self, line:&str) -> Vec<AgentEvent>;
    fn agent_isolation_env(&self) -> Vec<(String,String)>;
}
```

> Реализация может оказаться смесью трейта + свободных функций там, где трейт-объект неудобен (async-методы → выделить в отдельные не-trait хелперы, вызываемые по `match agent`). Финальная форма — в плане; важно, что **точек ветвления ровно столько, сколько строк в таблице §3.3**, и все они в одном модуле.

## 4. Детальный дизайн по концернам

### 4.1 Provisioning (install/mod.rs, bin/claude-shim → bin/agent-shim, onboarding)

- **EVENTS → per-agent.** Сейчас `EVENTS` (install/mod.rs:38-47) — 8 Claude-событий. Делаем `backend(a).hook_events()`:
  - Claude: те же 8 (`session-start/prompt/pre-tool/post-tool/notification/stop/stop-failure/session-end`).
  - Codex: `SessionStart→session-start`, `UserPromptSubmit→prompt`, `PreToolUse→pre-tool`, `PostToolUse→post-tool`, `Stop→stop`, **`PermissionRequest→permission`** (→Waiting), **`SubagentStart→subagent-start`**, **`SubagentStop→subagent-stop`**.
- **Команда хука:** `format!("{} {} {arg}", hook_bin, agent.label())` — метка `claude`/`codex` (фиксит баг install/mod.rs:832).
- **Файл регистрации:** Claude мержит в `~/.claude/settings.json`; Codex мержит в `~/.codex/hooks.json` (формат `{"hooks":{Event:[{"hooks":[{type,command,timeout}]}]}}`). MARKER `bin/jarvis-hook` агент-нейтрален → та же машина backup/atomic/merge/remove работает для обоих, `is_ours()` без изменений.
- **Шим:** переименовать `bin/claude-shim` → `bin/agent-shim`, диспетчеризовать по `name=$(basename "$0")`; ставить один скрипт под двумя именами `~/.jarvis/shims/{claude,codex}`. Резолв реального бинаря — `command -v "$name"`. Passthrough:
  - claude: `-p|--print`, `auth|setup-token`.
  - codex: `exec|e|login|logout|mcp|mcp-server|app-server|completion|doctor|--version|--help`.
- **Hook-trust (решение: bypass).** Codex-ветка шима добавляет `--dangerously-bypass-hook-trust` при оборачивании интерактивного `codex` (флаг — top-level, проверено). Прокидывается перед прочими аргументами. Для прозрачности: документируем риск в README.
- **Прокси.** Шим Codex НЕ синхронизирует `ANTHROPIC_BASE_URL` (Codex читает прокси из `config.toml [network]`). Generic `HTTP(S)_PROXY` sync безвреден — оставляем общий `NET_VARS` минус Anthropic-специфика, либо per-agent `NET_VARS`.
- **`codex_found()` / status / uninstall / onboarding-фаза** «Codex CLI». Онбординг показывает по карточке на обнаруженный бэкенд (параметризуем `renderIntegrationCard`).

### 4.2 Ингест событий (daemon.rs)

- **Ключ реестра → `(agent, session_id)`.** Меняем `HashMap<String, Session>` → ключ-строка `format!("{}\u{1}{}", agent.label(), sid)` (или составной тип). Затрагивает все `sessions.entry/get` (механически). Защищает от коллизий UUID-пространств; `evict_pane` остаётся по `tmux_pane`.
- **`s.agent`** ставится из конверта (есть) и **типизируется** через `Agent::from_label`.
- **`s.model` из payload** для Codex: в общем извлечении (601-641) если `payload.model` есть — ставить `s.model` (с учётом `model_at`-guard). Для Claude поле в payload отсутствует — поведение прежнее (майнинг из транскрипта).
- **Новые внутренние события:**
  - `permission` → `Status::Waiting` (как `notification`; текст по умолчанию «Codex ждёт подтверждения»).
  - `subagent-start`/`subagent-stop` → та же логика, что Claude `Task` pre/post-tool (board/subagents), но из выделенных событий; читать `payload.agent_type`/`agent_id`.
- **Tool-name маппинг.** Codex шлёт Claude-образные `tool_name` (Bash, apply_patch) → ветка generic-активности (711-717) работает as-is. `AskUserQuestion/TodoWrite/Task` Codex не шлёт → ветки просто не срабатывают (graceful).
- **`source`** на SessionStart у Codex есть (`startup/resume/clear/compact`) → существующая ветка `compact` (559,649) совместима.
- **`session-end`:** у Codex нет → живость только по `pid_alive($PPID)` (reconcile уже это умеет, 1250-1259).

### 4.3 Парсер транскрипта Codex (backend/codex/transcript.rs)

Вход: rollout `.jsonl`, строки `{timestamp,type,payload}`.
- `session_meta` (строка 1): `payload.id`, `payload.cwd`, `payload.git.branch`, `payload.model_provider`.
- `turn_context`: `model`, `effort`, `turn_id`.
- `response_item`: `payload.type` ∈ `message`(role,content[].type input_text/output_text), `reasoning`, `function_call`(name,arguments,call_id), `function_call_output`, `custom_tool_call`(apply_patch).
- `event_msg`: `task_complete.last_agent_message`, `token_count`.

Маппинг в **тот же `ChatItem`** (`role`, `kind`, `text`, `ts`): user/assistant message → text; function_call → tool-label (через общий `short_tool_label`, имена Codex уже Claude-образные); reasoning — пропускаем (или kind="reasoning", скрыт). `extract_model` = последний `turn_context.model`; `extract_branch` = `session_meta.git.branch`; `extract_title` = `session_index.jsonl thread_name` по id (или первая user-реплика). `full_final_reply` = последний assistant message / `task_complete.last_agent_message`. `transcript_dir_for(cwd)` — индекс по session_meta (Codex не кодирует cwd в путь): строится при `scan`/lazily из `session_index.jsonl` + чтение первой строки rollout. Линейный лог — `read_chain` для Codex = просто чтение (без uuid/parentUuid).

### 4.4 Контроль (ipc.rs, tmux.rs, backend)

- **Reply** — без изменений (tmux транспорт нейтрален). Codex-сессия в `-L jarvis` → `$TMUX_PANE` есть → `reply` печатает.
- **resume_cmd:** Claude `claude --resume {id}`; Codex `codex resume {id}` (UI-подсказка `tmux_needed`, ipc.rs:28-30, и история renderer.js:2238).
- **set_model:** Claude — slash `/model {m}` + confirm-regex. Codex TUI — `/model`-пикер (модель+reasoning в одном селекторе); реализуем как paste `/model` + навигация по пикеру под Codex (новая `paste_slash`-вариация/`answer`-хореография за бэкендом). Headless-путь (для внутреннего Codex) — просто `-m`/`-c` аргумент, без tmux.
- **set_effort:** Claude — `/effort {lvl}`. Codex — отдельного `/effort` НЕТ; effort = часть `/model`-пикера или `-c model_reasoning_effort` (для headless). Для интерактива: либо вшиваем в `/model`-хореографию, либо помечаем «effort для Codex меняется вместе с моделью» (UI скрывает отдельный effort-пикер для codex-сессии). Решение: **UI прячет отдельный effort для Codex; модель+effort одним пикером** (соответствует Codex UX).
- **answer_question:** Claude-хореография (digits/Right/Submit, tmux.rs:151-164) — за бэкендом; Codex approval/selection UI другой → отдельная реализация (или, если Codex не шлёт `AskUserQuestion`, вопрос-карточки для Codex не возникают, и метод — no-op/Permission-flow).
- **friendly_model/models/effort_levels** — per-agent: Codex модели (`gpt-5.x`, `gpt-5-codex` → «GPT-5», «Codex»), effort `minimal/low/medium/high/xhigh`.

### 4.5 Usage / limits (usage.rs, limits.rs, backend)

- **Скан Codex:** `scan_usage` для Codex тейлит `~/.codex/sessions/**/*.jsonl`, ищет `event_msg.token_count` → `info.total_token_usage` (input/cached/output/reasoning) + `model` + `cwd`(из session_meta) + `session_id` + `ts`. Маппинг в общий `Tok` (zero-fill отсутствующих cache-полей). Pre-filter substring `token_count` (аналог claude `"usage"`).
- **Агрегация/окно/`stats()`** — общие, без изменений (byModel/byProject/byBilling/sessions/window).
- **Прайсинг:** `price()` per-agent — таблица OpenAI для gpt-5.x (оценка $/1M, помечаем как оценку, конфиг-таблица). `friendly_model_or_other` whitelist — backend-supplied.
- **official_info → Option per-agent.** Codex: синтез из последнего `event_msg.token_count.rate_limits.primary` (`used_percent`, `resets_at`) + `secondary` → `PctReset`. **Фикс бага по умолчанию:** в `limits.rs:78` `map_or(true,…)` → при отсутствии official данных НЕ считать любой rate_limit подтверждённым лимитом (иначе Codex без official ложно покажет баннер). Политика: при `official=None` доверяем `classify_failure` только при явном rate_limit-маркере, без авто-баннера.
- **`fetch_official` (claude `-p /usage`)** остаётся Claude-only; для Codex — `scan`-производное.

### 4.6 Внутренний Codex: service-LLM + agent-host (claude_bin.rs → service.rs, agent/mod.rs)

- **Service-LLM (саммари/переводы).** Обобщаем `run_haiku`/`run_claude` → `run_service_llm(agent, prompt, timeout)`. Настройка `internalBackend: "auto"|"claude"|"codex"` (default `auto` = Claude при наличии, иначе Codex). Codex-путь: `codex exec --ignore-user-config -c model=<cheap> -c model_reasoning_effort=minimal --json -C <tmp> "<system+prompt>"` → парсим финальный `agent_message.text`. `--append-system-prompt` у Codex нет → HAIKU_SYSTEM вшиваем в начало промпта. `JARVIS_IGNORE=1` всё ещё ставим (шим). Прокси: `--ignore-user-config` теряет `[network]` → читаем `proxy =` из `~/.codex/config.toml` (одна regex-строка) и прокидываем `-c network.proxy="…"` если есть.
- **Agent-host (gated).** `CodexCliHost` рядом с `ClaudeCliHost`. Запуск:
  `codex exec --json --ignore-user-config -s read-only -c model=<m> -c mcp_servers.jarvis.command="<bin>" -c mcp_servers.jarvis.env.JARVIS_TOKEN="<tok>" -C <tmp> "<msg>"`, env `JARVIS_SOCK`. **`--ignore-user-config` = превентивная замена INV-TOOLS** (нет чужих MCP/скиллов). `parse_agent_line` маппит `thread.started→Init{session_id=thread_id}`, `item.completed{agent_message}→Delta/Done`, `item.*{mcp_tool_call|command_execution}→ToolUse`, `turn.completed→Done`. INV-TOOLS-инвариант для Codex заменяется на: (а) `--ignore-user-config`, (б) `-s read-only`, (в) только `mcp_servers.jarvis`. Если в потоке появляется не-jarvis tool-call — логируем/убиваем (defense-in-depth). resume: `codex exec resume <thread_id>`.

### 4.7 UI (renderer.js, index.html, onboarding.js)

- **Agent-бейдж:** отдельный пилл перед моделью, рендерим только когда `s.agent!=="claude"` (избегаем загромождения). CSS `.badge.agent` (примитивы есть).
- **Per-agent model/effort:** `MODELS_BY_AGENT`, `effortsFor(model, agent)`; `app_meta` отдаёт `effortLevels` per-agent (или `effortLevelsByAgent`). Для Codex отдельный effort-пикер скрыт (см. §4.4).
- **Темплейтизация строк** «Claude Code»/`claude`/`'claude'`-фолбэков (таблица из ресёрча: renderer.js:239/667/682/696/2238/1439/2656/2668, onboarding.js:12-13/104, onboarding.html:167, index.html:1432).
- **Settings:** `defaultBackend` (top-level) и блок `agents` через `set_block` (per-agent enable). `internalBackend` для §4.6.
- **Limit-баннер / stats** — лейбл провайдера per-agent.

## 5. Изменения модели данных

- `Session.agent: Option<String>` → семантически `Agent` (на проводе остаётся строка camelCase для UI-совместимости; в Rust — типизированный аксессор). Никаких новых обязательных полей.
- Реестр-ключ `(agent, session_id)`.
- Settings: `defaultBackend`, `internalBackend`, `agents.{codex}.enabled`.

## 6. План инкрементов (для writing-plans)

0. **Скелет:** `Agent` enum + `Backend` трейт + диспетчер + `ClaudeBackend` (перенос текущего поведения 1:1, тесты зелёные, поведение Claude неизменно).
1. **Provisioning:** per-agent EVENTS/hooks-file/label; `agent-shim` basename-диспатч + codex passthrough + bypass-hook-trust; `codex_found`/status/uninstall/onboarding-фаза. Фикс label-бага. → Codex-сессии появляются в панели с правильной меткой.
2. **Ингест:** `(agent,sid)`-ключ; `s.model` из payload; события `permission`/`subagent-*`; типизация `agent`.
3. **Транскрипт Codex:** rollout→`ChatItem` + title/branch/model + `transcript_dir_for`. → чат/саммари/тосты/голос для Codex.
4. **Контроль + UI:** resume_cmd; model/effort per-agent (Codex — модель+effort одним пикером); answer-question; agent-бейдж; per-agent model/effort vocab; темплейтизация строк.
5. **Usage/limits:** Codex usage-скан + rate_limits→official `Option`; прайсинг; фикс `map_or(true)`; per-agent лейблы stats.
6. **Внутренний Codex:** `run_service_llm` + `internalBackend` setting; `CodexCliHost` agent-host с превентивной изоляцией.
7. **Docs + тесты:** README RU→EN (зеркало в том же коммите), THIRD-PARTY/notes; юнит-тесты парсеров (фикстуры реальных rollout-строк), build_args, hook-install merge.

Каждый инкремент компилируется и оставляет Claude-поведение прежним.

## 7. Стратегия тестирования

- **Юнит (без живого процесса):** парсер rollout Codex (фикстуры из реальных `~/.codex/sessions` строк) → `ChatItem`; `parse_agent_line` Codex (фикстуры `thread.started/item.completed/turn.completed`); `build_args` Claude/Codex; per-agent hook-install merge (claude settings.json + codex hooks.json); `Agent::from_label`; usage-extract Codex `token_count`.
- **Инвариант:** существующие тесты agent/mod.rs, transcript, usage остаются зелёными (регресс Claude).
- **Ручная проверка (smoke):** запустить интерактивный `codex` под dev-Jarvis (шим+хуки в `~/.jarvis-dev`), убедиться: сессия в панели как `codex`, тост на Stop, чат рендерится, reply вставляется, смена модели через пикер, usage растёт.
- **Безопасность agent-host:** тест, что `codex exec` запускается с `--ignore-user-config` и без сторонних MCP (нет посторонних tool-call в потоке).

## 8. Риски и открытые вопросы

1. **TUI-хореография Codex (`/model`-пикер, approval).** Точные keystrokes неизвестны без живой проверки; инкремент 4 включает калибровку под реальный Codex TUI. Риск: пикер-разметка меняется между версиями Codex.
2. **`--ignore-user-config` теряет прокси.** Решение — читать `[network] proxy` из config.toml и прокидывать `-c`. Если формат поменяется — service-LLM Codex падает в фолбэк (не критично, есть Claude).
3. **Codex usage прайсинг — оценочный.** Таблица $/1M помечается как оценка, выносится в конфиг.
4. **Hook-trust bypass** снижает безопасность (хуки Jarvis запускаются без trust-подтверждения). Принято пользователем; документируем.
5. **Версионность Codex** (0.135 → 0.142 доступна): поля payload/rollout/флаги могут дрейфовать. Парсеры — дефенсивные (unknown→skip), как Claude.
6. **`(agent,session_id)` ключ** трогает много call-site — механический, но широкий рефактор; делаем в инкременте 2 атомарно.
7. **Смешанный список claude+codex** — бейдж различает; сортировка/пины не затронуты.

## 9. Обратная совместимость

- Существующие Claude-сессии: ключ становится `claude\u{1}<sid>` — прозрачно (старый `state.json` мигрирует при загрузке: при отсутствии метки считаем `claude`).
- `~/.claude/settings.json` хуки — без изменений по содержимому (метка `claude` уже там).
- `~/.codex/hooks.json` — Jarvis перетирает ручной файл корректной меткой `codex` (бэкап делается).
- Уже работающая команда `npm run setup` начинает ставить и Codex-интеграцию (если `codex` найден); `teardown` снимает обе.
