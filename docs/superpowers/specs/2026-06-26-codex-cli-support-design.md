# Spec: Codex CLI как первоклассный бэкенд Jarvis (наравне с Claude Code)

> Дата: 2026-06-26 · Статус: v2 (после 4 независимых ревью) · Effort: ultracode
> Цель: обернуть **Codex CLI** (`codex`, OpenAI) так же полно, как сейчас обёрнут **Claude Code** (`claude`), через общую абстракцию, без дублирования фич.
>
> **v2 changelog (по итогам ревью):** убран компаунд-ключ `(agent,sid)` (YAGNI + ломает restore_state/evict_pane/IPC); НЕ трогаем `limits.rs:78` (регресс Claude); трейт `Backend` разделён на sync-dyn-safe + free-fn dispatch (async несовместим с `dyn`); прокси-модель исправлена (egress = env-vars, не `config.toml [network]`); `--ignore-user-config` НЕ глушит skills → agent-host на чистом throwaway `CODEX_HOME` + **обязательный** per-item kill; hook-trust bypass — **условный + feature-detect**; добавлены пропущенные швы (`tail.rs`, `history.rs`, `commands_catalog.rs`, `wakeword/action.rs`, ~6 call-sites транскрипта); срезаны `defaultBackend` и обязательный OpenAI-прайсинг.

## 1. Цели и не-цели

**Цели (объём максимальный — выбор пользователя):**
1. **Мониторинг** интерактивных Codex-TUI сессий: панель, тосты, голосовые саммари, просмотр чата, статус/живость.
2. **Контроль**: вставка ответов (reply), смена модели/effort, ответ на вопрос, resume.
3. **Usage-статистика** Codex: токены/квота из rollout `token_count`/`rate_limits` (прайсинг $ — опциональная оценка).
4. **Внутренний Codex**: `codex exec` для собственных саммари/переводов (service-LLM) и для встроенного gated agent-chat (с усиленной изоляцией).

**Не-цели:**
- Мониторинг headless `codex exec` — **невозможен через хуки** (эмпирически: `exec` не дёргает `hooks.json`; хуки только в TUI). Вне scope.
- Никаких изменений поведения Claude — байт-в-байт прежнее (инвариант на каждом инкременте).
- Не macOS-only специфику не трогаем.

## 2. Ключевые факты-ограничения (эмпирически проверены, в т.ч. на ревью)

