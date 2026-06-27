# Дизайн: внешний ассистент (веб-поиск, команды, подробные ответы)

Дата: 2026-06-27
Статус: на ревью
Под-проект 4 дорожной карты «Jarvis как голосовой ассистент». Строится поверх
2a/2b (разговорный мозг: `convo/`), переиспользует confirm-инфру.

## Цель
Голосовой Джарвис умеет не только работать со своими данными (сессии/роут/
управление из 2a/2b), но и с ВНЕШНИМ миром: искать в интернете, отвечать подробно
на произвольные вопросы, выполнять команды/задачи. То есть «Hey Jarvis, найди в
интернете X», «объясни Z подробно», «сделай Y» → Джарвис исполняет и отвечает голосом.

Решения брейншторма:
- **Поиск/ответы — свободно (read-only), команды/сайд-эффекты — только через
  подтверждение** (карточка Да/Отмена с точным текстом команды). Микрофон недоверен.
- **Полный ответ озвучивается целиком** (прерывается крестиком/«стоп» — у нас уже есть).
- **Подход A:** полноценный Claude Code агент (веб-поиск + инструменты) +
  перехват разрешений через `--permission-prompt-tool` в наш confirm.

## Что уже есть (выверено по коду ветки integration)
- `agent::ClaudeCliHost` спавнит `claude` через `build_args`, но ЖЁСТКО заперт:
  `--allowedTools mcp__jarvis__*` + `inv_tools_ok` убивает агента при ЛЮБОМ
  не-jarvis инструменте (`agent/mod.rs:159,193`). Для внешнего ассистента нужен
  ДРУГОЙ хост (без INV-TOOLS-замка, с веб/тулзами).
- `bin/jarvis-mcp.rs` — MCP-сервер, проксирующий `mcp__jarvis__*` в реестр
  капабилити демона через unix-сокет (`JARVIS_SOCK`). Сюда добавим permission-тул.
- `convo/`: планировщик (`plan.rs`, `build_plan_prompt`+`parse_plan`), `skills.rs`
  (меню + `dispatch` + валидация), `mod.rs` (`converse_turn`), `vconfirm`+confirm
  (`route::hud::Phase::Confirm`, IPC `voice_confirm_resolve`), `speak_blocking` +
  `voice.stop()` + крестик-abort (`voice_abort`).
- `claude_bin::resolve_claude_bin()` + env (прокси сохраняется), `run_claude`.

## Архитектура (подход A)

### Триаж (когда зовём ассистента)
Планировщик (`convo/plan.rs`) уже выдаёт `Plan{speak, action?}`. Добавляем скил
`assistant{query}` в меню. Когда запрос НЕ внутренний (не вопрос про сессии, не
роут, не управление) — Haiku ставит `action=assistant`, `query` = суть запроса.
`dispatch` исполняет ассистента.

### AssistantHost (новый, `agent/assistant.rs`)
Спавн `claude` (НЕ ClaudeCliHost — там INV-TOOLS-замок). Флаги:
- `-p <query>`, `--output-format stream-json --verbose` (как ClaudeCliHost, парсим
  тем же `agent::parse_stream_line`).
- `--model` дефолт Claude Code (Sonnet) — веб/тул-оркестрация Haiku не по силам.
- `--allowedTools` = READ-набор авто-разрешённых: `WebSearch WebFetch Read Grep Glob`
  (выполняются без запроса → быстро).
- `--permission-prompt-tool mcp__jarvis__permission` — для ЛЮБОГО инструмента вне
  allowedTools (Bash/Write/Edit/…) claude зовёт наш permission-тул за разрешением.
- `--mcp-config <jarvis-mcp.json>` (наш MCP-сервер с permission-тулом).
- `--permission-mode default`, `--setting-sources project,local`, рабочая папка —
  ИЗОЛИРОВАННАЯ скретч-папка (`~/.jarvis[-dev]/assistant-cwd`), не корень проекта.
- Env как у ClaudeCliHost: `JARVIS_SOCK`, `JARVIS_IGNORE=1`,
  `DISABLE_NON_ESSENTIAL_MODEL_CALLS=1`; прокси наследуется.
Стримит события, собирает финальный текст (`AgentEvent::Done.result` или
конкатенация `Delta`), возвращает строку для озвучки.

