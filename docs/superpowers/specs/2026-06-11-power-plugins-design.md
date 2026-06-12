# Плагины питания: keep-awake («Не спать») и clamshell («Крышка»)

Дата: 2026-06-11. Статус: реализуется.

## Зачем

Уснувший мак — это замороженные claude-процессы и оборванные API-запросы:
«фоновый завод» встаёт. У демона уже есть точное знание, когда сон вреден —
пока хоть одна сессия `working`. Добавляем два режима как **подключаемые
плагины** (включаются/выключаются, не вкомпилированы в ядро демона):

1. **keep-awake** — вето на idle-сон через power assertion
   (Caffeine/Amphetamine-класс, `IOPMAssertion` через Electron
   `powerSaveBlocker`).
2. **clamshell** — closed-display mode (Amphetamine-класс): крышка закрыта,
   а мак работает. Root-уровень и термо-риски ⇒ **детектим и подсказываем,
   а не молча sudo**.

UX-модель списана с Raycast Coffee (исходники изучены) и Amphetamine
(доки/меню изучены), адаптирована под сценарий «агенты пашут в фоне».

## Решения и отклонённые альтернативы

| Решение | Альтернатива | Почему так |
|---|---|---|
| `powerSaveBlocker` (in-process assertion) | detached `caffeinate` как у Coffee | Блокер умирает вместе с демоном — невозможен «застрявший» запрет сна (fail-safe по построению). Coffee-путь требует `killall caffeinate` — под нож попадают чужие caffeinate. |
| «Пока жив процесс» — свой пульс `kill(pid,0)` каждые 15с | `caffeinate -w <pid>` | Один движок грантов на все триггеры; не плодим внешние процессы. |
| Clamshell через `pmset -a disablesleep` + опциональный sudoers | приватный `kPMSetClamshellSleepState` (путь Amphetamine) | Нативный API требует компилируемый хелпер — вне MVP. `pmset` делает то же на уровне IOPMrootDomain. |
| Плагины = папки `src/plugins/<id>/` со сканом при старте | npm-пакеты с манифестом | «Подключаемость» достигается папкой + тумблером в настройках; пакеты — оверкилл. |

## Архитектура

### Plugin host — `src/plugins/index.js`

Сканирует `src/plugins/*/index.js`. Контракт плагина:

```js
module.exports = {
  id: 'keep-awake',          // ключ настроек plugins.<id>
  name: 'Не спать',
  defaults: { ... },         // дефолты scoped-настроек
  init(ctx) {},              // включение (старт демона или тумблер)
  dispose() {},              // выключение/выход — обязан прибрать за собой
  onSessions(list) {},       // снапшот сессий при каждом изменении
  trayMenu() => [MenuItem template], // секция в right-click меню трея
  badge() => '☕' | '',      // символ в title трея
  status() => { line, ... }, // строка статуса для панели
};
```

`ctx`: `{ settings.get()/set(patch) (scoped по plugins.<id>), sessions(),
notify(title, body, {sessionId?}), ipcHandle(name, fn) (канал
`plugin:<id>:<name>`), updateTray(), log() }`.

Интеграция в `main.js` (точечные правки):
- `whenReady` → `pluginHost.init(ctx)`;
- `push()` → `pluginHost.onSessions(list)`;
- меню трея → базовые пункты + `pluginHost.trayMenus()`;
- `updateTray` → title + `pluginHost.badges()`;
- `before-quit` → `pluginHost.dispose()`;
- IPC `plugins:status` / `plugins:cmd` — generic для панели.

Вкл/выкл плагина: `~/.jarvis/settings.json` → `plugins.<id>.enabled`
(дефолт true). Тумблер в настройках панели вызывает host.setEnabled —
живой `init`/`dispose` без рестарта демона.

### keep-awake — `src/plugins/keep-awake/`

**`engine.js` — чистый движок грантов** (DI: `blocker{start,stop}`, таймеры,
`now()`; Electron не импортирует — тестируется в node):

- Гранты: `kind: auto | timer | process | manual`, `label`, `until?`, `pid?`.
- Инвариант: assertion активна ⇔ грантов > 0 (ровно как в IOPM: пока жив
  хоть один assertion — не спим).
- **auto** — умный триггер: `setWorking(n)`: `n>0` → acquire; `n==0` →
  release с линджером 60с (отмена, если working вернулся). Линджер гасит
  дребезг working→done→working между ходами и держит мост для авто-циклов
  (loop/cron), когда следующий промпт приходит через секунды после done.
- **timer** — `-t`-семантика Coffee: грант с `until`, по истечении release
  + событие `expired`.
- **process** — пульс 15с: `kill(pid, 0)` бросил ⇒ процесс мёртв ⇒ release
  + событие `processDied`.
- **manual** — бессрочно до выключения.
- Слот ручного семейства (manual/timer/process) — **один**: новый старт
  заменяет предыдущий (kill-then-start, как Coffee). `auto` — независимый
  грант (аналог Trigger-сессии Amphetamine: ручная сессия не убивает
  триггер).

