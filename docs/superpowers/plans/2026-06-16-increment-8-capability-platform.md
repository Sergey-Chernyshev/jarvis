# План реализации · Инкремент 8: Слой капабилити и платформа агента

> Спека: `docs/superpowers/specs/2026-06-16-increment-8-capability-platform-design.md`. Дата: 2026-06-16.
> Решения зафиксированы: транспорт — `claude` CLI как хост (§2-bis), топология 3b (chat-MCP через демон), гейт в демоне.
> Принцип: консолидация, не rewrite. Каждая фаза держит `cargo build` + `cargo test` зелёными и коммитится отдельно.

## Привязка к существующему коду (снято с живого кода)

- **Два канала демона:** Tauri IPC (`ipc.rs`, регистрация в `main.rs::generate_handler!`) — панель↔демон в процессе; unix-сокет (`server.rs`, `~/.jarvis/run.sock`) — внешние процессы (`jarvis-hook` POST-события → `reduce`, `GET /state`). **MCP-сервер-бинарь подключается к демону через сокет** — новый эндпоинт `POST /capability`.
- **`Daemon`** (`daemon.rs`) держит все сервисы: `sessions: Mutex<HashMap<String,Session>>`, `settings: settings::Store`, `usage: Arc<Usage>`, `history: Arc<History>`, `limits`, `commands`, `voice`. Доступ из IPC — `Daemon::get(&app) -> Arc<Daemon>`.
- **Сигнатуры фасадов:** `usage.stats(period)->Value`, `usage.for_session(id)->Option<Value>`, `usage.official_info()->Option<..>`; `history.projects(&usage)->Value`; `settings.load()->Value` / `settings.save(patch)->Value` / `settings.string/bool`; `limits.state()->LimitState`; `tmux::reply(pane,prompt)->Result`; `model::Session` (есть `task_board`, `status`, `project`, `cwd`, `tmux_pane`, `transcript`); `transcript::chain_from_entries`/`to_chat_items`/`full_final_reply`.
- **Контроль-действия уже есть в `ipc.rs`:** `session_reply` (tmux reply + ack), `session_set_model`, `session_set_effort`, `question_answer`, `task_action`. Капабилити-фасады **зовут общие хелперы**, не дублируют — при необходимости выносим тело из `ipc.rs` в функцию, которую зовут и IPC, и капабилити.
- **Деп-ы:** serde_json (preserve_order), tokio(full), axum 0.8, reqwest(blocking+json+rustls), chrono, regex. JSON-RPC/MCP крейта нет — пишем newline-delimited JSON-RPC 2.0 руками.
- **Тесты:** инлайн `#[cfg(test)] mod tests` (см. `daemon.rs`).

## Модуль `src-tauri/src/capability/`

- `contract.rs` — `RiskClass {Read,Control,Settings,Admin}`, `Provenance {Trusted,Untrusted}`, `CapabilityMeta {id,class,provenance,description,input_schema:Value}`, `CallOutput {value:Value, provenance:Provenance}`, `GateError {NotFound,Denied,NeedsConfirmation,Rejected,Failed}`. Все — serde-friendly.
- `grant.rs` — `Consumer {id, grant}`; `Grant {classes: HashSet<RiskClass>, confirm: ConfirmPolicy}`; дефолты: `agent_grant()` (read=авто, control/settings=confirm-always, admin=нет), `panel_grant()` (всё, т.к. это сам пользователь). Security-ключи settings.json (denylist для самоэскалации) — константа `SECURITY_KEYS`.
- `audit.rs` — append JSONL в `~/.jarvis/audit.jsonl` (`record(entry)`); `query(filter)->Vec<Value>` для `audit.query`. Best-effort, как `log.rs`.
- `confirm.rs` — `trait Confirmer { async fn confirm(&self, meta, args) -> bool }`. Реализации: `AutoApprove`/`AutoDeny` (тесты), `PanelConfirmer` (эмитит карточку в панель, ждёт ответа через oneshot).
- `gate.rs` — центральный `async fn invoke(d, consumer, id, args, confirmer) -> Result<CallOutput, GateError>`: (1) найти капабилити; (2) проверить грант по классу; (3) control/settings + политика → confirmer; (4) settings.set → проверка SECURITY_KEYS (запрет самоэскалации); (5) исполнить handler; (6) аудит; (7) вернуть с провенансом.
- `registry.rs` — `Registry`: `HashMap<&'static str, Entry>`, `Entry {meta, handler: Box<dyn Fn(Arc<Daemon>, Value) -> BoxFuture<Result<Value,String>> + Send+Sync>}`. `register(meta, handler)`; `list_for(grant) -> Vec<&CapabilityMeta>` (грант-фильтр для tools/list); `tools_json(grant) -> Value` (проекция в MCP tool defs).
- `native/` — фасады, каждый `pub fn register(reg: &mut Registry)`:
  - `sessions.rs` — list/get/reply/queue/control/launch/interrupt.
  - `chats.rs` — read (нативно из transcript); search/summarize — позже через chat-MCP импорт.
  - `metrics.rs`, `notifications.rs`, `tasks.rs`, `settings.rs`, `audit.rs`.
