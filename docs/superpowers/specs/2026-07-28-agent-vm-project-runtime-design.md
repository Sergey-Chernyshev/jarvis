# Agent VM как проектный runtime внутри Jarvis

Дата: 2026-07-28

Основа: [`2026-07-03-plugin-system-agent-vm-design.md`](2026-07-03-plugin-system-agent-vm-design.md)

Статус: v2, письменное ревью перед implementation plan

## 1. Решение

Jarvis получает первый полноценный **project runtime**: пользователь открывает
проект, выбирает Claude или Codex и пишет задачу. Дальше Jarvis сам:

1. находит либо создаёт project VM через существующий `agent-vm`;
2. запускает VM при необходимости;
3. переносит разрешённые настройки Claude/Codex и адресно доставляет авторизацию;
4. запускает coding agent без внешнего Terminal;
5. показывает живой чат, инструменты, изменённые файлы и итог;
6. уведомляет о готовности VM, вопросах, завершении и ошибках;
7. даёт готовую команду входа в VM, если пользователю всё-таки нужен терминал.

Встроенный терминальный эмулятор не строим. Основной интерфейс — Project Manager
и чат Jarvis; Terminal остаётся диагностическим escape hatch.

### 1.1 Зафиксированные продуктовые решения

| Область | Решение v1 |
|---|---|
| Workspace | Весь каталог проекта монтируется в VM через штатный mount mode `agent-vm` |
| Доступ к проекту | Только `read-write`, как уже умеет `agent-vm`; `read-only` не показываем и не эмулируем |
| Изменения в `agent-vm` | Не требуются |
| Запуск | Ленивый: VM поднимается при первом сообщении или явном «Запустить» |
| Запуск вместе с Jarvis | Только для проектов, которые пользователь закрепил для автозапуска |
| Агент | Headless Claude/Codex со структурированным event-stream |
| Терминал | Не отображаем; показываем и копируем `avm shell <vm>` и команду resume |
| Конфиги | Claude и Codex зеркалируются автоматически по явному allowlist |
| Секреты | Не синхронизируются как обычные файлы; доставляются адресно, без аргументов команд и логов |
| Главная страница | Активные VM видны отдельной компактной полосой и на карточках сессий |
| Уведомления | Готовность/ошибка VM, ожидание ответа, завершение агента, аварийная остановка |
| UI | Полноценный project workspace, а не набор технических кнопок плагина |

## 2. Почему старая спека меняется

Старая версия правильно выбрала out-of-process PluginHost, EntityStore и
first-party plugin `agent-vm`, но оставляла Jarvis в роли монитора: запуск агента
происходил в новом окне Terminal, а чат попадал в Jarvis позднее через hooks и
transcript.

Новый продуктовый контракт сильнее:

- Terminal не нужен для нормальной работы;
- запуск, остановка и продолжение агента принадлежат UI Jarvis;
- чат и результаты должны приходить в реальном времени;
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

Это означает, что новая работа — не второй чат с нуля. Нужны remote runtime,
нормализованный transport и новый project-oriented view поверх существующих
рендереров.

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

1. Из Project Manager можно создать, запустить и использовать VM-агента, не
   открывая Terminal.
2. После первого provisioning повторный запуск проекта не требует ручной
   настройки VM или повторного логина.
3. Chat delta, tool activity, file changes, questions и result появляются в UI
   не позднее секунды после получения соответствующей JSONL-строки плагином.
4. Состояние VM и агента видно на главной и в Project Manager.
5. Значимые переходы состояния дают кликабельное уведомление без дублирующего
   шума.
6. Перезапуск Jarvis не теряет уже записанную историю; незавершённый ход честно
   становится `interrupted` и может быть продолжен через session ID.

### 4.2 Не входит в v1

- read-only mount и выбор access mode;
- встроенный terminal emulator;
- clone-mode как путь автоматического создания проекта;
- синхронизация всего `$HOME`;
- копирование `.env`, private SSH keys, cloud credentials и macOS Keychain;
- обратная синхронизация гостевых user-конфигов на host;
- несколько одновременно работающих агентов в одной project VM;
- marketplace сторонних runtime-плагинов;
- гарантированное продолжение работающего turn после завершения процесса
  Jarvis: VM выживает, текущий turn помечается `interrupted`.

