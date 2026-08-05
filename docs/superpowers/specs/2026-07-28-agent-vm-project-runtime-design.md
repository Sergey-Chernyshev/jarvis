# Agent VM как проектный runtime внутри Jarvis

Дата: 2026-07-28

Основа: [`2026-07-03-plugin-system-agent-vm-design.md`](2026-07-03-plugin-system-agent-vm-design.md)

Статус: v4, runtime и Project Manager скорректированы после live-проверки
2026-07-28

## 1. Решение

Jarvis получает первый полноценный **project runtime**: пользователь открывает
проект, выбирает Claude или Codex и пишет задачу. Дальше Jarvis сам:

1. находит либо создаёт project VM через существующий `agent-vm`;
2. запускает VM при необходимости;
3. переносит разрешённые настройки Claude/Codex и адресно доставляет авторизацию;
4. создаёт или переиспользует постоянную интерактивную terminal-сессию агента;
5. транслирует реальный экран этой сессии и ввод прямо в Project Manager;
6. уведомляет о готовности VM, вопросах, завершении и ошибках;
7. даёт готовую команду входа в VM, если пользователю всё-таки нужен терминал.

Основной интерфейс — Project Manager со встроенным terminal viewport. Это не
реконструированный чат: источником истины остаётся реальная интерактивная
PTY/tmux-сессия Claude/Codex. Внешний Terminal остаётся диагностическим escape
hatch, но для обычной работы не нужен.

### 1.1 Зафиксированные продуктовые решения

| Область                | Решение v1                                                                                 |
| ---------------------- | ------------------------------------------------------------------------------------------ |
| Workspace              | Весь каталог проекта монтируется в VM через штатный mount mode `agent-vm`                  |
| Доступ к проекту       | Только `read-write`, как уже умеет `agent-vm`; `read-only` не показываем и не эмулируем    |
| Изменения в `agent-vm` | Не требуются                                                                               |
| Запуск                 | Ленивый: VM поднимается при первом сообщении или явном «Запустить»                         |
| Запуск вместе с Jarvis | Только для проектов, которые пользователь закрепил для автозапуска                         |
| Агент                  | Один постоянный интерактивный Claude/Codex process на project + backend                    |
| Терминал               | Реальный terminal viewport в Jarvis с reconnect; также даём `avm shell <vm>`               |
| Конфиги                | Claude и Codex зеркалируются автоматически по явному allowlist                             |
| Секреты                | Не синхронизируются как обычные файлы; доставляются адресно, без аргументов команд и логов |
| Главная страница       | Активные VM видны отдельной компактной полосой и на карточках сессий                       |
| Уведомления            | Готовность/ошибка VM, ожидание ответа, завершение агента, аварийная остановка              |
| UI                     | Полноценный project workspace, а не набор технических кнопок плагина                       |
| Каталог проектов       | Любую существующую папку можно добавить системным folder picker                            |
| Поиск                  | По имени проекта и каноническому пути, без transcript/chat metadata                        |
| Избранное              | Звезда закрепляет проект; порядок меняется вручную вверх/вниз                              |
| Представление          | Переключаемые компактный список и почти квадратные карточки                                |
| Runtime status         | Большая плашка видна только во время запуска/retry/ошибки                                  |

## 2. Почему старая спека меняется

Старая версия правильно выбрала out-of-process PluginHost, EntityStore и
first-party plugin `agent-vm`, но оставляла Jarvis в роли монитора: запуск агента
происходил в новом окне Terminal, а чат попадал в Jarvis позднее через hooks и
transcript.

Новый продуктовый контракт сильнее:

- Terminal не нужен для нормальной работы;
- запуск, остановка и продолжение агента принадлежат UI Jarvis;
- реальный терминальный экран и результаты должны приходить в реальном времени;
- project VM становится состоянием проекта, видимым на главной;
- конфигурация агента и авторизация готовятся автоматически.

Архитектурная база старой спеки сохраняется:

- `PluginHost` изолирует интеграцию отдельным процессом;
- `EntityStore` публикует нормализованные `vm.*` и `agent_run.*`;
- capability-гейт остаётся границей прав;
- плагин не получает произвольный HTML и не исполняется внутри webview.

## 3. Текущее состояние и реальные швы

### 3.1 Что уже есть в Jarvis

- `src-tauri/src/entities.rs` и `entities.publish/query` уже реализованы.
- Вкладка «Проекты» группирует историю по `cwd` и умеет запускать
  Claude/Codex, но сейчас вызывает `session_launch`, который открывает внешний
  Terminal через AppleScript.
- Главный чат умеет читать историю, показывать живой хвост, группировать ходы,
  отображать tool/file-факты, diff и ИИ-разбор результата.
- `src-tauri/src/agent/mod.rs` уже разбирает Claude `stream-json`.
- `src-tauri/src/backend/codex_agent.rs` уже разбирает `codex exec --json`.
- Существующий notification/TTS path умеет дедупликацию, коалесинг и переход
  по клику к сессии.
- Настройки уже имеют `plugins.<id>`, а capability-платформа —
  `Consumer::plugin`.

Это означает, что новая работа — не второй чат с нуля. Нужны persistent
terminal runtime, безопасный двусторонний transport и project-oriented view
поверх реальной сессии.