- `mcp_import.rs` — MCP-клиент демона к внешнему chat-MCP (фаза 6).
- `mod.rs` — сборка реестра (`build_registry()`), re-exports.

## Фазы

### Фаза 1 — Ядро капабилити (без сервисов) ✅ тестируемо без LLM
contract + grant + audit + confirm + gate + registry. Пара тестовых капабилити (read-эхо, control-эхо). Юнит-тесты гейта: грант allow/deny по классу; control требует confirm (AutoDeny→Rejected, AutoApprove→ok); settings.set по security-ключу → Denied (самоэскалация); аудит пишется; провенанс в выводе. **Закрывает приёмочные 4, 6 в тестах.** Регистрируем модуль в `main.rs`, держим build зелёным (фича за флагом-неиспользованием ок).

### Фаза 2 — Нативные read-фасады
sessions.list/get, metrics.query, notifications.history, tasks.get, settings.get, audit.query, chats.read. Делегируют в реальные сервисы демона. Тесты: где можно собрать минимальный `Daemon`/сервис — проверяем форму; где нужен живой Tauri AppHandle — выносим чистую логику и тестируем её. **Закрывает 1, 9.**

### Фаза 3 — Control/settings-фасады
sessions.reply/queue/control/launch/interrupt, settings.set. Выносим тело `session_reply` и сеттеров модели/effort из `ipc.rs` в общие функции, зовём из обоих мест (no dup). Confirmation идёт через гейт. **Закрывает 2, 3 (запутанный помощник) в тестах с AutoDeny/AutoApprove.**

### Фаза 4 — MCP-сервер (бинарь) + сокет-эндпоинт
- `server.rs`: новый `POST /capability` `{consumer, id, args}` → `gate::invoke` с `PanelConfirmer` → JSON-ответ `{ok, value, provenance}` / `{ok:false, error}`. Сокет уже 0600 (только владелец).
- `src/bin/jarvis-mcp.rs`: stdio JSON-RPC 2.0 MCP-сервер. `initialize`, `tools/list` (из реестра, грант агента), `tools/call` → `POST /capability` на сокет. Тонкий мост, как `jarvis-hook`. Юнит-тесты протокольного слоя (framing, tools/list shape, tools/call → socket call mock).
- `Cargo.toml`: новый `[[bin]] jarvis-mcp`.
- Установщик (`bin/setup.rs`): класть `jarvis-mcp` в `~/.jarvis/bin/`.

### Фаза 5 — Агент-хост
`agent/mod.rs`: `trait AgentHost`; `ClaudeCliHost` — спавнит `claude` с `--strict-mcp-config --mcp-config <jarvis-mcp>` + системный промпт, без встроенных tools, переиспользуя авторизацию/прокси (`claude_bin`). Стримит ответ. IPC `agent_send(message)` + событие `agent:delta`/`agent:done` в панель. Карточка подтверждения: `PanelConfirmer` эмитит `agent:confirm` + ждёт `agent_confirm_reply` IPC.