## 5. Рассмотренные runtime-подходы

### A. Интерактивный TTY/tmux и разбор экрана

Jarvis запускает обычный TUI внутри guest tmux, читает pane capture и отправляет
клавиши.

- Плюс: терминальная сессия естественно attachable.
- Минусы: ANSI, resize, полноэкранный TUI, вопросы и tool activity невозможно
  надёжно превратить в продуктовые события; UI неизбежно становится логом.

Отклонён как основной transport. tmux может остаться ручным диагностическим
режимом, но не источником данных.

### B. Только hooks и tail transcript

Повторяет локальную интеграцию Jarvis: guest hooks присылают lifecycle, host
читает transcript раз в секунду.

- Плюс: минимум новой parser-логики.
- Минусы: нет надёжного канала ввода, холодный transcript появляется не сразу,
  путь гостевого файла недоступен host напрямую, Codex/Claude имеют разные
  ограничения hooks.

Отклонён как единственный transport. Transcript остаётся recovery-источником.

### C. Структурированный headless runtime — выбран

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

Минус: текущий turn привязан к supervisor Jarvis. При падении host-процесса он
не считается живым; следующий turn использует resume.

## 6. Архитектура

```text
┌──────────────────────────── Jarvis core ──────────────────────────────┐
│ ProjectRuntimeRegistry  RunStore(JSONL)  NotificationRouter           │
│ SecretStore(Keychain)       ▲                 ▲                       │
│         │ RPC/events         │ normalized     │ lifecycle             │
│ PluginHost ─────────────── EntityStore ───── UI bridge                │
└─────────┼────────────────────┼─────────────────┼───────────────────────┘
          │ UDS + plugin token │                 │ Tauri events
┌─────────▼────────────────────┴──── agent-vm plugin ────────────────────┐
│ inventory · lifecycle · ConfigMirror · GuestBootstrap · RunSupervisor │
│     avm list/start/stop/create          limactl shell + JSONL parser   │
└─────────┬───────────────────────────────────────────┬──────────────────┘
          │ avm                                       │ limactl shell
┌─────────▼──────────────── Linux project VM ─────────▼──────────────────┐
│ RW virtiofs project · ~/.claude · ~/.codex · claude/codex headless    │
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

#### `RunSupervisor`

На один активный project run:

- запускает remote agent process;
- читает stdout JSONL и отдельно stderr;
- нормализует события;
- ведёт монотонный `seq`;
- пишет event journal;
- поддерживает cancel и один queued follow-up;
- завершает run по process exit/result/error.

#### `RunStore`

Пишет append-only journal
`<jarvis-dir>/agent-vm/runs/<run-id>.jsonl` с правами `0600` и компактный index.
Run сначала сохраняется, затем событие emit-ится UI: перезапуск окна не теряет
историю.

## 7. Модель данных и состояния

### 7.1 Идентичности

```text
ProjectProfile 1 ── 1 ProjectRuntime/VM 1 ── N AgentRun 1 ── N RunEvent
```

- `projectId`: hash канонического host `cwd`, не basename;
- `vmName`: имя из agent-vm Record;
- `runId`: UUID Jarvis, одна непрерывная лента чата;
- `backendSessionId`: Claude/Codex session/thread ID для resume;
- `turnId`: UUID одного сообщения пользователя и ответа агента.

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

### 7.3 Agent state

```text
idle → starting → working → waiting → working → completed
                    ↘ failed
                    ↘ cancelled
                    ↘ interrupted