### Permission-MCP-капабилити `permission`
Контракт Claude Code `--permission-prompt-tool`: на запрос инструмента claude
зовёт MCP-тул с `{tool_name, input}` и ждёт ответ вида
`{"behavior":"allow","updatedInput":<input>}` или `{"behavior":"deny","message":"…"}`.
(ТОЧНУЮ форму ответа — выверить в реализации против текущего claude CLI; см.
Открытые вопросы.)
Хендлер (`capability/native/permission.rs` + регистрация в `jarvis-mcp`):
- `tool_name` ∈ READ-allowlist (на всякий, хотя read и так в allowedTools) → allow.
- сайд-эффект (Bash/Write/Edit/NotebookEdit/…) → показать confirm-карточку
  (`vconfirm` + `Phase::Confirm{nonce, text}`, текст = `tool_name` + краткий input,
  напр. `Bash: rm -rf …`) → дождаться → allow/deny.
- неизвестный/непонятный → **deny** (fail-closed).

### Озвучка/HUD
Финальный текст ассистента → `speak_blocking` (целиком, прерывается крестиком/×) +
`Phase::Reply{text}` в HUD. Во время работы — `Phase::Thinking{text:"ищу…"}`.

## Модель доверия (микрофон недоверенный)
- Reads (веб/чтение) — свободно. Сайд-эффекты — ТОЛЬКО через карточку Да/Отмена с
  точным текстом команды (человек видит, что выполнится). permission-тул дефолт — deny.
- Рабочая папка ассистента — изолированный скретч, не репозиторий.
- Веб-контент и реплика с мика — недоверенные данные; инъекция может заставить
  агента ПОПРОБОВАТЬ команду, но она упрётся в карточку (точный текст виден).
- permission-тул резолвится из тоста (как `voice_confirm_resolve`), агент не может
  сам себя одобрить (in-process IPC, не на MCP-сокете для резолва).

## Этапы (TDD, инкрементально)

**4a — поиск + подробные ответы (read-only), безопасно, plan-ready:**
1. `agent/assistant.rs` — `AssistantHost` с READ-набором (`--allowedTools WebSearch
   WebFetch Read Grep Glob`), БЕЗ permission-тула (сайд-эффекты просто недоступны),
   скретч-cwd, стрим → финальный текст. Сборка флагов — чистая функция (тест).
2. Скил `assistant{query}` в `skills.rs`/`plan.rs` + `dispatch` → AssistantHost →
   текст → озвучка (`speak_blocking`) + `Phase::Reply`. HUD «Ищу…».
3. Триаж: промпт-меню учит Haiku ставить `assistant` на общих/внешних запросах.
   **Поставляемо:** «найди в интернете / объясни подробно».

**4b — выполнение команд через permission-tool + карточку (опасное):**
4. `capability/native/permission.rs` + регистрация в `jarvis-mcp` — permission-тул
   (read→allow; side-effect→confirm-карточка; неизвестное→deny). Чистый
   классификатор read-vs-side-effect (тест).
5. AssistantHost: добавить `--permission-prompt-tool mcp__jarvis__permission` +
   полный тул-набор (Bash/Write/Edit). Confirm-карточка показывает точную команду.
6. Безопасность-тесты: side-effect недостижим без confirm; deny по умолчанию;
   резолв permission только in-process.

Каждый этап — рабочий коммит. После 4a — голосовой веб-ассистент; после 4b —
+ выполнение команд с подтверждением.

## Тестирование
- Сборка флагов AssistantHost — чистая (read-набор в 4a; +permission-prompt-tool в 4b).
- Триаж: `assistant` появляется в меню; `parse_plan` ловит `assistant{query}`.
- permission-классификатор read-vs-side-effect (чистый, юнит): WebSearch→allow,
  Bash→confirm, неизвестное→deny.
- Смоук: фейковый агент → финальный текст озвучен; ветка confirm read→allow,
  команда→confirm(мок)→allow/deny.
- Безопасность: команда без confirm не выполняется; deny-default; резолв вне MCP.

## Вне scope
Персистентная память внешних задач между разговорами; параллельные ассистент-
задачи; не-Claude бэкенды ассистента; тонкая настройка модели/инструментов из UI.

## Открытые вопросы (выверить в реализации)
1. ТОЧНЫЙ контракт `--permission-prompt-tool` текущего `claude` CLI: имя тула,
   форма запроса (`tool_name`/`input`) и ответа (`behavior: allow|deny`,
   `updatedInput`/`message`). Сверить с CLI/докой до 4b.
2. Достаёт ли финальный ответ из stream-json надёжно (`Done.result` vs склейка
   `Delta`); как ограничить чрезмерно длинную озвучку (озвучиваем целиком, но
   агент должен давать voice-friendly ответ — инструкция в промпте).
3. Доступность `WebSearch`/`WebFetch` в текущей установке `claude` (включены ли);
   фолбэк, если веб-инструментов нет.
4. Изоляция скретч-cwd и нужные `--setting-sources`, чтобы агент не тянул чужие
   проектные MCP/хуки и не писал в репозиторий.