**`index.js` — wiring**: `powerSaveBlocker.start(keepDisplayOn ?
'prevent-display-sleep' : 'prevent-app-suspension')`; смена `keepDisplayOn`
на лету перезапускает блокер. Меню трея (Amphetamine-стиль, сжатый):

```
Не спать: выкл | агенты (2) | ещё 47м | пока жив Safari   ← статус, disabled
  Пока агенты работают (авто)            ✓ тумблер
  ───
  Бессрочно
  15 минут / 30 минут / 1 час / 2 часа / 4 часа / 8 часов
  Пока жив процесс… ▸  (GUI-приложения + процессы claude)
  Выключить                               ← если есть ручной грант
  ───
  Не гасить экран                          ✓ тумблер
```

Список процессов: GUI-приложения через `System Events` (как Coffee) +
`pgrep -fl claude` (наш сценарий!), собирается асинхронно при открытии меню.

Уведомления (политика Amphetamine 4+: ручной старт/стоп — молча, иконка
скажет): только таймер истёк / процесс умер.

Настройки: `{ enabled: true, auto: false, keepDisplayOn: false }` —
как у Caffeine/Amphetamine, по умолчанию всё ручное; авто-триггер
«пока агенты работают» — опт-ин тумблер (решение юзера от 2026-06-11).

### clamshell — `src/plugins/clamshell/`

**`core.js` — чистые функции**: `parseClamshell(ioreg)`,
`parseSleepDisabled(pmset -g)`, `parseBattery(pmset -g batt)`,
`decideSuggest(ctx)` (матрица: спал? были working? внешний дисплей? armed?
не чаще раза в час?), `sudoersContent(user)`.

**`index.js` — wiring**:

- **Детект и подсказка** (дефолтный режим, без root): `powerMonitor.suspend`
  → снапшот числа working; `resume` → если работа была прервана сном и
  закрыть-крышку-значит-уснуть (`AppleClamshellCausesSleep = Yes`) →
  уведомление «Сон прервал N работающих сессий — включить closed-display?»
  (клик → панель). Если подключён внешний дисплей — текст про родной
  clamshell-режим (disablesleep не нужен).
- **Arm/disarm**: `pmset -a disablesleep 1|0`. Тихо через
  `sudo -n` при установленном `/etc/sudoers.d/jarvis-pmset`, иначе
  osascript-диалог администратора (явное согласие — каждый раз).
- **autoArm** (только при sudoers): связка с keep-awake — гранты есть →
  `disablesleep 1`, грантов нет → `0`. Мак не уснёт с крышкой посреди
  генерации, но и не будет бодрствовать вечно.
- **Fail-safe** (урок Amphetamine Enhancer):
  1. маркер `~/.jarvis/clamshell.json` — кто и когда поставил `disablesleep`;
  2. на старте демона: `SleepDisabled=1` + маркер наш + причин держать нет →
     восстановить 0 (демон перезапустился после краша);
  3. `before-quit` → восстановить 0;
  4. **батарейный сторож** (пульс 60с, пока armed): на батарее и
     `% ≤ batteryFloor (15)` → disarm + уведомление; тихий sudo недоступен →
     `pmset sleepnow` (форс-сон не требует root и спасает батарею/термо).
- Предупреждение Air: безвентиляторные маки троттлят под закрытой крышкой —
  однократная приписка в подсказке.

Настройки: `{ enabled: true, suggest: true, autoArm: false, batteryFloor: 15 }`.

Меню трея:

```
Крышка: спит как обычно | не спит даже закрытой   ← статус, disabled
  Closed-display mode                  ✓ тумблер (arm/disarm сейчас)
  Авто при работе агентов              ✓ тумблер (нужен sudoers, иначе disabled с подсказкой)
  Подсказывать после прерванного сна   ✓ тумблер
  Настроить тихий режим (sudoers)…     ← если ещё не установлен
```

### UI-поверхности

- **Трей** — главный дом фичи (как у Amphetamine): секции обоих плагинов в
  right-click меню; title `◇ ☕ ⚙2` (`☕` — assertion активна, `⌒` — armed).
- **Панель**: footer добавляет « · ☕ агенты (2)» / « · ☕ ещё 47м»;
  в настройках секция «Плагины» (тумблеры enabled, авто, экран, подсказки).
- **preload**: `getPlugins()`, `pluginCmd(id, cmd, args)`.

### Тесты — `scripts/test.mjs`

- engine: инвариант грантов, замена ручного слота, линджер auto,
  таймер (короткие реальные таймауты), пульс процесса с фейковым kill.
- clamshell core: парсеры на реальных образцах вывода `ioreg`/`pmset`,
  матрица `decideSuggest`, sudoers-контент.

### Проверка на живой системе

1. `pmset -g assertions | grep -i electron` — assertion появляется при
   working-сессии и уходит после линджера.
2. Таймер 1 мин — истёк → уведомление, assertion снята.
3. Краш демона (`kill -9`) при armed → рестарт восстанавливает
   `SleepDisabled 0`.