```

`waiting` означает структурированный вопрос/разрешение, а не отсутствие stdout.
`interrupted` используется при потере supervisor или рестарте Jarvis во время
turn.

### 7.4 Нормализованный `RunEvent`

Обязательный envelope:

```json
{
  "runId": "uuid",
  "turnId": "uuid",
  "seq": 42,
  "at": 1785250000000,
  "type": "assistant.delta",
  "payload": {},
  "backend": "claude",
  "vm": "jarvis"
}
```

Типы v1:

- `run.started`, `run.resumed`;
- `user.message`;
- `assistant.delta`, `assistant.message`;
- `tool.started`, `tool.completed`, `tool.failed`;
- `file.changed`;
- `question.opened`, `question.answered`;
- `usage.updated`;
- `result.completed`;
- `run.cancelled`, `run.failed`, `run.interrupted`.

Неизвестный upstream event сохраняется как диагностический
`backend.unmapped`, но не ломает поток.

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
7. Исходное сообщение автоматически отправляется агенту; повторно нажимать
   «Запустить» не требуется.
8. На готовность приходит toast; если пользователь уже смотрит этот project
   chat, toast подавляется и остаётся inline-status.

### 8.2 Повторный запуск

1. `stopped` VM автоматически получает `avm start <name>`.
2. ConfigMirror сравнивает fingerprint и переносит только изменившиеся
   разрешённые файлы.
3. Новый run создаётся либо существующий продолжается по backend session ID.
4. VM уже `ready` — сообщение уходит без отдельного подтверждения.

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

| Backend | Переносим | Не переносим |
|---|---|---|
| Claude | `~/.claude/settings.json`, `~/.claude/CLAUDE.md`, `agents/`, `commands/`, `skills/`, декларативный список plugins | transcripts, history, debug, cache, file-history, весь `~/.claude.json` |
| Codex | `~/.codex/config.toml`, `~/.codex/AGENTS.md`, `skills/` | sessions/rollouts, logs, cache, временные файлы |
| Git | `.gitconfig` через штатную sanitization agent-vm | credential helpers, stored credentials, private SSH keys |

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

Карточка проекта содержит:

- имя, путь и branch;
- environment badge: `Нет VM / Создаётся / Готова / Работает / Ошибка`;
- backend/model активного или последнего run;
- краткую текущую задачу либо последний result;
- primary action `Открыть` или `Продолжить`;
- quick action `+ Claude`, `+ Codex`.

Карточки не превращаются в dashboard метрик. CPU/RAM/disk живут в detail.

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

| Событие | Inline | System toast | TTS |
|---|---:|---:|---:|
| Provisioning начался | Да | Нет | Нет |
| VM готова | Да | Да, если project не открыт | По настройке |
| VM start/restart failed | Да | Да | Коротко |
| Агент начал turn | Да | Нет | Нет |
| Агент ждёт ответа | Да | Да | По существующей настройке |
| Агент закончил | Да | Да, если chat не открыт | По существующей настройке |
| Агент/VM аварийно завершился | Да | Да | Да |
| Обычная ручная остановка | Да | Нет | Нет |

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

| Ошибка | Поведение |
|---|---|
| `avm`/Lima не установлен | Setup card с диагностикой и явной установкой, без бесконечного spinner |
| `.agent-vm.yaml` невалиден | Показываем parser error и открываем файл; не перезаписываем |
| Record orphaned | Объясняем `recreate`/`prune`; автоматического destructive выбора нет |
| VM не стартует | Entity `error`, безопасный stderr tail, Retry |
| Config несовместим с Linux | Остальной snapshot применяется, конкретный item получает warning |
| Auth отсутствует/просрочен | Run не стартует; focused auth card, остальные VM-функции работают |
| JSONL line битая | Логируем sanitized sample/hash, поток продолжается |
| CLI event schema изменилась | `backend.unmapped`; result/exit остаются видимы; incompatible после порога ошибок |
| Plugin упал | PluginHost restart backoff, entities stale, UI показывает reconnecting |
| Jarvis закрылся во время turn | Journal сохранён, turn `interrupted`, доступен Resume |
| Project path исчез | Runtime не запускается; Record не удаляется |

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
  "projectRuntimes": [{
    "id": "vm",
    "title": "Agent VM",
    "agents": ["claude", "codex"],
    "workspace": "host-mount-rw",
    "supports": ["chat", "resume", "cancel", "files", "shell-command"]
  }]
}
```

`launchTargets` старой спеки заменяется более точным `projectRuntimes`: новый
контракт возвращает не terminal launch-spec, а управляемый run.

### 16.2 RPC

- `runtime.ensure {projectId,cwd,agent}` → operation ID;
- `runtime.status {projectId}`;
- `runtime.send {runId?,message,agent}` → run/turn IDs;
- `runtime.cancel {runId}`;
- `runtime.stop {projectId}`;
- `runtime.restart {projectId}`;
- `runtime.commands {projectId,runId?}`;
- `runtime.replay {runId,afterSeq}`.

Long operations не держат IPC request открытым. Ответ возвращает operation ID,
дальше идут события.