| Факт | Следствие |
|---|---|
| `codex exec` НЕ шлёт хуки; интерактивный `codex` (TUI) шлёт | Мониторинг = только интерактивные сессии |
| Хук-payload Codex **Claude-совместим**: `session_id`,`cwd`,`transcript_path`,`tool_name`,`tool_input`; `$PPID`=pid codex | Ядро ингеста daemon.rs работает почти без изменений |
| Payload несёт `model`,`turn_id`,`permission_mode` (на ВСЕХ событиях); Stop → `last_assistant_message` (НЕТ `payload.message`); `source` только SessionStart | `s.model` из payload (только Codex, с guard); waiting — через `PermissionRequest` |
| Codex-события (10): PreToolUse, **PermissionRequest**, PostToolUse, PreCompact, PostCompact, SessionStart, UserPromptSubmit, **SubagentStart/Stop**, Stop. НЕТ Notification/StopFailure/SessionEnd | Codex не триггерит claude-путь лимитов/Notification; live-детект иной |
| `~/.codex/hooks.json` существует, написан вручную с меткой `claude` | Нужен **новый writer** на `~/.codex/hooks.json` с меткой `codex` (не «фикс», install/mod.rs только Claude-settings писал) |
| Hook-trust: `config.toml [hooks.state]` sha256, прообраз не воспроизводится | Свежие машины: шим инжектит `--dangerously-bypass-hook-trust` **условно** (если наш хук ещё не доверен) + **feature-detect** флага |
| Rollout: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`, строки `{timestamp,type,payload}` (session_meta/turn_context/response_item/event_msg) | Отдельный парсер за общим `ChatItem` |
| Контроль: `-m`, `-c model_reasoning_effort=<minimal\|low\|medium\|high\|xhigh>`, `codex resume <id>`; **нет отдельного `/effort`** — «Switch models or reasoning effort with /model» | UI: для Codex модель+effort одним пикером |
| Usage: нет `/usage`; квота инлайн `event_msg.token_count.rate_limits.{primary,secondary}` (`used_percent`,`window_minutes`,`resets_at`,`plan_type`) | `official_info` per-agent (Option); Codex синтезирует при `scan` |
| `codex exec --json`: thread.started/turn.started/item.{started,completed}/turn.completed; init **без `tools[]`**; item.type agent_message/reasoning/command_execution/mcp_tool_call | INV-TOOLS не воспроизводим на init → **per-item kill** |
| **ПРОКСИ:** `config.toml [network] proxy` = sandbox-MITM, НЕ egress. API-egress = env `HTTP(S)_PROXY` (reqwest) — шим уже синкает | Никакого `-c network.proxy`; env-прокси наследуется |
| `--ignore-user-config` глушит чужие **MCP**, но **НЕ skills** (FS-discovery из `$CODEX_HOME/skills/`) | Agent-host: чистый throwaway `CODEX_HOME` без skills, не `--ignore-user-config` |

## 3. Архитектура

### 3.1 Принцип: один модуль-шов

Код уже ~90% агент-нейтрален: ключ сессии = `payload.session_id`; `Session.agent` есть (model.rs:129), но не читается; бейдж UI (renderer.js:239) падает в `s.agent`; tmux печатает в любой TUI. Вводим `enum Agent { Claude, Codex }` и сосредотачиваем бэкенд-специфику в новом модуле `src-tauri/src/backend/`.

### 3.2 Форма абстракции (исправлено по ревью C1)

`async fn` в `dyn`-трейте несовместимы, а этот код намеренно не тянет `#[async_trait]` (только free async fn — claude_bin.rs:48/85 — и ручной `Pin<Box<dyn Future>>` — confirm.rs:14). Поэтому шов = **две части**:

**(A) sync, dyn-safe `trait Backend`** — чистые данные/форматирование, диспетчер `fn backend(a: Agent) -> &'static dyn Backend` (`ClaudeBackend`/`CodexBackend` — ZST):
```rust
pub trait Backend: Send + Sync {
    fn agent(&self) -> Agent;
    fn hook_events(&self) -> &'static [(&'static str, &'static str)];  // (ИмяСобытияАгента, внутр-arg)
    fn hooks_path(&self) -> PathBuf;            // ~/.claude/settings.json | ~/.codex/hooks.json
    fn cli_found(&self) -> bool;
    fn shim_passthrough(&self, argv1: Option<&str>) -> bool;
    fn to_chat_items(&self, entry: &Value) -> Vec<ChatItem>;
    fn read_entries(&self, file: &Path, max_bytes: u64) -> Vec<Value>; // claude: read+chain; codex: read
    fn extract_title(&self, entries: &[Value]) -> Option<String>;
    fn extract_branch(&self, entries: &[Value]) -> Option<String>;
    fn extract_model(&self, entries: &[Value]) -> Option<String>;
    fn transcript_dir_for(&self, cwd: &str) -> Option<PathBuf>;
    fn resume_cmd(&self, sid: &str) -> String;
    fn friendly_model(&self, id: &str) -> String;
    fn models(&self) -> &'static [(&'static str, &'static str)];
    fn effort_levels(&self) -> &'static [&'static str];
    fn has_separate_effort(&self) -> bool;       // claude:true, codex:false
    fn price(&self, model: &str) -> (f64, f64);  // оценка/конфиг
}
```

**(B) free-функции, диспетчеризуемые `match agent`** — async/stateful, в своих модулях (как `claude_bin.rs`):
- Provisioning: `install_hooks(agent, hook_bin)`, `uninstall_hooks(agent)`, `hooks_status(agent)` (в install/mod.rs).
- Control: `set_model(agent,d,sid,model).await`, `set_effort(...)`, `answer_question(...)` (в ipc.rs/tmux.rs).
- Service-LLM: `run_service_llm(agent, prompt, timeout).await` (в service.rs).
- Usage: `scan_usage(agent, &mut state)`, `official_info(agent)` — методы `Usage` (state живёт в синглтоне, не в ZST — ревью S2).
- Agent-host: `CodexCliHost` рядом с `ClaudeCliHost`; `parse_agent_line(agent, line)`.