### 3.2 Что фактически умеет `agent-vm` v0.1

Проверено по актуальному `main` и документации
[`MikD1/agent-vm`](https://github.com/MikD1/agent-vm):

- один Go CLI `avm` поверх Lima, macOS-only;
- mount mode монтирует весь host project через virtiofs с `writable: true`;
- `.agent-vm.yaml` обязателен для mount mode;
- `~/.config/agent-vm` монтируется во все VM read-only как
  `/mnt/host/agent-vm`;
- Claude module умеет взять `modules/claude/settings.json` и список plugins;
- Codex module умеет взять `modules/codex/auth.json` и записывает его с `0600`;
- `.gitconfig` переносится без credential-секций;
- clone mode использует SSH agent forwarding, но в v1 Jarvis его не создаёт;
- демона, API и event-stream у `avm` нет.

Следствие: lifecycle выполняем через `avm`, а запуск агента и поток событий —
через отдельный runtime adapter поверх `limactl shell`. Исходники `agent-vm`
для этого менять не требуется.

## 4. Цели, критерии успеха и границы

### 4.1 Цели

1. Из Project Manager можно создать, запустить и использовать постоянного
   VM-агента, не открывая внешнее окно Terminal.
2. После первого provisioning повторный запуск проекта не требует ручной
   настройки VM или повторного логина.
3. Изменения terminal screen появляются в UI не позднее секунды, а ввод
   отправляется в тот же живой process без relaunch.
4. Состояние VM и агента видно на главной и в Project Manager.
5. Значимые переходы состояния дают кликабельное уведомление без дублирующего
   шума.
6. Закрытие project view не останавливает агента. После повторного открытия
   Jarvis подключается к той же tmux-сессии; если процесс действительно умер,
   UI честно показывает `exited` и предлагает новый запуск.
7. Пользователь может добавить произвольную папку, найти её по имени/пути,
   закрепить в избранном, изменить порядок и выбрать list/cards view.

### 4.2 Не входит в v1

- read-only mount и выбор access mode;
- clone-mode как путь автоматического создания проекта;
- синхронизация всего `$HOME`;
- копирование `.env`, private SSH keys, cloud credentials и macOS Keychain;
- обратная синхронизация гостевых user-конфигов на host;
- несколько одновременно работающих агентов в одной project VM;
- marketplace сторонних runtime-плагинов;
- хранение полного terminal output в EntityStore, project mount или run journal;
- несколько одновременных terminal-сессий одного backend в одном проекте.

## 5. Рассмотренные runtime-подходы

### A. Постоянный TTY/tmux и трансляция экрана — выбран

Jarvis создаёт detached tmux-сессию на отдельном host tmux-server. В её PTY
работает `limactl shell` и обычный интерактивный Claude/Codex внутри VM.
Project Manager читает `capture-pane` и отправляет ввод через tmux buffer.

- Плюсы: один реальный process, нативные вопросы/подтверждения/slash-команды,
  reconnect без relaunch, никакой реконструкции чата.
- Ограничение v1: viewport показывает отрендеренный экран tmux без обещания
  структурированных tool/file-событий. Файлы и diff остаются отдельными
  project actions, а не извлекаются из screen scraping.

Выбран как основной transport. Screen scraping не используется для построения
семантического чата: экран показывается пользователю как экран.

### B. Только hooks и tail transcript

Повторяет локальную интеграцию Jarvis: guest hooks присылают lifecycle, host
читает transcript раз в секунду.

- Плюс: минимум новой parser-логики.
- Минусы: нет надёжного канала ввода, холодный transcript появляется не сразу,
  путь гостевого файла недоступен host напрямую, Codex/Claude имеют разные
  ограничения hooks.

Отклонён как единственный transport. Transcript остаётся recovery-источником.

### C. Структурированный headless runtime — отклонён для project UI

Плагин выполняет agent CLI в VM через `limactl shell`, читает JSONL и переводит
его в единый `RunEvent`. Каждый ход — отдельный headless invocation с resume:

- Claude: `claude -p --output-format stream-json`, затем `--resume <id>`;
- Codex: `codex exec --json`, затем `codex exec resume <id> --json`.

Плюсы:

- чистый live-stream без screen scraping;
- текущие parser-компоненты Jarvis переиспользуются;
- единый UI для Claude и Codex;
- процесс можно отменять, ошибки и exit status наблюдаемы;
- transcript/session ID сохраняют историю между ходами.

Минусы, подтверждённые live-проверкой:

- новый CLI process и повторный bootstrap на каждый prompt выглядят как
  перезапуск агента, даже если backend session продолжается;
- интерактивные TUI-состояния приходится искусственно переводить в чат;
- жизненный цикл process привязан к одному headless turn.

Поэтому headless transport остаётся пригодным для фоновых одноразовых задач,
но не является transport Project Manager.

## 6. Архитектура

```text
┌──────────────────────────── Jarvis core ──────────────────────────────┐
│ ProjectRuntimeRegistry  TerminalBridge       NotificationRouter       │
│ SecretStore(Keychain)       ▲ transient screen        ▲               │
│         │ RPC/events         │ + input                 │ lifecycle     │
│ PluginHost ─────────────── EntityStore ───── UI bridge                │
└─────────┼────────────────────┼─────────────────┼───────────────────────┘
          │ UDS + plugin token │                 │ Tauri events
┌─────────▼────────────────────┴──── agent-vm plugin ────────────────────┐
│ inventory · lifecycle · ConfigMirror · GuestBootstrap                 │
│              avm list/start/stop/create                               │
└─────────┬──────────────────────────────────────────────────────────────┘
          │ avm
┌─────────▼────────── dedicated host tmux ───────────────────────────────┐
│ one detached session per project/backend · limactl shell PTY          │
└─────────┬──────────────────────────────────────────────────────────────┘
┌─────────▼──────────────── Linux project VM ────────────────────────────┐
│ RW virtiofs project · ~/.claude · ~/.codex · interactive claude/codex │
└───────────────────────────────────────────────────────────────────────┘
```

### 6.1 Компоненты

#### `PluginHost`

Реализует discovery, manifest validation, enable, spawn, handshake, health,
restart backoff и versioned RPC из старой спеки. Первый first-party plugin
поставляется вместе с Jarvis и включается через onboarding Agent VM.

#### `ProjectRuntimeRegistry`

Связывает канонический host `cwd` с VM и активным чатом. Хранит только
управляющие метаданные; `agent-vm` Record остаётся источником фактов о VM.

#### `AgentVmController`

- читает Records из `~/.config/agent-vm/vms`;
- сверяет runtime через `limactl list --json`;
- вызывает `avm create/start/stop/restart`;
- переводит известные строки прогресса `avm` (`==> Phase …`) в
  best-effort operation steps; источником истины остаются exit status,
  Record и состояние Lima, а не текстовый parser;
- публикует `vm.<name>`;
- возвращает shell/resume commands;
- не парсит человекочитаемый `avm list`.

#### `ConfigMirror` и `GuestBootstrap`

Строят разрешённый snapshot host-конфигурации, проверяют fingerprint и
атомарно применяют его в конкретной VM. Подробнее — §9.

#### `SecretStore`

Новый фасад над macOS Keychain для Claude API/OAuth token и будущих
runtime-secrets. Текущий `service.claudeSecret`, если он заполнен в
`settings.json`, один раз переносится в Keychain и очищается из JSON только
после успешной записи и контрольного чтения. Plugin получает secret bytes
только на время адресного bootstrap конкретной VM.

#### `TerminalSessionManager`

На пару `projectId + backend`:

- вычисляет детерминированное безопасное имя tmux-session;
- переиспользует живую detached session и никогда не создаёт process на Enter;
- запускает `limactl shell` в guest workspace и обычный interactive CLI;
- отдаёт terminal snapshot транзитно через Tauri IPC;
- отправляет текст через именованный tmux buffer, без shell interpolation;
- разделяет bracketed paste и `Enter` коротким settle-window, чтобы TUI не
  оставлял вставленный prompt в строке ввода;
- поддерживает resize, Enter/Escape/стрелки и interrupt;
- отличает `starting`, `ready`, `working`, `exited` и `disconnected`.

#### `TerminalBridge`

Terminal output не публикуется в EntityStore и не пишется в project mount или
общий journal: экран может содержать секретный ввод. UI получает ограниченный
snapshot напрямую по Tauri IPC. Reconnect перечитывает scrollback живой pane.

## 7. Модель данных и состояния

### 7.1 Идентичности

```text
ProjectProfile 1 ── 1 ProjectRuntime/VM 1 ── N TerminalSession
```

- `projectId`: hash канонического host `cwd`, не basename;
- `vmName`: имя из agent-vm Record;
- `terminalId`: детерминированный ID `projectId + backend`;
- `tmuxSession`: безопасное имя detached session;
- `backend`: `claude` или `codex`.

Одинаковые basename в разных каталогах не конфликтуют. VM-name collision
показывается как setup error с явным выбором существующей VM или переименованием.

### 7.2 VM state

```text
unknown → provisioning → starting → ready → stopping → stopped
                    ↘ error       ↘ error      ↘ error
ready ↔ working
```

`working` — композиционное UI-состояние: VM `ready`, внутри есть активный run.
Backend entity хранит отдельно `runtimeState` и `agentState`.

### 7.3 Agent terminal state

```text
absent → starting → ready ↔ working
             ↘ exited
ready/working → disconnected → ready/working
```

`working` означает, что terminal process жив; точное внутреннее состояние TUI
не угадывается по тексту. Вопрос или permission prompt виден непосредственно
на экране. `disconnected` означает потерю attachment, а не смерть agent process.

### 7.4 Terminal snapshot

Транзитный ответ UI:

```json
{
  "terminalId": "project-…-claude",
  "backend": "claude",
  "vm": "sup-ac82ab61d14d",
  "state": "working",
  "screen": "…bounded rendered terminal screen…"
}
```

`screen` имеет жёсткий byte/line limit, не логируется и не сохраняется core.

### 7.5 Project Manager state

Каталог не смешивается с Agent VM autostart profiles:

```json
{
  "projectManager": {
    "folders": [
      {
        "projectId": "project-…",
        "project": "jarvis",
        "cwd": "/canonical/path/jarvis"
      }
    ],
    "favoriteProjectIds": ["project-…"],
    "view": "list"
  }
}
```

- `agentVm.projects` по-прежнему означает только проекты с включённым
  `startWithJarvis`;
- `projectManager.folders` — пользовательский каталог, который дополняет
  проекты из chat history и VM inventory;
- путь канонизируется backend-ом и должен указывать на существующую директорию;
- звезда проекта из history/inventory сначала закрепляет его в `folders`;
- порядок `favoriteProjectIds` является источником истины и не меняется от
  `updatedAt`, новых чатов или состояния VM;
- неизвестные поля блока сохраняются при точечной мутации;
- это private settings Jarvis: содержимое папки, agent configs, credentials,
  proxy и terminal output сюда не попадают.

## 8. Основные пользовательские сценарии

### 8.1 Первый запуск проекта

1. Пользователь открывает проект и нажимает `+ Claude`, `+ Codex` либо сразу
   отправляет первое сообщение.
2. Jarvis проверяет `avm`, Lima и Record.
3. Если `.agent-vm.yaml` отсутствует, Jarvis создаёт минимальный файл:

   ```yaml
   modules: [node, claude, codex]
   resources:
     cpus: 4
     memory: 4GiB
     disk: 120GiB
   ```

   Существующий файл никогда не перезаписывается. UI сообщает, что новый
   project config добавлен в рабочее дерево.

4. Jarvis вызывает `avm create <cwd>`.
5. Project view остаётся доступным и показывает живые этапы provisioning.
6. После `VM ready` выполняются ConfigMirror и GuestBootstrap.
7. Jarvis создаёт persistent terminal session, подключает viewport и отправляет
   исходное сообщение в неё; повторно нажимать «Запустить» не требуется.
8. На готовность приходит toast; если пользователь уже смотрит этот project
   chat, toast подавляется и остаётся inline-status.

### 8.2 Повторный запуск

1. `stopped` VM автоматически получает `avm start <name>`.
2. ConfigMirror сравнивает fingerprint и переносит только изменившиеся
   разрешённые файлы.
3. Если terminal session уже жива, Jarvis подключается к ней без bootstrap и
   без запуска нового Claude/Codex.
4. Новый run создаётся либо существующий продолжается по backend session ID.
5. VM уже `ready` — сообщение уходит без отдельного подтверждения.

### 8.3 Сообщение во время работы

В v1 одновременно выполняется один turn на project runtime. Следующее сообщение
можно:

- поставить одним follow-up в очередь;
- заменить ещё не запущенный follow-up;
- отменить через `Stop`.

UI явно показывает `Отправится следом`. Неограниченной очереди нет.

### 8.4 Вход через Terminal

В project header есть action `Войти в VM`:

- основной copy: `avm shell <vm-name>`;
- вторичный copy после появления session ID:
  `avm shell <vm-name>`, затем готовая команда
  `claude --resume <id>` или `codex resume <id>`.

Jarvis не запускает Terminal автоматически и не пытается отрисовывать его.

### 8.5 Перезапуск Jarvis

- PluginHost поднимается автоматически;
- registry и Records восстанавливают VM-состояния;
- RunStore восстанавливает чаты;
- активный на момент остановки turn становится `interrupted`;
- пользователь видит `Продолжить` — следующий headless invocation идёт через
  backend resume;
- закреплённые runtimes со `startWithJarvis=true` запускаются с concurrency 1,
  чтобы несколько Lima boots не забили машину.

## 9. Файлы, конфиги и секреты

### 9.1 Project workspace

Вся каноническая директория проекта передаётся `avm create <cwd>` и
монтируется штатным virtiofs mount `read-write`. Это и есть общий файловый
контракт:

- host и VM видят одни и те же изменения;
- отдельного upload/download для project files нет;
- file viewer/diff Jarvis продолжает читать host paths;
- guest path из событий переписывается в соответствующий host path до
  попадания в текущий file/diff pipeline.

Jarvis доверяет проекту только после явного запуска пользователем. В UI рядом с
первым запуском написано: «Агент сможет изменять файлы этого проекта».

### 9.2 Автоматический allowlist конфигурации

Проектные инструкции уже находятся в RW mount и не копируются отдельно:
`CLAUDE.md`, `.claude/`, `.mcp.json`, `AGENTS.md`, `.codex/` внутри repo.

User-scoped snapshot:

| Backend | Переносим                                                                                                                                                       | Не переносим                                                                                         |
| ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| Claude  | `~/.claude/settings.json`, `~/.claude/CLAUDE.md`, `agents/`, `commands/`, `skills/`, project-scoped `projects/<host-cwd>/memory/`, декларативный список plugins | session JSONL, tool-results, transcripts, history, debug, cache, file-history, весь `~/.claude.json` |
| Codex   | `~/.codex/config.toml`, `~/.codex/AGENTS.md`, `skills/`                                                                                                         | sessions/rollouts, logs, cache, временные файлы                                                      |
| Git     | `.gitconfig` через штатную sanitization agent-vm                                                                                                                | credential helpers, stored credentials, private SSH keys                                             |

Из `~/.claude.json` разрешается извлечь только user-scoped `mcpServers`.
Файл целиком не копируется: он смешивает OAuth, project trust и caches.

Абсолютные macOS paths в MCP/config проверяются до переноса:

- path внутри project root переписывается на guest workspace;
- известный shared config path переписывается на guest home;
- остальные серверы помечаются `Не перенесён: host-only path` и видны в
  Config status; тихо запускать сломанную конфигурацию нельзя.

### 9.3 Merge-правила

- ConfigMirror формирует snapshot, не мутируя host source.
- Snapshot читает только обычные файлы внутри allowlist roots, не следует по
  symlink за их пределы и ограничивает размер одного файла/общего архива.
- Запись в guest идёт во временный файл/каталог и завершается atomic rename.
- Jarvis-owned runtime overlay имеет приоритет только для transport:
  output JSONL, session persistence, permission mode и guest-safe hooks.
- Пользовательские model/provider/MCP/skills сохраняются.
- Host-specific Jarvis hooks не копируются буквально; они заменяются
  guest-safe runtime bridge либо удаляются с диагностикой.
- Project memory re-key-ится с canonical host `cwd` на guest workspace key и
  синхронизируется owner-private при каждом `runtime.ensure`; работающая VM
  при этом не перезапускается.
- Guest edits user-config не возвращаются на host и будут заменены следующим
  snapshot. Project-scoped config в RW mount остаётся двусторонним.

### 9.4 Секреты

Документированный `/mnt/host/agent-vm` read-only, но общий для всех project VM.
Поэтому Jarvis **не складывает туда новые per-project токены**.

Схема доставки:

1. При первом включении Agent VM пользователь один раз разрешает
   `Использовать текущие логины Claude и Codex в project VM`.
2. Claude:
   - API/OAuth token хранится новым `SecretStore` в macOS Keychain;
   - существующий plaintext `service.claudeSecret` безопасно мигрируется и
     удаляется из `settings.json`;
   - если переносимого token ещё нет, setup card предлагает выполнить
     `claude setup-token` и сохранить результат в Keychain;
   - Jarvis не пытается экспортировать subscription login из macOS Keychain.
3. Codex:
   - читается текущий `~/.codex/auth.json`, если выбран file credential store;
   - Keychain-only credential автоматически не экспортируется.
4. Secret bytes передаются GuestBootstrap через stdin адресно в одну VM,
   никогда не помещаются в argv/env host-процесса и не логируются.
5. В guest создаются `~/.claude/.credentials.json` либо защищённый
   `~/.jarvis-vm/agent.env`, а также `~/.codex/auth.json`, владелец — VM user,
   mode `0600`.
6. В journal пишутся только `secret kind`, fingerprint и результат доставки.

`.env`, AWS/GCP/Azure credentials, SSH private keys и произвольные secret
folders автоматически не передаются. Их поддержка — отдельная будущая
allowlist-функция.

## 10. Headless runtime protocol

### 10.1 Запуск

Плагин вызывает только массив argv, без shell-конкатенации пользовательских
строк:

```text
limactl shell --workdir <guest-workspace> <vm> -- <agent> <structured-flags>
```

Prompt передаётся через stdin либо безопасный stdin protocol. Имена VM и пути
валидируются по Record; shell quoting не является границей безопасности.

Claude adapter использует session persistence и `stream-json`; Codex adapter —
`exec --json`. Конкретные опциональные флаги определяются capability probe
установленной CLI-версии, чтобы обновление CLI не ломало весь plugin.

### 10.2 Поток и backpressure

- stdout читается построчно;
- одна JSONL-строка ограничена 1 MiB;
- UI delta coalesce-ится каждые 40–80 мс, journal хранит исходную
  нормализованную последовательность;
- stderr ограничивается кольцевым буфером 64 KiB, проходит sanitizer и
  показывается только в диагностике/ошибке;
- event queue между plugin и core ограничена; text deltas можно объединять,
  lifecycle/result/file/question события терять нельзя;
- каждый event имеет `seq`, UI может запросить replay `afterSeq`.

### 10.3 Файлы и результаты

`tool_use`/Codex item events дают изменённые paths. После turn plugin:

1. нормализует guest path в host path;
2. проверяет, что path находится внутри project mount;
3. строит список изменённых файлов через существующий git diff pipeline;
4. сохраняет итоговый result;
5. запускает существующий turn analysis;
6. отдаёт UI result card: summary, files, tests/commands, warnings.

Никакого отдельного копирования результата из VM не нужно: project mount общий.

### 10.4 Questions и approvals

Структурированный вопрос преобразуется в существующую question-card Jarvis.
Пока ответ не получен, run = `waiting`. Ответ идёт через adapter в следующий
input/resume либо через поддержанный protocol текущего процесса.

Если backend в headless режиме не может продолжить текущий process после
question, plugin завершает turn как `waiting`, сохраняет backend session ID и
после ответа вызывает resume. Это видимая семантика, не ошибка.

## 11. UI/UX

UI использует текущий визуальный язык Jarvis: компактность Raycast, клавиатурная
навигация, системная типографика и минимум постоянного chrome. Agent VM не
получает отдельный «технический мир» в настройках.

### 11.1 Главная страница

Над списком сессий появляется компактная полоса `Активные среды`, только если
есть `starting/ready/working/error` VM:

```text
Активные среды   ● jarvis  Codex работает   12м   ›
                 ◐ api     запускается      46с   ›
```

- высота одной строки соответствует обычной session row;
- `working` имеет мягкий pulse, учитывающий reduced motion;
- state выражен и цветом, и текстом/иконкой;
- клик открывает project workspace;
- VM с ожидающим агентом сортируется выше просто `ready`;
- error не исчезает, пока пользователь не увидел либо не повторил действие.

Обычная session row получает host badge `VM · <name>`. Отдельную полосу не
показываем, если та же информация не несёт действия.

### 11.2 Список проектов

Project Manager объединяет chat history, VM inventory, autostart profiles и
папки, добавленные пользователем:

- верхний поиск фильтрует только имя и путь проекта;
- `Добавить папку` открывает системный macOS folder picker;
- избранные идут отдельной первой секцией и не пересортировываются
  автоматически;
- звезда добавляет/убирает проект, стрелки двигают избранное выше/ниже;
- переключатель сохраняет `list` либо `cards`; карточки почти квадратные;
- строка/карточка показывает имя, путь, состояние VM и быстрый вход в
  Claude/Codex;
- история остаётся доступна компактной иконкой без числа чатов;
- tags, chat count, transcript summary, model и прочая метрика на этом уровне
  не показываются;
- online sidecar не дублируется большой зелёной карточкой. Status banner
  появляется только для запуска, retry, несовместимости или ошибки.

CPU/RAM/disk и autostart живут в environment detail, а не в каталоге.

### 11.3 Project workspace

Три логических зоны в одном окне:

1. **Header** — back, project, branch, VM state, backend picker, Stop/Run.
2. **Conversation** — текущая лента сообщений и collapsed tool groups.
3. **Result drawer** — изменённые файлы, diff, команды/tests и итог хода.

На узкой панели result drawer открывается slide-over, используя уже
существующий document/diff viewer. На широкой будущей раскладке он может стать
правой колонкой без изменения data contract.

Environment popover:

- VM name и state;
- CPU/RAM/disk из Record;
- workspace `Read & write`;
- config fingerprint и предупреждения переноса;
- `Скопировать avm shell …`;
- `Перезапустить VM`, `Остановить VM`;
- destructive `Удалить VM` не входит в primary actions и требует отдельного
  подтверждения.

### 11.4 Provisioning

Первый запуск показывает inline stepper:

```text
Подготавливаю среду
✓ Конфигурация проекта
✓ Создание VM
● Установка Claude и Codex
○ Перенос настроек
○ Запуск агента
```

- UI остаётся навигируемым;
- текущий этап и последние безопасные log lines доступны по раскрытию;
- elapsed timer живой, ETA не выдумываем;
- cancel доступен до запуска агента;
- retry продолжает из Record через `avm recreate` только после явного
  объяснения, что recreate пересоздаёт VM.

### 11.5 Chat и result

- текст ассистента стримится с сохранением пользовательского scroll position;
- tool events группируются в компактные chips;
- вопросы используют существующий красивый picker;
- file changes появляются сразу, final diff уточняется после turn;
- result card завершает каждый ход и остаётся сворачиваемой;
- input доступен во время работы: сообщение становится одним queued follow-up;
- `Esc` закрывает drawer, повторный `Esc` возвращает к проектам;
- основные действия доступны с клавиатуры и имеют видимые focus states.

## 12. Уведомления

### 12.1 События

| Событие                      | Inline |               System toast |                       TTS |
| ---------------------------- | -----: | -------------------------: | ------------------------: |
| Provisioning начался         |     Да |                        Нет |                       Нет |
| VM готова                    |     Да | Да, если project не открыт |              По настройке |
| VM start/restart failed      |     Да |                         Да |                   Коротко |
| Агент начал turn             |     Да |                        Нет |                       Нет |
| Агент ждёт ответа            |     Да |                         Да | По существующей настройке |
| Агент закончил               |     Да |    Да, если chat не открыт | По существующей настройке |
| Агент/VM аварийно завершился |     Да |                         Да |                        Да |
| Обычная ручная остановка     |     Да |                        Нет |                       Нет |

Toast ведёт прямо в project/run, а не просто открывает главную панель.

### 12.2 Дедупликация

Ключ: `<projectId>:<runtime|run>:<transition>`. Повторный poll того же состояния
не создаёт уведомление. `ready → working → ready` допустим, `ready → ready` —
нет. Несколько завершений за короткий период используют существующий
coalescing voice queue.

## 13. Автозапуск и ресурсы

- По умолчанию VM запускается только при первом действии пользователя.
- Опция project profile `Запускать VM вместе с Jarvis` выключена.
- Закреплённые VM стартуют последовательно, concurrency = 1.
- Агент автоматически не начинает новую задачу без prompt.
- Idle auto-stop выключен в v1: неожиданная остановка долгоживущих dev
  сервисов опаснее экономии памяти. Есть ручной Stop.
- Menu-bar quit прекращает supervisor turns, но не удаляет и не обязана
  останавливать VM.

## 14. Ошибки и восстановление

| Ошибка                        | Поведение                                                                         |
| ----------------------------- | --------------------------------------------------------------------------------- |
| `avm`/Lima не установлен      | Setup card с диагностикой и явной установкой, без бесконечного spinner            |
| `.agent-vm.yaml` невалиден    | Показываем parser error и открываем файл; не перезаписываем                       |
| Record orphaned               | Объясняем `recreate`/`prune`; автоматического destructive выбора нет              |
| VM не стартует                | Entity `error`, безопасный stderr tail, Retry                                     |
| Config несовместим с Linux    | Остальной snapshot применяется, конкретный item получает warning                  |
| Auth отсутствует/просрочен    | Run не стартует; focused auth card, остальные VM-функции работают                 |
| JSONL line битая              | Логируем sanitized sample/hash, поток продолжается                                |
| CLI event schema изменилась   | `backend.unmapped`; result/exit остаются видимы; incompatible после порога ошибок |
| Plugin упал                   | PluginHost restart backoff, entities stale, UI показывает reconnecting            |
| Jarvis закрылся во время turn | Journal сохранён, turn `interrupted`, доступен Resume                             |
| Project path исчез            | Runtime не запускается; Record не удаляется                                       |

## 15. Безопасность

- Plugin identity — per-plugin token, UDS `0600`, provenance
  `plugin:agent-vm`.
- Внешние команды собираются argv-массивами; prompt/config идут через stdin.
- Project path канонизируется до вызова plugin.
- Guest path принимается только после маппинга в известный workspace.
- UI file/diff сохраняет текущую проверку обычного файла и provenance.
- Secrets redaction применяется к stderr, journal и diagnostics.
- Secret bootstrap не содержит значения в argv, process title или toast.
- Runtime secrets хранятся на host в macOS Keychain; legacy plaintext
  мигрируется до первого VM bootstrap.
- Guest credential files — `0600`; guest config directories — `0700`.
- Публичный репозиторий содержит только синтетические fixtures. Рабочие
  настройки, employer-specific данные, credential-файлы и LLM proxy values
  никогда не записываются в checkout, commit, patch или smoke-артефакт.
- Host-side Agent VM state хранится только под `<jarvis-dir>/agent-vm`; файлы
  bootstrap создаются внутри private guest home, вне RW project mount.
- Перед каждым коммитом Agent VM выполняется `npm run check:public`; любое
  совпадение блокирует коммит, а диагностика выводит только категорию и путь,
  не найденное значение.
- Удаление/recreate VM всегда отдельное подтверждение.
- VM — изоляция процессов, но RW project mount означает реальную возможность
  менять host-код; UI говорит об этом до первого запуска.
- Сеть VM не объявляется безопасной границей: permission/runtime overlay не
  должен обещать сетевую изоляцию, которой Lima-конфиг не обеспечивает.

## 16. Контракты ядра и плагина

### 16.1 Дополнение manifest

```json
{
  "id": "agent-vm",
  "protocolVersion": 1,
  "projectRuntimes": [
    {
      "id": "vm",
      "title": "Agent VM",
      "agents": ["claude", "codex"],
      "workspace": "host-mount-rw",
      "supports": ["terminal", "reconnect", "input", "files", "shell-command"]
    }
  ]
}
```

`launchTargets` старой спеки заменяется более точным `projectRuntimes`.
Lifecycle VM остаётся в плагине, а terminal transport работает через
ограниченные in-process Tauri commands и известную entity выбранной VM.

### 16.2 RPC

- `runtime.ensure {projectId,cwd,agent}` → operation ID;
- `runtime.status {projectId}`;
- `runtime.stop {projectId}`;
- `runtime.restart {projectId}`;
- `runtime.commands {projectId}`.

Long operations не держат IPC request открытым. Ответ возвращает operation ID,
дальше идут события.

### 16.3 Terminal Tauri IPC

- `agent_vm_terminal_ensure {projectId,backend}` → terminal identity/state;
- `agent_vm_terminal_snapshot {terminalId}` → bounded transient screen;
- `agent_vm_terminal_input {terminalId,text,submit}` → accepted;
- `agent_vm_terminal_key {terminalId,key}` → accepted;
- `agent_vm_terminal_resize {terminalId,cols,rows}` → applied;
- `agent_vm_terminal_stop {terminalId}` → stopped.
- `agent_vm_commands_get {projectId,cwd,backend}` → каталог встроенных,
  пользовательских, проектных и plugin slash-команд для подсказок composer.

Каждая команда повторно проверяет, что terminal ID соответствует живой
`plugin:agent-vm` VM entity и выбранному backend. Произвольные tmux session,
VM name, shell fragment или host path UI передать не может.
Каталог команд отдельно сверяет `projectId` с canonical `cwd`; выбор подсказки
только дополняет поле и не отправляет команду без подтверждения пользователя.

### 16.4 Project Manager Tauri IPC

- `project_manager_state_get` → private catalog/favorites/view;
- `project_manager_folder_pick` → system picker, canonical folder и новый state;
- `project_manager_favorite_set {cwd,favorite}` → canonical upsert и новый state;
- `project_manager_favorite_move {projectId,direction}` → порядок избранного;
- `project_manager_view_set {view}` → `list | cards`.

Folder picker запускается фиксированным скриптом без пользовательских
подстановок. Отмена возвращается как `{ok:true,cancelled:true}`. Все остальные
команды повторно валидируют идентификатор либо канонизируют путь на backend.

## 17. Тестирование

### 17.1 Pure/unit

- projectId/path mapping и VM-name collision;
- VM/terminal state reducers;
- deterministic terminal/session identity;
- tmux argv/shell quoting и allowlist клавиш;
- config allowlist, path rewrite, merge и fingerprint;
- secret redaction и запрет попадания secret bytes в argv/log;
- notification transition/dedupe;
- manifest/schema validation.

### 17.2 Plugin integration с fake runner

- отсутствующая VM → init/create/start/bootstrap;
- stopped VM → start без recreate;
- provisioning progress и failure;
- config snapshot применяется атомарно;
- auth missing/expired;
- plugin crash → stale/restart;
- повторный ensure с совпавшим fingerprint не читает Keychain и не копирует
  bundle повторно.

Реальный `limactl` в автоматических тестах не запускается.

### 17.3 UI contract

- active VM rail показывается только при активных/error VM;
- project card состояния и быстрые actions;
- каталог произвольных папок и системный picker contract;
- поиск только по имени/пути;
- favorite toggle, ручной порядок и persistence;
- list/cards view и отсутствие chat count/transcript summary;
- отсутствие online-status banner при штатно подключённом sidecar;
- provisioning stepper;
- реальный terminal viewport, reconnect, input и special keys;
- project files/diff drawer;
- shell command copy;
- notification deep link;
- keyboard navigation, focus, reduced motion и empty/error states.

### 17.4 Ручной macOS smoke

1. Чистый project без `.agent-vm.yaml`.
2. Первый prompt → config → VM → bootstrap → persistent Claude terminal.
3. Второй prompt → тот же tmux pane и тот же Claude PID, без bootstrap.
4. Файл изменился в VM и сразу виден host/diff viewer.
5. Закрытие/reopen project view подключается к той же pane.
6. Stop/start terminal и VM.
7. Codex terminal с тем же UX.
8. VM подсвечена на главной.
9. Ready/exited/error дают ровно одно кликабельное уведомление.
10. `avm shell <vm>` открывает workspace.
11. Проверка, что secrets и terminal output отсутствуют в
    log/journal/process argv.

## 18. Инкременты реализации

Каждый инкремент — отдельный проверяемый commit/PR; feature остаётся под
`agentVmRuntime` до завершения UI smoke.

1. **PluginHost foundation**

   Manifest/discovery/spawn/handshake/supervision/versioned RPC, quotas
   EntityStore, fake plugin.

2. **Agent VM inventory и lifecycle**

   Record parser, `limactl --json`, `avm` lifecycle, project binding,
   `.agent-vm.yaml` creation, `vm.*`, shell commands.

3. **ConfigMirror и GuestBootstrap**

   SecretStore/Keychain и legacy migration, allowlist, fingerprint, path
   rewrite, безопасная адресная доставка auth, readiness diagnostics.

4. **Persistent Terminal Runtime**

   Dedicated tmux server, deterministic project/backend session, interactive
   `limactl shell`, transient capture/input/reconnect and terminal status.

5. **Project Manager UI**

   active VM rail, project cards, project workspace, provisioning stepper,
   terminal viewport, project files/diff drawer, environment popover. Обязательная
   проверка живого UI и скриншотами, не только DOM-тестами.

6. **Notifications, autostart и recovery**

   transition dedupe, deep links, TTS routing, pinned sequential start,
   restart recovery и interrupted/resume.

7. **Hardening и rollout**

   real VM smoke Claude+Codex, schema drift, log redaction, performance,
   migration/feature flag, документация пользователя.

## 19. Acceptance criteria v1

- [ ] Из существующего проекта можно одним действием открыть постоянного Claude
      или Codex; Jarvis сам доводит среду до запуска и отправляет первый ввод.
- [ ] Весь project root доступен VM read-write через штатный `agent-vm`.
- [ ] Исходники `agent-vm` не менялись.
- [ ] Разрешённые user-конфиги Claude/Codex автоматически применяются.
- [ ] Авторизация адресно попадает только в выбранную VM и не появляется в
      argv/log/journal.
- [ ] Claude runtime secret хранится в macOS Keychain, legacy plaintext после
      проверенной миграции очищен.
- [ ] Реальный terminal screen и ввод транслируются в UI без реконструкции чата.
- [ ] Два последовательных prompt используют ту же tmux pane и тот же agent PID.
- [ ] Закрытие и повторное открытие project view не останавливает агента.
- [ ] Active/starting/error VM видны на главной и в Project Manager.
- [ ] Есть copy action для корректного private-profile `avm shell`.
- [ ] Waiting/done/ready/error уведомления дедуплицированы и открывают нужный
      project/run.
- [ ] UI имеет законченные loading/empty/error/focus/reduced-motion states и
      проверен на запущенном приложении.
- [ ] После рестарта окна Jarvis viewport reconnect-ится к живой detached
      terminal session; реально умершая session не показывается работающей.

## 20. Отложенные расширения

- read-only project mount после появления штатного контракта в `agent-vm`;
- clone-mode project runtime;
- per-project allowlist дополнительных secrets/config folders;
- несколько параллельных runs и worktrees;
- semantic extraction tools/files/results поверх terminal, только если она
  не меняет terminal как source of truth;
- сторонние runtime providers поверх стабилизированного PluginHost protocol.