### 16.3 Core → UI

- `runtime:state`;
- `runtime:operation`;
- `run:event`;
- `run:recovered`;
- `config:mirror-status`.

UI после mount/reopen сначала получает snapshot, потом подписывается на events;
гонка закрывается `seq`/replay.

## 17. Тестирование

### 17.1 Pure/unit

- projectId/path mapping и VM-name collision;
- VM/agent state reducers;
- Claude/Codex JSONL → `RunEvent`;
- monotonic seq, replay и delta coalescing;
- config allowlist, path rewrite, merge и fingerprint;
- secret redaction и запрет попадания secret bytes в argv/log;
- notification transition/dedupe;
- manifest/schema validation.

### 17.2 Plugin integration с fake runner

- отсутствующая VM → init/create/start/bootstrap/send;
- stopped VM → start без recreate;
- provisioning progress и failure;
- config snapshot применяется атомарно;
- auth missing/expired;
- cancel убивает remote invocation;
- plugin crash → stale/restart/replay;
- malformed/upgraded backend events.

Реальный `limactl` в автоматических тестах не запускается.

### 17.3 UI contract

- active VM rail показывается только при активных/error VM;
- project card состояния и быстрые actions;
- provisioning stepper;
- live chat, queued follow-up, cancel;
- result/files/diff drawer;
- shell/resume command copy;
- notification deep link;
- keyboard navigation, focus, reduced motion и empty/error states.

### 17.4 Ручной macOS smoke

1. Чистый project без `.agent-vm.yaml`.
2. Первый prompt → config → VM → bootstrap → live Claude result.
3. Файл изменился в VM и сразу виден host/diff viewer.
4. Stop/start и resume.
5. Codex run с тем же UX.
6. Перезапуск Jarvis и восстановление history.
7. VM подсвечена на главной.
8. Waiting/done/error дают ровно одно кликабельное уведомление.
9. `avm shell <vm>` открывает workspace.
10. Проверка, что secrets отсутствуют в log/journal/process argv.

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

4. **Headless RunSupervisor**

   Claude/Codex adapters, normalized events, journal/replay, resume, cancel,
   one-follow-up queue, `agent_run.*`.

5. **Project Manager UI**

   active VM rail, project cards, project workspace, provisioning stepper,
   live chat, result/files/diff drawer, environment popover. Обязательная
   проверка живого UI и скриншотами, не только DOM-тестами.

6. **Notifications, autostart и recovery**

   transition dedupe, deep links, TTS routing, pinned sequential start,
   restart recovery и interrupted/resume.

7. **Hardening и rollout**

   real VM smoke Claude+Codex, schema drift, log redaction, performance,
   migration/feature flag, документация пользователя.

## 19. Acceptance criteria v1

- [ ] Из существующего проекта можно одним действием отправить задачу Claude
      или Codex; Jarvis сам доводит среду до запуска.
- [ ] Весь project root доступен VM read-write через штатный `agent-vm`.
- [ ] Исходники `agent-vm` не менялись.
- [ ] Разрешённые user-конфиги Claude/Codex автоматически применяются.
- [ ] Авторизация адресно попадает только в выбранную VM и не появляется в
      argv/log/journal.
- [ ] Claude runtime secret хранится в macOS Keychain, legacy plaintext после
      проверенной миграции очищен.
- [ ] Chat, tools, files, questions и result транслируются в UI.
- [ ] Active/starting/error VM видны на главной и в Project Manager.
- [ ] Есть copy actions для `avm shell` и backend resume.
- [ ] Waiting/done/ready/error уведомления дедуплицированы и открывают нужный
      project/run.
- [ ] UI имеет законченные loading/empty/error/focus/reduced-motion states и
      проверен на запущенном приложении.
- [ ] После рестарта Jarvis история сохранена, незавершённый turn помечен
      `interrupted` и продолжается через Resume.

## 20. Отложенные расширения

- read-only project mount после появления штатного контракта в `agent-vm`;
- clone-mode project runtime;
- per-project allowlist дополнительных secrets/config folders;
- несколько параллельных runs и worktrees;
- guest runner daemon, переживающий quit Jarvis без interruption;
- встроенный terminal view, только если copy/attach окажется недостаточно;
- сторонние runtime providers поверх стабилизированного PluginHost protocol.