### 3.3 Что остаётся общим (НЕ форкается)

Редьюсер/реестр/статус-машина daemon.rs (ключ = **bare sid**, eviction, reconcile, эффекты, push/persist); `Session`+`Status`; контракт `ChatItem`; `AgentEvent`; tmux-транспорт (reply/буферы/send-keys/list_panes_meta/focus/ping); голос/STT/wakeword-движок; capability/MCP (`jarvis-mcp`+токен); settings-store; рендер панели; агрегация usage (`Tok/HourAgg/SessionAgg/WindowAgg`, 5h-окно, offsets, `stats()`); `parse_ts`, `squeeze_reply`; `jarvis-hook` (уже `$1`=agent).

### 3.4 Ключ реестра — остаётся `sid` (ревью C3/S1/O3)

Компаунд-ключ `(agent,sid)` **отклонён**: оба агента — UUID (коллизия астрономически невероятна), а IPC/UI повсюду оперируют голым `session_id` (ipc.rs:301/331/381/…; capability/native/*; confirm_panel; limits; screen_prompt; main.rs:290). Менять ключ → ломать `session()/with_session()` (daemon.rs:515/533), `restore_state` (insert по голому id, daemon.rs:256), `evict_pane` (сравнение ключа, daemon.rs:1704/1708) и тащить `agent` через весь IPC. `Session.agent` уже хранится — диспетч по нему **после** lookup, бесплатно. Ключ = `sid`.

## 4. Детальный дизайн

### 4.1 Provisioning (install/mod.rs, bin/agent-shim, onboarding)

- **EVENTS per-agent** (`backend(a).hook_events()`): Claude — 8 как сейчас; Codex — `SessionStart→session-start`, `UserPromptSubmit→prompt`, `PreToolUse→pre-tool`, `PostToolUse→post-tool`, `Stop→stop`, `PermissionRequest→permission`, `SubagentStart→subagent-start`, `SubagentStop→subagent-stop`.
- **Метка хука:** `format!("{} {} {arg}", hook_bin, agent.label())` — фикс литерала `claude` на **install/mod.rs:1003** (не :832 — ревью S4).
- **Writer для `~/.codex/hooks.json`:** формат идентичен тому, что строит claude-writer (install/mod.rs:991-1006) — `{"hooks":{Event:[{"hooks":[{type,command,timeout}]}]}}`. Переиспользуем `event_installed`/`is_ours`(MARKER=`bin/jarvis-hook`, label-agnostic)/backup/atomic/merge. Параметризуем путь+EVENTS+label. Бэкап ручного файла делается.
- **Шим `bin/claude-shim`→`bin/agent-shim`** (правит `include_str!` install/mod.rs:22 + тест :1391): диспетч по `name=$(basename "$0")`, ставится под `shims/claude` и (если `codex_found()`) `shims/codex`. Резолв реального — `command -v "$name"`. Passthrough проверяем по **`argv[1]`** (ревью CONSIDER): claude `-p/--print`,`auth/setup-token`; codex `exec|e|login|logout|mcp|mcp-server|app-server|completion|doctor|resume|fork|apply|review|cloud|features|update|sandbox`.
- **Hook-trust bypass (условно + feature-detect — ревью C3/S5):** codex-ветка шима добавляет `--dangerously-bypass-hook-trust` **только если** наш хук ещё не доверен (проверка наличия `[hooks.state]`-записи в `~/.codex/config.toml`) **и** флаг существует (`codex --help | grep`). Иначе запуск без флага (trust для чужих хуков не отключаем). Прокидываем перед позиционным промптом; проверяем приём и до, и после сабкоманды (`codex resume <id>`).
- **Прокси:** codex-шим НЕ синкает `ANTHROPIC_BASE_URL`; generic `HTTP(S)_PROXY`/`ALL_PROXY`/`NO_PROXY` синкаются (egress codex — именно эти env). `NET_VARS` делаем per-agent (Codex без Anthropic-переменной).
- **`codex_found()`/status/uninstall/onboarding-фаза «Codex CLI».** Карточка интеграции — по одной на обнаруженный бэкенд (параметризуем `renderIntegrationCard`). Статус честный: «установлено, но не доверено» если bypass не активен и trust отсутствует (ревью C-a).

### 4.2 Ингест (daemon.rs)

- **Ключ = bare sid** (без изменений). `s.agent` ставится из конверта (есть, 609-611) и типизируется `Agent::from_label`.
- **`s.model` из payload — ТОЛЬКО для Codex, с guard** (ревью S2): `if agent==Codex { if payload.model && (model_at стар/пуст) { s.model=friendly } }`. Claude-ветка не трогается (майнинг из транскрипта в refresh_meta).
- **Новые внутр-события:** `permission`→`Status::Waiting` (как notification; текст «Codex ждёт подтверждения»); `subagent-start`/`subagent-stop`→ та же board/subagents-логика, что Claude `Task` pre/post-tool, читая `payload.agent_type`/`agent_id`.
- **Tool-name:** Codex шлёт Claude-образные имена (Bash/apply_patch)→generic-ветка (711-717) as-is; AskUserQuestion/TodoWrite/Task Codex не шлёт→ветки не срабатывают (graceful).
- **`source` compact** (559/649) совместим. **Нет session-end у Codex** → живость только `pid_alive($PPID)` (reconcile 1250-1259 уже умеет).

### 4.3 Транскрипт Codex + все потребители парсера (backend/codex/transcript.rs) — ревью C2/G4

Парсер rollout→`ChatItem`: `session_meta`(id,cwd,git.branch,model_provider), `turn_context`(model,effort,turn_id), `response_item`(message role+content[].input_text/output_text; reasoning→скрыт; function_call/custom_tool_call→tool-label через общий `short_tool_label`), `event_msg`(task_complete.last_agent_message, token_count). `extract_model`=последний turn_context.model; `extract_branch`=session_meta.git.branch; `extract_title`=`~/.codex/session_index.jsonl thread_name` по id (или первая user-реплика); `read_entries`=просто чтение (лог линейный). `transcript_dir_for(cwd)` — ленивый индекс из `session_index.jsonl`+первая строка rollout.

**`full_final_reply` для Codex предпочитает `payload.last_assistant_message` из Stop-хука** (ревью S4: rollout может быть не сфлашен на момент Stop), фолбэк — rollout.

**Диспетчеризовать per-agent ВСЕ call-sites парсера** (иначе пусто для Codex): `ipc.rs:305` (chat_open), `tail.rs:13/66` + **сигнатура `TailHandle::start` получает `Agent`** (ipc.rs:313 передаёт `s.agent`), `daemon.rs:910` (`ai_toast_summary`→full_final_reply — **это вход голоса/TTS и тела done-тоста**), `daemon.rs:963-1008` (refresh_meta branch/title/model), `daemon.rs:1152-1156` (dialog summary), `capability/native/chats.rs:43-48` (`chats.read`).

### 4.4 Контроль (ipc.rs, tmux.rs)

- **Reply** — без изменений (tmux нейтрален). Codex в `-L jarvis`→`$TMUX_PANE` есть→печатает.
- **`resume_cmd`** per-agent: Claude `claude --resume {id}`; Codex `codex resume {id}`. Маршрутизировать **все** хардкоды: ipc.rs:29 (tmux_needed), renderer.js:696/699/2238 (включая `--dangerously-skip-permissions`-аналог).
- **`set_model`:** Claude — slash `/model`+confirm-regex. Codex — `/model`-пикер (модель+reasoning). `set_effort`: Claude `/effort`; **Codex — отдельного нет** → UI прячет effort-пикер для codex (`has_separate_effort()==false`), смена effort через тот же `/model`-пикер. Headless (для service/agent-host) — `-m`/`-c`, без tmux.
- **`answer_question`:** Claude-хореография (tmux.rs:151-164) за бэкендом; Codex approval-UI иной → отдельная реализация; если Codex не шлёт AskUserQuestion — карточки-вопроса не возникает, метод no-op (waiting приходит через `permission`).
- **friendly_model/models/effort_levels** per-agent: Codex `gpt-5.x`/`gpt-5-codex`→«GPT-5»/«Codex»; effort `minimal/low/medium/high/xhigh`.

### 4.5 Usage / limits (usage.rs, limits.rs)

- **Скан Codex:** `scan_usage(Codex,…)` тейлит `~/.codex/sessions/**/*.jsonl`, ищет `event_msg.token_count`→`info.total_token_usage`(input/cached/output/reasoning)+model+cwd(session_meta)+sid+ts. Маппинг в общий `Tok` (zero-fill). Pre-filter substring `token_count`. Агрегация/окно/`stats()` — общие; `price()` применяется **с price() правильного агента построчно**.
- **`limits.rs:78` НЕ ТРОГАЕМ** (ревью C2/S3): это Claude-fail-safe, Codex туда не попадает (нет StopFailure). 
- **Лимиты Codex — отдельный путь, не через StopFailure** (ревью G3): при `scan` берём последний `token_count.rate_limits.primary`(used_percent,resets_at)→ синтез `official_codex`. `official_info`/`LimitState` делаем **per-provider** (`official_claude`/`official_codex`, state живёт в `Usage`, не в ZST — ревью S2). Баннер/auto-resume Codex (если делаем) — от порога used_percent при scan, не от хука. **Известное ограничение:** в инкременте лимит-баннер Codex может быть «только индикатор %», без auto-resume — допустимо (Codex auto-resume вне критичного пути).
- **`fetch_official`** (`claude -p /usage`) — Claude-only. Прайсинг $ Codex — оценка/конфиг, помечаем (ревью O5); базово показываем counts+%.

### 4.6 Внутренний Codex: service-LLM + agent-host (исправлено по ревью C1/Codex-correctness)

**service-LLM (саммари/переводы):** `run_service_llm(agent,prompt,timeout)`; настройка `internalBackend: "auto"|"claude"|"codex"` (default `auto`=Claude при наличии, иначе Codex). Заменяем хардкод-гейты `resolve_claude_bin().is_none()` на «доступен ли любой service-бэкенд» (daemon.rs:905/1148 — ревью S1); добавляем `resolve_codex_bin()`. Codex-путь: `codex exec --json -m <model> -c model_reasoning_effort=low -C <tmp> "<HAIKU_SYSTEM + prompt>"` (нет `--append-system-prompt`→вшиваем в промпт), env `JARVIS_IGNORE=1`. Прокси — **env наследуется** (не `-c network.proxy`). **Без `minimal`** (400 при image_gen/web_search). Парсим финальный `agent_message.text`.

**agent-host (gated) — чистый throwaway CODEX_HOME + ОБЯЗАТЕЛЬНЫЙ per-item kill** (ревью C1):
- Изоляция домом, а не `--ignore-user-config` (он не глушит skills): `~/.jarvis/codex-agent-home/` = `auth.json`→symlink на `~/.codex/auth.json` (живой OAuth) + минимальный `config.toml` (только `[mcp_servers.jarvis]` + model), **без `skills/`**. Запуск `CODEX_HOME=~/.jarvis/codex-agent-home codex exec --json -s read-only -c mcp_servers.jarvis.command="<bin>" -c mcp_servers.jarvis.env.JARVIS_TOKEN="<tok>" -C <tmp> "<msg>"`, env `JARVIS_SOCK`,`JARVIS_IGNORE`.
- **Мандаторный kill в `parse_agent_line`/цикле:** любой stream-item `command_execution`/`local_shell`/`exec` ИЛИ `mcp_tool_call` с server≠jarvis → немедленный `child.kill()` (Codex-аналог INV-TOOLS, по item, т.к. init без tools[]). Не опционально.
- `parse_agent_line(Codex)`: `thread.started→Init{session_id=thread_id}`, `item.completed{agent_message}→Delta/Done`, `item.*{mcp_tool_call(jarvis)}→ToolUse`, `turn.completed→Done`. resume: `codex exec resume <thread_id>`.
- **Wakeword** (action.rs:99/111) и `agent_send` (ipc.rs:781) — оба через выбор хоста по `internalBackend` (ревью G5).

### 4.7 Вторичные Claude-связанные поверхности (ревью G1/G2 + строки)

- **History-панель (history.rs):** отдельный сканер, хардкод `claude_dir()/projects` + Claude-JSONL `parse_meta`. Делаем per-agent: Codex-скан `~/.codex/sessions/**` тем же rollout-парсером (переиспользование 4.3). `history_get` (ipc.rs:366) объединяет оба.
- **Палитра команд (commands_catalog.rs):** `BUILTINS`/dirs per-agent. Codex: минимальный набор + `~/.codex/prompts` (если есть); claude-only команды (`/usage`,`/compact`,`/effort`) для codex не показываем. `commands_get` (ipc.rs:329) ветвится по `s.agent`.
- **`screen_prompt.rs`** регэкспы — Claude-TUI; для Codex не матчатся (Codex использует `permission`-хук) — это ОК, фиксируем как «у Codex нет screen-scrape фолбэка для пикера».
- **`detect_effort_levels`** (daemon.rs:1329, скрейп `claude --help`) + глобальный `effort_levels` (daemon.rs:154, app_meta ipc.rs:338) → per-agent (Codex статичный список); ретайр глобала.
- **Темплейтизация строк** (Rust+UI): ipc.rs:29/304, limits.rs:128/229/719, capability descriptions (sessions.rs:22, control.rs:27); UI — renderer.js:239/667/682/696/2238/1439/2656/2668, onboarding.js:12-13/104, onboarding.html:167, index.html:1432.

### 4.8 UI (renderer.js, index.html, onboarding.js)

Agent-пилл перед моделью, **рендерим только при `s.agent!=="claude"`** (и до майнинга модели, чтобы «codex» не показывался как имя модели — ревью CONSIDER); CSS `.badge.agent`. `MODELS_BY_AGENT`, `effortsFor(model,agent)`; для codex effort-пикер скрыт. `app_meta` отдаёт effort per-agent. Settings: `internalBackend` (есть консьюмер), `agents.{codex}.enabled` через `set_block`. **`defaultBackend` — НЕ добавляем** (нет консьюмера — ревью O4). Лимит-баннер/stats — лейбл провайдера per-agent.

## 5. Изменения модели данных

- `Session.agent: Option<String>` → семантически `Agent` (на проводе строка; в Rust типизированный аксессор). Новых обязательных полей нет. **Ключ реестра не меняется.**
- Settings: `internalBackend`, `agents.{codex}.enabled`.

## 6. План инкрементов (для writing-plans)

0. **Скелет:** `Agent` enum + sync-`Backend` трейт + диспетчер + free-fn заглушки; `ClaudeBackend` переносит текущее 1:1 (вкл. `restore_state`/`evict_pane` без изменений ключа). Тесты зелёные, Claude неизменен, компилируется.
1. **Provisioning:** per-agent EVENTS/hooks-file/label (фикс :1003); writer `~/.codex/hooks.json`; `agent-shim` basename-диспатч (argv[1] passthrough) + **условный feature-detect bypass**; `codex_found`/status(«не доверено»)/uninstall/onboarding-фаза; codex-шим ставится только при `codex_found`. → Codex-сессии в панели с меткой `codex`.
2. **Ингест:** типизация `agent`; `s.model` из payload (Codex+guard); события `permission`/`subagent-*`. (Ключ НЕ трогаем.)
3. **Транскрипт Codex + потребители:** rollout→`ChatItem`; `full_final_reply` из payload; диспетч ВСЕХ call-sites (§4.3) + сигнатура `TailHandle::start(agent)`. → чат/тосты/**голос**/саммари для Codex.
4. **Контроль + UI:** `resume_cmd` (вкл. 696/699/2238); model/effort per-agent (codex — один пикер, effort скрыт); answer-question; agent-пилл; per-agent model/effort vocab; темплейтизация строк.
5. **Usage/limits:** Codex usage-скан + per-provider `official`/`LimitState` из rate_limits (НЕ через StopFailure, НЕ трогаем :78); прайсинг-оценка; per-agent лейблы.
6. **Вторичные поверхности:** History per-agent (Codex rollout-скан); палитра команд per-agent; `detect_effort` per-agent + ретайр глобала; остаточная темплейтизация.
7. **Внутренний Codex:** `run_service_llm`+`internalBackend`+`resolve_codex_bin`+гейты 905/1148; `CodexCliHost` (чистый CODEX_HOME + **обязательный per-item kill**); wakeword+agent_send через выбор хоста.
8. **Docs+тесты:** README RU→EN (зеркало в том же коммите; новые секции: риск hook-trust-bypass, онбординг-карточка Codex); юнит-тесты (ниже).

Каждый инкремент компилируется; Claude-поведение неизменно на каждом.

## 7. Тестирование (с учётом ревью)

- **Юнит:** парсер rollout Codex (фикстуры реальных строк)→`ChatItem`; `parse_agent_line(Codex)`; **agent-host kill** на синтетическом `--json` с `command_execution`/чужим `mcp_tool_call` (Codex-аналог INV-TOOLS-теста); hook-install merge (claude settings.json + codex hooks.json, label); `Agent::from_label`; usage-extract Codex `token_count`; `full_final_reply` предпочитает `last_assistant_message` при обрезанном rollout; shim basename-диспатч + passthrough + bypass feature-detect/fallback + отсутствие реального бинаря→127.
- **Регресс-инвариант (Claude byte-for-byte):** существующие тесты agent/mod.rs/transcript/usage зелёные; **Claude limit-баннер при `official=None`+rate_limit StopFailure всё ещё показывается** (анти-регресс :78); Codex не достигает `on_stop_failure`.
- **Smoke (ручной):** интерактивный `codex` под dev-Jarvis (`~/.jarvis-dev`): метка `codex`, тост на Stop, чат рендерится, reply, смена модели пикером, usage растёт.

## 8. Риски и открытые вопросы

1. **TUI-хореография Codex** (`/model`-пикер, approval keystrokes) — калибруется вживую в инк.4; может дрейфовать между версиями.
2. **Agent-host остаточный риск:** `-s read-only` запрещает запись, но **чтение секретов возможно** до срабатывания per-item kill (kill на первом же tool-item — до этого модель могла «подумать», но не выполнить инструмент). Чистый CODEX_HOME убирает skills/чужой MCP; kill убирает shell. Это сильнее, чем «опционально», но слабее claude-INV-TOOLS на init. Если неприемлемо — agent-host Codex можно отложить (service-LLM-половина «внутреннего Codex» самодостаточна).
3. **Прайсинг Codex — оценка** (выносим в конфиг).
4. **Hook-trust bypass** (условный) всё же отключает trust для нашего хука; документируем. Если флаг исчезнет в новой версии Codex — feature-detect не инжектит (шим не падает).
5. **Версионность Codex** (0.135→0.142): payload/rollout/флаги дрейфуют — парсеры дефенсивные (unknown→skip).
6. **History/usage Codex** зависят от наличия rollout-файлов; `--ephemeral`-сессии не пишут rollout → невидимы (ОК, edge).

## 9. Обратная совместимость

- Ключ реестра = `sid` (не меняется) → миграции состояния НЕ требуется; `Session.agent` опционально, отсутствие→`None`→трактуем `claude` при диспетче.
- `~/.claude/settings.json` — без изменений.
- `~/.codex/hooks.json` — Jarvis перетирает ручной файл правильной меткой `codex` (с бэкапом).
- `npm run setup` начинает ставить и Codex-интеграцию (если `codex` найден); `teardown` снимает обе.