### Фаза 6 — Импорт chat-MCP (3b)
`mcp_import.rs`: демон как MCP-клиент к внешнему chat-MCP (транспорт/инструменты — **сверить с реальным процессом; если форма расходится — остановиться и описать, §14.2**). Регистрация его tools как untrusted-капабилити. Реализует chats.search/summarize.

### Фаза 7 — Панель чата
`ui/`: чат-поверхность (ввод, стрим ответа, история тёрна), карточки подтверждения side-effect (текст+цель, Подтвердить/Отклонить). Поверх существующего IPC, без слома каналов.

## Порядок коммитов
По фазе на коммит, сообщение `feat(capability): …` / `feat(agent): …`. Ветка `increment-8-capability-platform`. `master` не трогаем — демон пользователя продолжает работать.

## Статус реализации (2026-06-16, ветка `increment-8-capability-platform`)

**Готово и протестировано (фазы 1–4):**
- ✅ **Фаза 1** — ядро: `contract`, `grant`, `confirm`, `audit`, `registry`, `gate`. 9 юнит-тестов (грант по классу, подтверждение side-effect, запрет самоэскалации, аудит, провенанс). Приёмочные 4, 6.
- ✅ **Фаза 2** — read-фасады: sessions.list/get, metrics.query/session, notifications.history, limits.get, tasks.get, settings.get, audit.query, chats.read. Приёмочный 9.
- ✅ **Фаза 3** — control/settings: sessions.reply, sessions.control, settings.set. Общие ядра вынесены из `ipc.rs` (no dup). Приёмочные 2, 3 (структурно). *Отложено:* sessions.queue/launch/interrupt (новая инфраструктура).
- ✅ **Фаза 4** — досягаемость: `POST /capability` + `GET /capabilities` в `server.rs`; реестр в `Daemon`; бинарь `jarvis-mcp` (stdio JSON-RPC 2.0, 6 тестов + офлайн smoke). Приёмочные 1, 5 (по сокету), 7.
- Всего зелёных тестов: **112** (100 app + 6 setup + 6 mcp). `master` не тронут.

**Осталось (фазы 5–7):**
- ⏳ **Фаза 5 — агент-хост.** Спавн `claude` CLI с `--strict-mcp-config --mcp-config <jarvis-mcp>` + системный промпт; стрим в панель; **PanelConfirmer** (заменяет AutoDeny в `server.rs::handle_capability` — эмитит карточку, ждёт ответа). Не сделано автономно: проверяется только живым запуском CLI (нужна авторизация/прокси пользователя, тратит подписку, может спавнить реальные сессии).
- 🚫 **Фаза 6 — импорт chat-MCP.** ЗАБЛОКИРОВАНА: внешнего chat-MCP нет в этом репозитории (§14.2). По правилу спеки — остановиться и описать, а не выдумывать форму. Нужен путь к процессу/его транспорт.
- ⏳ **Фаза 7 — панель чата.** Фронтенд в `ui/`: чат-поверхность + карточки подтверждения. Зависит от фазы 5.

**Как подключить агента (фаза 5), снимок для продолжения.** MCP-конфиг для `claude` CLI:
```json
{ "mcpServers": { "jarvis": { "command": "~/.jarvis/bin/jarvis-mcp" } } }
```
Запуск (по образцу `claude_bin::run_haiku`): `claude --strict-mcp-config --mcp-config <конфиг> --append-system-prompt <промпт> -p <сообщение>` (+ stream-json для стрима). Установщик (`bin/setup.rs`) должен класть `jarvis-mcp` в `~/.jarvis/bin/`. PanelConfirmer: завести в демоне реестр ожидающих подтверждений (token→oneshot), эмитить `agent:confirm` в панель, IPC `agent_confirm(token, approved)` резолвит; подставить вместо `AutoDeny`.

## Риски / границы
- Фазы 1–3 полностью тестируемы и закрывают ядро безопасности — наивысшая уверенность.
- Фаза 4 (MCP-протокол) тестируется на уровне framing; живая связка с CLI проверяется только запуском (нужна авторизация).
- Фаза 6 заблокирована до нахождения внешнего chat-MCP — при расхождении формы останавливаемся и описываем, не выдумываем.
