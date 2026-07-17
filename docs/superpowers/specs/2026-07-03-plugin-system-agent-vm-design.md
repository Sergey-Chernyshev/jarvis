# Плагинная система Jarvis + интеграция agent-vm (issue #44)

Дата: 2026-07-03 · Ветка: `feat/agent-vm-plugin` · Статус: дизайн на ревью

## 1. Контекст и цель

Issue [#44](https://github.com/Sergey-Chernyshev/jarvis/issues/44): интеграция с
[agent-vm](https://github.com/MikD1/agent-vm) («avm») — CLI, поднимающий изолированные
Linux-VM (по одной на проект) для запуска кодинг-агентов. Требование issue: остаёмся в границах
проекта — **монитор/пульт, не оркестрация агентов**.

Решение шире issue: avm заводится не как хардкодная фича, а как **первый плагин** новой
плагинной системы. Требования к системе:

1. Плагины настраиваются в отдельном разделе «Плагины» в настройках.
2. Плагины получают доступ к функциям Jarvis (данные сессий, уведомления/TTS, голосовые
   команды, трей) — управляемо, с грантами.
3. Плагины могут потреблять данные друг друга: данные плагина мапятся в основную модель
   данных Jarvis и re-экспортируются другим плагинам через API ядра под флагами доступа.

**Главный пользовательский сценарий (MVP)** — запуск терминалов Claude/Codex в VM прямо из
Jarvis: во вкладке «Проекты» (реальный запуск из неё уже есть — PR #43) рядом с «новый
Claude/Codex» появляется плагинный вариант «в VM»; альтернативный вход — деталь плагина в
настройках с выбором проекта. Запущенная в VM сессия отдаёт события в Jarvis (хуки из гостя)
и появляется в общем списке сессий в карточке своего проекта.

## 2. Пресерч: как это устроено у других

Изучены шесть систем (Raycast, Obsidian, VS Code, Tauri v2, Stream Deck, Home Assistant).
Выжимка уроков, применимых к Jarvis:

| Система | Модель | Что берём | Что отвергаем |
|---|---|---|---|
| **Raycast** | Node-процессы + React-реконсайлер в нативный UI; манифест `package.json` | `preferences[]`-схема → **автогенерация UI настроек** + типизированное чтение; `tools[]` — типизированные функции, которые вызывает AI, с `Tool.Confirmation` для side-effect'ов (прямой аналог голосовых скилов с confirm-тостом) | Свой React-рендерер — оверкилл для ванильного JS UI |
| **Obsidian** | Full-trust JS в процессе приложения | Простоту DX как ориентир | Модель доверия: полный доступ без изоляции; меж-плагинный `app.plugins.plugins[id].api` — неверсионированный reach-in |
| **VS Code** | Отдельный extension-host-процесс; манифест `contributes` | `contributes.configuration` → авто-UI настроек; ленивая активация; `extensionDependencies` + `exports` — типизированный меж-плагинный хендшейк (опциональный escape-hatch на будущее) | Отдельный JS-host — лишний рантайм для наших задач |
| **Tauri v2 plugins** | Rust-плагины, **компилируются в бинарь** | Дизайн permissions+capabilities ACL (у Jarvis уже есть свой аналог); `Channel`/`emit` для стриминга | Как механизм устанавливаемых плагинов непригоден: не runtime-loadable |
| **Stream Deck** | Плагин = **отдельный процесс на любом языке**, WebSocket + регистрационный handshake (`-port -pluginUUID -registerEvent -info`) | Модель out-of-process сайдкара с handshake — шаблон для тяжёлых/нативных плагинов (avm) | Настройки как HTML-страницы плагина (у нас — схема в манифесте) |
| **Home Assistant** | Интеграции нормализуют данные в **центральную entity-модель** (`hass.states`), любая интеграция/автоматизация читает по `entity_id`, не зная источника; `dependencies` vs `after_dependencies`; config_flow-схема → авто-UI | **Entity-store ядра — хребет меж-плагинных данных** (ровно требование №3); событийная шина; сервисы как RPC | Python-in-process full trust |

Ключевой вывод пресерча: три требования тянут к трём разным эталонам — автогенерация настроек
(Raycast/VS Code), меж-плагинные данные (Home Assistant), тяжёлый нативный первый плагин
(Stream Deck). Правильная архитектура — **комбинация**, а не выбор одного.

## 3. Что уже есть в Jarvis (швы)

Ресерч кода показал: data plane уже «плагинный», не хватает загрузчика, view plane и реестров.

- **Capability-платформа — готовый фундамент** (`src-tauri/src/capability/`):
  `Registry<C>::register(meta, handler)`, гейт с confirm/audit, классы риска
  `Read/Control/Settings/Admin`, и **уже существующий `Consumer::plugin(id, classes)`**
  (`grant.rs:100`): least-privilege из манифеста, `admin` отфильтрован, confirm-always,
  анти-самоэскалация (`SECURITY_KEYS`, `SETTINGS_ALLOWLIST`). Спека
  `2026-06-18-increment-8-v2-capability-platform-rework-design.md` §10 прямо описывает путь
  плагина (манифест → токен в `~/.jarvis/tokens.json` → грант), помечено «заложено, не строим».
- **Power-хост** (`power/mod.rs`) — бывший Electron-plugin-host, схлопнутый в 2 хардкодных
  плагина; RPC `plugins_status`/`plugins_cmd`, generic `_enable`, `tray_items()` уже generic,
  роутинг — `match id`. Электроновский контракт (`init/dispose/onSessions/trayMenu/badge/status`,
  спека `2026-06-11-power-plugins-design.md`) — готовый чертёж жизненного цикла.
- **Настройки** (`settings.rs`): динамический JSON, блоки `plugins.<id>`, готовые аксессоры
  `plugin(id, defaults)` / `set_plugin(id, patch)`.
- **UI настроек** (`ui/settings2.js`): хардкодные `NAV` + `RENDERERS` — динамического пути нет,
  нужна одна generic-панель «Плагины».
- **Голосовые скилы**: хардкод-триплет (`convo/skills.rs: skills_menu()` + `dispatch()` match +
  спец-кейсы в `converse_turn`); ABI уже чистый: `{skill, args}` + `SkillOutcome`; ветка
  `other => Rejected` (`skills.rs:348`) — точка делегирования в реестр.
- **Сокет-граница**: UDS `~/.jarvis/run.sock` (0600) + токены `x-jarvis-token`; хуки агентов и
  MCP-мост уже ходят по нему. Сессии создаются **только из хук-событий**; реконсиляция живости
  по pid/tmux хоста (`reconcile_sessions`).
- **Уведомления**: буферизованный тост-путь + `notify_id_voiced` (TTS), координация с диктовкой
  (`coord::Interaction`).

## 4. Факты про agent-vm (v0.1, зафиксировано на 2026-07-03)

- Один Go-бинарник поверх **Lima** (`vmType: vz` — Apple Virtualization, virtiofs), только macOS.
  Модули агентов: `claude`, `codex`. Два режима workspace: mount (проект хоста в госте) и clone.
- **Нет демона, API, JSON-вывода, событий, tmux, проброса портов.** `avm list` — plain-text
  таблица (парсить хрупко).
- Наблюдение снаружи: **YAML-реестр `~/.config/agent-vm/vms/<name>.yaml`** (авторитетный
  инвентарь: modules, workspace, resources; атомарная запись) + **`limactl list --json`**
  (стабильный runtime-статус). Управление: shell-out в `avm start/stop/restart/recreate/delete`.
- Телеметрия агентов: `avm` копирует `~/.config/agent-vm/modules/claude/settings.json` в гостя
  при создании/пересоздании VM → **туда можно внедрить хуки Claude Code**. Гость достигает хоста
  по `host.lima.internal:<port>` (user-mode сеть Lima; проверить на конкретной инсталляции).
  Хостовый маунт `/mnt/host/agent-vm` — read-only, канал «гость → хост» только сетевой.
- Ограничения: примитива «послать ввод работающему агенту» нет (нужен свой tmux в госте через
  `limactl shell`); модуль codex хуков не получает; **до 1.0 все интерфейсы нестабильны**
  (особенно схема Record-YAML — в AGENTS.md названа внутренним состоянием).

## 5. Варианты архитектуры

### Вариант A — встроенный модуль (третий power-плагин), без плагинной системы

Rust-модуль `agent_vm` в ядре рядом с `keep-awake`/`clamshell`: FS-watch реестра, poll limactl,
shell-out в avm, хардкодная панель в настройках.

- Плюсы: самый быстрый путь к issue #44 (1–2 инкремента); ноль нового ABI.
- Минусы: не решает ни одно из трёх требований к системе; каждая следующая интеграция — снова
  код в ядре; меж-плагинного API не появляется; хардкод в `NAV`/`RENDERERS` растёт.
- Вердикт: **отклонён** как основной путь (годится только как аварийный фолбэк, если issue
  нужен срочно).

### Вариант B — sidecar-плагины на capability-платформе + entity-store ядра (рекомендуется)

Плагин = **отдельный процесс** (любой язык), запускаемый Jarvis'ом с handshake
(Stream Deck-модель), говорящий с ядром по **существующему UDS + токену** (шов из спеки
capability v2 §10). Декларативный **манифест** (Raycast/HA-модель) описывает настройки
(схема → автогенерация UI), голосовые скилы, публикуемые сущности (`provides`) и потребляемые
данные других плагинов (`consumes`). Ядро владеет **entity-store** (HA-модель): плагины
публикуют нормализованные сущности, ядро re-экспортирует их всем потребителям через
capability-API под грантами — меж-плагинный обмен решён by construction, плагины друг с другом
напрямую не говорят.

- Плюсы: закрывает все три требования; переиспользует готовые швы (capability, tokens,
  `plugins.<id>`, power-RPC, сайдкар-супервизор); изоляция процессов (крэш/kill/idle-stop);
  agent-vm ложится идеально (shell-out, FS-watch, TCP-мост для хуков из VM); язык плагина
  свободный.
- Минусы: больше всего работы (протокол, host, generic-панель, entity-store); нужен маленький
  SDK, чтобы DX не страдал.

### Вариант C — in-webview JS-плагины (Raycast/Obsidian-стиль)

JS-модули грузятся в webview панели, регистрируются в `window.jarvis`-API.

- Плюсы: лучший DX (ванильный JS, как весь UI), UI-вклады тривиальны.
- Минусы: **для agent-vm непригоден** — нужны shell-out/FS-watch/TCP-сервер, т.е. пришлось бы
  выдать webview-коду широкие invoke-права, что ломает модель безопасности (webview =
  `Consumer::panel`, полное доверие); изоляции нет; тяжёлые задачи не влезают.
- Вердикт: **не сейчас**. Совместим с B как будущий второй слой для чисто UI-плагинов
  (виджеты, панели) поверх того же entity-store.

**Рекомендация: Вариант B.** Дальше — его детальный дизайн.

## 6. Дизайн (вариант B)

### 6.1 Компоненты

```
┌────────────────────────── Jarvis (Tauri) ──────────────────────────┐
│  PluginHost (Rust)        EntityStore (Rust)      SkillRegistry    │
│  discovery/spawn/         entity_set/query/       (замена хардкод- │
│  handshake/supervise      subscribe + events      триплета скилов) │
│        │                        ▲    │                   ▲         │
│        │ UDS ~/.jarvis/run.sock │    │ emit «entities»   │         │
└────────┼────────────────────────┼────┼───────────────────┼─────────┘
         ▼                        │    ▼                   │
   plugin: agent-vm  ─────────────┘  UI: раздел «Плагины» (generic)
   (процесс-сайдкар)                 трей-секции, тосты, TTS
   FS-watch vms/*.yaml · limactl --json · shell-out avm
   TCP :<port> ← хуки Claude из гостевых VM (host.lima.internal)
```

### 6.2 Манифест плагина

`~/.jarvis/plugins/<id>/manifest.json` (first-party плагины — в репо, `plugins/<id>/`,
симлинк/копирование при установке). Чистый JSON, без кода:

```jsonc
{
  "id": "agent-vm",
  "name": "Agent VM",
  "version": "0.1.0",
  "description": "Монитор и пульт для VM с агентами (avm)",
  "entry": { "type": "binary", "path": "bin/jarvis-plugin-agent-vm" },
  // классы риска — маппятся в Consumer::plugin(id, classes), admin недоступен
  "capabilities": ["read", "control"],
  // конкретные capability ядра, которые плагин зовёт (least-privilege внутри классов)
  "uses": ["sessions.ingest", "notify.toast", "notify.voiced", "entities.publish"],
  // сущности, которые плагин публикует в entity-store
  "provides": [
    { "kind": "vm", "attrs": ["state", "modules", "workspace", "resources"] }
  ],
  // данные других плагинов (пусто у первого плагина; включается грантом в UI)
  "consumes": [],
  // вклад в запуск сессий: плагин становится «средой запуска» рядом с локальной.
  // UI рендерит вариант «в VM» у кнопок запуска вкладки «Проекты» и в детали плагина
  "launchTargets": [
    { "id": "vm", "title": "в VM (agent-vm)", "agents": ["claude", "codex"] }
  ],
  // голосовые скилы: description — строка для меню LLM-планировщика
  "skills": [
    { "name": "vm_status",  "risk": "read",
      "description": "статус виртуалок: сколько запущено, что с конкретной VM" },
    { "name": "vm_power",   "risk": "control",
      "description": "запустить/остановить/перезапустить виртуалку по имени",
      "args": { "name": "string", "op": "start|stop|restart" } }
  ],
  // схема настроек → автогенерация UI (типы = уже существующие компоненты settings2.js)
  "settings": [
    { "key": "enabled",       "type": "toggle",    "title": "Включён", "default": false },
    { "key": "announce",      "type": "toggle",    "title": "Озвучивать смену состояний VM", "default": true },
    { "key": "pollInterval",  "type": "segmented", "title": "Опрос limactl",
      "options": ["5s", "15s", "60s"], "default": "15s" },
    { "key": "hooksInject",   "type": "toggle",    "title": "Внедрять хуки Claude в VM", "default": true }
  ],
  "tray": true
}
```

Значения настроек живут в существующем `settings.plugins.<id>` (аксессоры уже есть).
Ключи `grants`/`plugins`/`gatePolicy`/`capability` защищены `SECURITY_KEYS` — плагин не может
расширить собственные права через settings-патч.

### 6.3 Жизненный цикл (PluginHost)

Обобщение `power::Power` + супервизор сайдкаров (тот же 5-секундный цикл):

1. **Discovery**: скан `~/.jarvis/plugins/*/manifest.json` (+ dev-путь `plugins/` в репо за
   настройкой `pluginsDevDir`). Валидация манифеста; невалидный → статус `error`, не грузим.
2. **Enable** (тумблер в UI → generic `_enable`, уже есть в `plugins_cmd`): выпуск токена в
   `~/.jarvis/tokens.json` с `Consumer::plugin(id, classes-из-манифеста)`.
3. **Spawn + handshake** (Stream Deck-модель, но по UDS): процесс запускается с
   `JARVIS_SOCKET=<path> JARVIS_PLUGIN_ID=<id> JARVIS_TOKEN=<token>`; первым запросом плагин
   зовёт `POST /plugin/register` (версия протокола, pid) — до этого ядро не считает его живым.
4. **Run**: плагин зовёт capability по UDS (как агент сегодня: `/capabilities`, `/capability`);
   подписки (сессии, события) — long-poll `GET /plugin/events` (SSE-поверх-UDS не тянем в v1).
5. **Supervise**: крэш → рестарт с backoff; `disable` → SIGTERM, снятие токена; статусы
   `running/stopped/error/incompatible` — в `plugins_status` (тот же RPC, что сейчас).

Ядро остаётся владельцем трея (`tray_items()` из статуса плагина) и тостов — плагин лишь зовёт
`notify.*`-capability, попадая в существующий буферизованный путь и координацию с диктовкой.

### 6.4 Data plane: EntityStore + события + сессии

**EntityStore** (новый модуль `src-tauri/src/entities.rs`, HA-модель, урезанная до нужного):

- Сущность: `{ id: "vm.my-api", kind: "vm", owner: "plugin:agent-vm", state: "running",
  attrs: {…}, updated_at }`. Ключ — `kind.<object_id>`, владелец — только пишущий.
- Новые capability: `entities.publish` (upsert/remove своих сущностей; Read-класс,
  плагин пишет только под своим owner), `entities.query(kind?, owner?)`,
  `entities.subscribe(kind)` (через `/plugin/events`). Панель (`Consumer::panel`) видит всё;
  плагин-читатель — только kind'ы из своего `consumes` с включённым флагом-грантом.
- UI получает `emit("entities", …)` — как сейчас `state`/`plugins`.

**Меж-плагинный доступ (требование №3)**: второй плагин объявляет
`"consumes": [{ "plugin": "agent-vm", "kind": "vm" }]` → в разделе «Плагины» у него появляется
переключатель-грант «Читает данные Agent VM: виртуалки» (по умолчанию выключен — Raycast-модель
«cross-launch = согласие юзера»). Данные он получает через ядро (`entities.query/subscribe`),
**не** через прямой канал к плагину-источнику. Никакой зависимости от рантайма источника: если
agent-vm выключен, сущности его owner'а помечаются `stale`, потребитель это видит по атрибуту.

Классификация по риску: `entities.publish` и `sessions.ingest` — **Read-класс** (вход данных
в ядро без side-effect'ов на системе пользователя, всё помечается провенансом) — подтверждения
не требуют, иначе confirm-гейт на каждом хук-событии сделал бы телеметрию бесполезной.
Control-класс остаётся за действиями плагина наружу (shell-out, запись в чужие конфиги).

**Сессии агентов из VM — первосортные сессии Jarvis** (это суть «монитора»): плагин пересылает
хук-события в существующий пайплайн через новую capability `sessions.ingest` (провенанс
`plugin:<id>`, untrusted). Payload нормализуется: `host = "vm:<vm-name>"`, гостевые
`pid/tmux_pane` не имеют смысла на хосте → **liveness-провайдер**: `reconcile_sessions`
делегирует проверку живости сессий с `host=vm:*` плагину-владельцу (VM запущена + процесс агента
жив в госте), вместо host-проверок pid/tmux. Карточка сессии получает бейдж VM.

### 6.5 Раздел «Плагины» в настройках

Одна generic-панель (новый пункт `NAV` + один рендерер — дальше всё data-driven):

- Список: иконка, имя, версия, статус (`running/stopped/error`), тумблер enable.
- Деталь плагина: настройки, отрендеренные из `settings`-схемы манифеста существующими
  компонентами (`drow/toggle/segmented`); блок «Права»: классы риска + `uses` + `consumes`-гранты
  (переключатели, выключены по умолчанию); блок «Здоровье»: pid, аптайм, последняя ошибка,
  кнопка «Перезапустить».
- Никакого HTML от плагина в v1 (безопасность + простота). Если схемы не хватит — расширяем
  типы схемы, а не открываем произвольный UI.

### 6.6 Голосовые скилы плагинов

- Новый `SkillRegistry` на `Daemon`: встроенные скилы регистрируются так же, как плагинные
  (рефакторинг хардкод-триплета — `skills_menu()` собирается из реестра, `dispatch()` ищет в
  реестре, ветка `other =>` уходит).
- Скил плагина: `description` из манифеста попадает в меню LLM-планировщика; диспетч зовёт
  плагин по UDS (`POST /plugin/skill {name, args}` → `SkillOutcome`), таймаут 10с →
  `Rejected` + тост.
- Риск-класс скила прогоняется через существующий confirm-гейт: `read` — авто, `control` —
  подтверждение (Raycast `Tool.Confirmation`, но у нас это уже есть в capability-гейте).

### 6.7 Плагин agent-vm (первый потребитель)

Отдельный бинарь `jarvis-plugin-agent-vm` (Rust, в этом же репо — воркспейс-крейт; SDK-крейт
`jarvis-plugin-sdk` с клиентом UDS-протокола выделяется из него):

- **Инвентарь**: FS-watch `~/.config/agent-vm/vms/*.yaml` (+ `XDG_CONFIG_HOME`) → сущности
  `vm.<name>` (attrs: modules, workspace mode/path, resources, source). Runtime-состояние —
  poll `limactl list --json` (интервал из настроек). `avm list` не парсим (plain text, хрупко).
- **Управление**: скил `vm_power` + команды из UI → shell-out `avm start/stop/restart <name>`
  (control, confirm-гейт). `recreate`/`delete` — только из UI детали плагина с отдельным
  подтверждением (деструктивно: clone-VM теряет незапушенное).
- **Запуск сессии в VM — главный сценарий MVP.** Точки входа: (а) вкладка «Проекты» — у
  группы проекта рядом с «новый Claude/Codex» появляется вариант «в VM» (рендерится из
  `launchTargets` манифеста); (б) деталь плагина в настройках — «Запустить сессию» с пикером
  проекта (тот же список групп истории) и агента. Флоу по шагам:
  1. UI зовёт `session_launch` с `target: "agent-vm:vm"` (новый опциональный параметр; без
     него — прежний локальный путь). Ядро маршрутизирует плагину:
     `POST /plugin/launch {cwd, agent, sessionId?}`. Инициатор — панель (юзер кликнул),
     поэтому confirm-гейта здесь нет.
  2. Плагин ищет VM проекта по `workspace.hostPath == cwd` в Record-YAML. Нет VM → ядро
     показывает подтверждение «Создать VM для проекта? (~минуты)» → `avm init` (модули из
     настроек плагина, по умолчанию `claude,codex`) + `avm create`; прогресс — сущность
     `vm.<name>` в состоянии `provisioning` + тост по готовности. VM остановлена → `avm start`.
  3. Плагин возвращает ядру **launch-spec** — внутреннюю команду для терминала:
     `limactl shell --workdir <guestPath> <vm> -- tmux -L jarvis new -A -s <name>
     '<claude|codex …>'`. Ядро исполняет её существующим `launch::spawn` — терминал юзера
     (Terminal/iTerm2/кастом) открывается как при локальном запуске. Прокси-команда хоста в
     VM не пробрасывается (среда гостя — забота модулей avm).
     Симметрия с локальным путём: там агентов оборачивает в `tmux -L jarvis` agent-shim,
     здесь — тот же tmux, но внутри гостя. Это же даёт канал управления (send-keys).
  4. Хуки в госте отдают session-start → TCP-мост → `sessions.ingest` → сессия появляется в
     общем списке. **Нормализация cwd**: события из гостя приходят с `guestPath`; плагин
     переписывает его в `hostPath` из Record — иначе VM-сессия не сгруппируется в карточку
     своего проекта во вкладке «Проекты».
  5. Resume для VM-сессий: строка сессии в «Проектах» → тот же launch-spec с
     `claude --resume <id>` / `codex resume <id>` внутри guest-tmux (`new -A` подхватит
     живую tmux-сессию, если она ещё существует).
- **Телеметрия агентов (хуки из VM)**: при включённом `hooksInject` плагин идемпотентно
  дописывает хуки в `~/.config/agent-vm/modules/claude/settings.json` (тем же паттерном, что
  `reconcile_hooks()` — merge, не перезапись). Хук в госте: `curl http://host.lima.internal:
  <port>/event -H 'x-vm-token: …'` (шелл-однострочник, без бинарей в госте). Плагин держит TCP-
  листенер на `127.0.0.1:<port>` (гость достигает его через шлюз Lima), валидирует per-VM токен,
  нормализует payload (`host=vm:<name>`) и пересылает в ядро через `sessions.ingest`.
  Оговорки честно фиксируются в UI: хуки попадают только в VM, созданные/пересозданные после
  включения; codex-агенты в VM телеметрию не дают (у avm-модуля codex нет хуков, `codex exec`
  их не эмитит — известный факт).
- **Уведомления**: смена состояния VM / `waiting` от агента в VM → существующий
  `notify_id_voiced` (гейт настройкой `announce`).
- **Пульт (reply/continue) для VM-сессий**: раз агент запущен нами в guest-tmux, плагин
  реализует транспорт ответа — `limactl shell <vm> -- tmux -L jarvis send-keys …` по семантике
  `tmux.rs::reply` (C-u → set-buffer → paste → Enter). `session_reply` для сессий с
  `host=vm:*` делегируется плагину-владельцу (per-session transport provider). Работает только
  для сессий, запущенных через Jarvis (у запущенных вручную через `avm shell` нет нашего
  tmux) — честно показываем это в UI отсутствием поля ответа.

### 6.8 Безопасность

- Идентичность: токен per-plugin в `tokens.json`, UDS 0600. Грант — `Consumer::plugin`
  (least-privilege, admin отфильтрован, confirm-always для control/settings, анти-эскалация
  `SECURITY_KEYS`/`SETTINGS_ALLOWLIST`) — **уже реализовано и покрыто тестами**.
- Провенанс `plugin:<id>` в аудите гейта; `sessions.ingest` помечает сессии источником.
- TCP-листенер agent-vm — поверхность атаки локальной сети VM: слушаем только на loopback
  (Lima-шлюз транслирует), per-VM bearer-токен, полезная нагрузка проходит ту же валидацию,
  что хук-события с хоста.
- Установка плагинов в v1 — только ручная (каталог) и first-party из репо; подпись/магазин —
  за скоупом (совпадает с «заложено, не строим» фазы 6 capability-спеки).

### 6.9 Инкременты реализации

Каждый — отдельный PR с тестами, UI живьём проверяется на инкрементах 3–4:

1. **EntityStore + capability `entities.*`** (ядро, без UI). Юнит-тесты store и грант-фильтрации.
2. **PluginHost**: discovery/манифест/enable/spawn/handshake/supervise; обобщение
   `plugins_status`/`plugins_cmd`; токен-выпуск; RPC `/plugin/launch` + параметр `target` в
   `session_launch`. Фейк-плагин в тестах.
3. **Раздел «Плагины»** в settings2.js: список + деталь + схема-рендер + гранты.
4. **agent-vm v1 (монитор + запуск)**: инвентарь + runtime-статус → сущности `vm.*`, тосты/TTS,
   трей; **запуск сессии в VM** из «Проектов» (`launchTargets`) и из детали плагина — включая
   создание VM с подтверждением и guest-tmux. Закрывает MVP-сценарий и минимум issue #44.
5. **agent-vm v2 (телеметрия)**: hook-инжект, TCP-мост, `sessions.ingest`, нормализация
   `guestPath→hostPath`, liveness-провайдер, бейдж VM в списке сессий.
6. **Пульт**: `session_reply` через transport provider (guest-tmux send-keys); resume
   VM-сессий из «Проектов». **SkillRegistry** + скилы (`vm_status`, `vm_power` с confirm).
7. **Меж-плагинные `consumes`-гранты** (UI + фильтрация query/subscribe) — активируется, когда
   появится второй плагин-потребитель.

Инкременты 1–4 дают MVP (запуск + мониторинг VM без телеметрии агентов); 5 делает VM-сессии
видимыми в списке; 6–7 включаемы независимо.

Заметки к инкременту 2 (из ревью инкремента 1, 2026-07-17):

- **Квоты EntityStore** до появления первого живого плагина с publish: лимит сущностей
  per-owner (~1000) и размера `attrs` (десятки КБ) — отказ, не молчаливое усечение; проверить
  лимит тела UDS-запроса в `ipc.rs`.
- **Судьба глобальной инъекции `_consumer`**: сейчас гейт инжектит её в args каждого вызова;
  рассмотреть opt-in флаг в `CapabilityMeta` (consumer-aware капабилити) и спрятать `_consumer`
  из confirm-карточки default-ветки `resolve_target` (`confirm_panel.rs`).
- **Протокол `/plugin/*`**: при реализации PluginHost зафиксировать версию протокола в
  handshake, формат ошибок и семантику переподключения long-poll (`/plugin/events`).

## 7. Тестирование

- Ядро: юнит-тесты EntityStore (upsert/владение/stale), PluginHost (handshake, рестарт,
  снятие токена при disable), грант-фильтрация `consumes`.
- Плагин: парсинг Record-YAML (фикстуры реальных файлов avm v0.1), парсинг `limactl list
  --json`, идемпотентность hook-инжекта (merge существующего settings.json).
- Интеграционно (ручной чек-лист, т.к. нужны Lima+avm): создание VM → сущность появилась;
  stop/start голосом с confirm; хук из гостя доходит до списка сессий.
- Контракт-риск avm до 1.0: парсеры за отдельным модулем `avm_compat.rs` с версией-детектом;
  ломающее изменение → статус плагина `incompatible`, а не тихие ошибки.

## 8. Решения по открытым вопросам (утверждены 2026-07-04)

1. **Размещение плагина**: в этом репо, отдельная папка `plugins/agent-vm/` (манифест +
   Rust-крейт; SDK-клиент UDS-протокола выделяется в `plugins/sdk/` по мере надобности).
   Отдельный репозиторий — потом, когда протокол стабилизируется.
2. **Создание VM**: поддерживаем оба пути — запуск в уже созданные руками VM **и**
   авто-создание при первом запуске (диалог подтверждения + прогресс `provisioning` + тост).
3. **Codex в VM без телеметрии в MVP** — принято (терминал запускается, но сессия не видна в
   списке; лечится позже модулем `modules.d/jarvis.sh` для новых VM).
4. UI-плагины в webview (вариант C, `panels` в манифесте) — отложено до реального запроса;
   в v1 — нет.
