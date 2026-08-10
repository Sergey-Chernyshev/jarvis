# Jarvis UI v2: атомарная миграция на React и TypeScript

Дата: 2026-08-07  
Статус: утверждённое направление, полная миграционная спецификация  
Область: весь frontend Jarvis; Rust/Tauri runtime сохраняется

## 1. Резюме решения

Frontend Jarvis полностью переписывается на React 19, strict TypeScript и
Vite. Новый frontend разрабатывается параллельно в `ui-next/`, но не включается
пользователям частями. После достижения функционального и визуального паритета,
прохождения performance-gate и проверки всех Tauri-окон приложение атомарно
переключается с текущего `ui/` на собранный `ui-next/dist/`.

Rust-ядро, daemon, Tauri windows, hooks, voice/STT runtime, session parsing,
plugin host и persistence не переписываются. Меняется их публичная граница с UI:
строковые вызовы и неструктурированные `serde_json::Value` заменяются
типизированными DTO, командами, событиями и общей моделью ошибок.

Основные технологии:

- React 19.x;
- TypeScript со всеми строгими проверками;
- Vite в SPA/MPA-режиме без SSR;
- Zustand для нормализованного состояния runtime;
- Motion for React для системных анимаций;
- Radix Primitives только для сложных интерактивных примитивов;
- CSS Modules и общие design tokens;
- TanStack Virtual для длинных лент;
- `react-markdown`, `remark-gfm` и безопасная схема sanitization для Markdown;
- Vitest, React Testing Library и Playwright WebKit;
- генерируемый из Rust TypeScript-контракт команд и событий.

Полный rewrite означает один пользовательский cutover, а не один огромный
коммит. Внутри `ui-next` работа разбивается на вертикальные срезы, каждый из
которых отдельно собирается, тестируется и сравнивается с legacy UI.

### Browser MVP как Phase 0 design baseline

До подключения Tauri создан полноэкранный browser MVP в `ui-next/`. Он уже
фиксирует целевую информационную архитектуру, typed domain model,
`JarvisClient` boundary, mock state, темы, motion и пользовательские переходы.
Функциональный источник истины:
`docs/superpowers/specs/2026-08-07-react-ui-mvp-functional-inventory.md`.

MVP не является production cutover и не отменяет этапы ниже. Его роль —
сделать дизайн и parity проверяемыми до дорогой интеграции с runtime. При
переносе настройки сохраняют удачную геометрию legacy `ui/settings2.js`, а
остальные экраны используют ту же шкалу плотности и общие controls.

Вспомогательный визуальный артефакт:
`https://www.figma.com/design/i2mzds3ZsBwKatIsfGcmm7`.

## 2. Почему текущую архитектуру нужно заменить

Текущий frontend вырос из небольшого Tauri renderer в набор глобальных
скриптов:

- `ui/renderer.js` — более 4 200 строк;
- `ui/settings2.js` — около 1 900 строк;
- `ui/voice-history.js` — около 1 600 строк;
- `ui/index.html` — около 1 700 строк вместе с глобальными стилями;
- `ui/bridge.js` вручную поддерживает более 80 IPC-вызовов.

Проблема не в самом vanilla JavaScript. Проблема в отсутствии выраженных
архитектурных границ:

- состояние хранится в глобальных переменных;
- экран, бизнес-логика, DOM-операции и IPC смешаны в одних файлах;
- формы ответов Rust-команд не проверяются TypeScript-компилятором;
- новые возможности трудно локализовать в одном компоненте или feature-модуле;
- агенту нужно читать тысячи строк, чтобы восстановить неявные зависимости;
- одни и те же UI-паттерны создаются вручную и постепенно расходятся;
- слушатели событий и обработчики DOM привязаны к жизненному циклу глобального
  скрипта, а не компонента;
- тесты покрывают отдельные функции и текстовые контракты, но не композицию
  экранов и пользовательские сценарии.

Есть и непосредственная причина лагов. Daemon коалесцирует изменения состояния
примерно до одного push за 120 мс, но каждый push вызывает `render()`, который
очищает список сессий и заново создаёт строки, SVG, тексты и обработчики. В
других частях UI также распространена полная очистка контейнера перед
перерисовкой. Это создаёт лишнюю работу DOM, layout и garbage collector.

Замена синтаксиса на JSX сама по себе проблему не решит. Целевая архитектура
должна гарантировать локальные подписки, нормализованное состояние,
инкрементальные события, ленивую загрузку тяжёлых экранов и виртуализацию
длинных коллекций.

## 3. Цели

### 3.1. Типизация

1. TypeScript знает аргументы и ответы каждой доступной UI команды.
2. TypeScript знает payload каждого Tauri event.
3. Rust DTO является источником истины для сериализуемых доменных типов.
4. Команда не может вернуть произвольную несовместимую форму ответа.
5. Ошибки представлены discriminated union, а не строкой неизвестного формата.
6. В production-коде запрещён неявный `any`.

### 3.2. Понятность для кодовых агентов

1. У каждой фичи есть одна директория, публичный API, fixtures и тесты.
2. Компонент можно открыть в браузерном harness без запуска daemon.
3. Прямой доступ к Tauri разрешён только внутри `core/ipc`.
4. Архитектурные ограничения проверяются линтером и CI.
5. Generated-код явно отделён и не редактируется вручную.
6. В `ui-next/AGENTS.md` описаны карта проекта и обычный путь добавления фичи.

### 3.3. Производительность

1. Изменение одной сессии не пересобирает весь список.
2. Длинный чат и история не создают тысячи DOM-узлов одновременно.
3. Неосновные экраны не входят в initial bundle панели.
4. Анимации не запускаются на техническое обновление каждого поля.
5. Основные взаимодействия укладываются в один-два кадра.
6. Производительность измеряется автоматически, а не оценивается субъективно.

### 3.4. Масштабируемость UI

1. Все окна используют одну библиотеку компонентов, типов и токенов.
2. Сложные состояния описываются конечными типизированными вариантами.
3. Feature-модуль можно менять без чтения внутренних деталей соседней фичи.
4. Новый экран добавляется без расширения центрального renderer-файла.
5. Общие interaction patterns не копируются между окнами.

## 4. Не-цели

- Переписывать Rust daemon, session parser, voice/STT или plugin runtime.
- Переходить с Tauri на Electron или создавать нативный SwiftUI frontend.
- Добавлять SSR, Next.js, SvelteKit или серверный frontend.
- Полностью менять визуальную идентичность Jarvis во время миграции.
- Одновременно редизайнить каждый пользовательский сценарий.
- Добавлять сложные SVG-анимации в первой версии UI v2.
- Сохранять две production-реализации frontend после cutover.
- Подключать крупный готовый design system со стилем, отличным от Jarvis.

SVG-анимации являются отдельным последующим слоем. Текущая motion-архитектура
должна позволить добавить их без замены базового API, но не включает их
создание и choreography.

## 5. Границы миграции

### 5.1. Что переписывается

- главное окно панели;
- список активных сессий;
- command palette и actions menu;
- чат сессии;
- вопросы и варианты ответа;
- task board и subagent-индикация;
- карточки ходов;
- Markdown и document/diff viewer;
- проекты и история;
- usage/statistics;
- настройки;
- модели, STT, voice, wake-word и integration surfaces;
- config-health banner и recovery actions;
- voice history;
- onboarding;
- отдельное окно agent chat;
- toast stack и voice HUD.

### 5.2. Что сохраняется

- названия Tauri window labels: `main`, `onboarding`, `agent-chat`, `toast`;
- размеры, native vibrancy, window positioning и focus behavior;
- существующая бизнес-логика команд;
- disk formats и session persistence;
- семантика текущих user-facing сценариев;
- текущие URL entry points: `index.html`, `onboarding.html`,
  `agent-chat.html`, `toast.html`.

Сохранение имён HTML entry points позволяет не менять Rust window navigation
во время cutover. Vite собирает новые entry points с теми же выходными именами.

## 6. Целевая структура

```text
ui-next/
├── index.html
├── onboarding.html
├── agent-chat.html
├── toast.html
├── package.json
├── tsconfig.json
├── vite.config.ts
├── eslint.config.js
├── AGENTS.md
├── src/
│   ├── entries/
│   │   ├── panel/
│   │   │   ├── main.tsx
│   │   │   └── PanelApp.tsx
│   │   ├── onboarding/
│   │   ├── agent-chat/
│   │   └── toast/
│   ├── core/
│   │   ├── ipc/
│   │   │   ├── client.ts
│   │   │   ├── events.ts
│   │   │   ├── mock-client.ts
│   │   │   └── index.ts
│   │   ├── store/
│   │   │   ├── runtime-store.ts
│   │   │   ├── selectors.ts
│   │   │   └── event-reducer.ts
│   │   ├── errors/
│   │   ├── lifecycle/
│   │   └── performance/
│   ├── features/
│   │   ├── sessions/
│   │   ├── chat/
│   │   ├── questions/
│   │   ├── task-board/
│   │   ├── documents/
│   │   ├── projects/
│   │   ├── usage/
│   │   ├── settings/
│   │   ├── models/
│   │   ├── voice/
│   │   ├── voice-history/
│   │   ├── onboarding/
│   │   ├── notifications/
│   │   └── config-health/
│   ├── shared/
│   │   ├── ui/
│   │   ├── motion/
│   │   ├── styles/
│   │   ├── icons/
│   │   ├── hooks/
│   │   └── lib/
│   ├── generated/
│   │   └── bindings.ts
│   └── harness/
│       ├── HarnessApp.tsx
│       ├── scenarios/
│       └── fixtures/
└── tests/
    ├── visual/
    ├── performance/
    └── parity/
```

## 7. Правила зависимостей

Разрешённое направление:

```text
entries -> features -> core/shared
features -> shared
core -> generated/shared
shared -> shared
```

Запрещено:

- `shared` импортирует `features`;
- одна feature импортирует внутренний файл другой feature;
- любой файл вне `core/ipc` импортирует Tauri `invoke` или `listen`;
- feature читает Zustand store целиком без selector;
- component выполняет сериализацию/десериализацию Rust payload;
- generated-код импортирует handwritten feature-код;
- feature определяет глобальный CSS selector для чужого экрана.

Если одной фиче нужен сценарий другой, взаимодействие идёт через её публичный
`index.ts`, типизированную команду или domain event. Импорт вида
`features/chat/internal/parser` запрещён.

ESLint проверяет import boundaries и запрещённые импорты. `tsc` запускается с:

```json
{
  "strict": true,
  "noUncheckedIndexedAccess": true,
  "exactOptionalPropertyTypes": true,
  "useUnknownInCatchVariables": true,
  "noImplicitOverride": true,
  "noFallthroughCasesInSwitch": true
}
```

## 8. Компонентная модель

### 8.1. Shared primitives

В `shared/ui` размещаются только повторно используемые визуальные и
интерактивные примитивы:

- `Button`, `IconButton`;
- `Tabs`, `SegmentedControl`;
- `Switch`, `Checkbox`, `Slider`, `Select`;
- `Dialog`, `Popover`, `Tooltip`;
- `TextField`, `SearchField`, `Composer`;
- `Badge`, `StatusDot`, `Keycap`;
- `Card`, `ListRow`, `Divider`;
- `EmptyState`, `InlineError`, `ErrorPanel`;
- `Progress`, `Spinner`, `Skeleton`;
- `ScrollArea`, `VirtualList`;
- `Toast`, `Banner`;
- `Markdown`, `CodeBlock`, `FileLink`;
- `ErrorBoundary`.

Простые компоненты реализуются непосредственно в Jarvis. Radix используется
только там, где цена самостоятельной реализации высока: focus trap, keyboard
navigation, portal, dismiss behavior и accessibility semantics. Внешний
компонент всегда скрыт за Jarvis wrapper, поэтому feature-код не зависит от
Radix API напрямую.

### 8.2. Feature components

Feature-компонент выражает пользовательский сценарий:

- `SessionList`, `SessionRow`, `SessionActions`;
- `ChatScreen`, `MessageList`, `MessageComposer`, `ToolGroup`;
- `TurnCard`, `TaskBoard`, `QuestionSheet`;
- `DocumentViewer`, `DiffViewer`;
- `ProjectList`, `ProjectHistory`;
- `UsageDashboard`;
- `SettingsScreen` и независимые settings sections;
- `VoiceHistoryScreen`;
- `OnboardingFlow`;
- `ToastStack`, `VoiceHud`.

Feature-компоненты не становятся универсальными “на всякий случай”. Повторное
использование начинается с конкретного второго потребителя.

### 8.3. Размер и ответственность файлов

- одна основная ответственность на component/hook/store module;
- production-файл свыше 300 строк вызывает lint warning и требует осознанного
  исключения;
- функция свыше 80 строк вызывает lint warning;
- большие статические данные и tokens выносятся отдельно;
- barrel exports допускаются только на публичной границе feature/shared;
- generated, fixtures и тесты исключены из ограничения размера.

Ограничение не является целью само по себе. Оно служит ранним сигналом, что
компонент снова начинает смешивать несколько сценариев.

## 9. Навигация и UI state

Главная панель не использует URL router. Набор экранов конечен и описывается
discriminated union:

```ts
type PanelRoute =
  | { name: "sessions" }
  | { name: "chat"; sessionId: SessionId }
  | { name: "projects"; projectId?: ProjectId }
  | { name: "usage"; period: UsagePeriod }
  | { name: "voice-history" }
  | { name: "settings"; section?: SettingsSection };
```

Overlay-состояния отделены от route:

```ts
type PanelOverlay =
  | { name: "none" }
  | { name: "command-palette"; query: string }
  | { name: "question"; sessionId: SessionId }
  | { name: "task-board"; sessionId: SessionId }
  | { name: "document"; sessionId: SessionId; path: string; tab: "doc" | "diff" };
```

Route хранится в локальном app-shell store конкретного окна. Domain state
daemon не содержит информацию о том, какой tab открыт или какой popover
раскрыт.

## 10. Типизированная граница Rust и TypeScript

### 10.1. Принцип

Rust является источником истины для передаваемых через Tauri DTO. Команды не
возвращают UI-facing `serde_json::Value`, если форма ответа известна заранее.

До:

```rust
#[tauri::command]
pub fn state_get(app: AppHandle) -> Value
```

После:

```rust
#[derive(Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct StateSnapshot {
    pub revision: u64,
    pub sessions: Vec<SessionDto>,
}

#[tauri::command]
#[specta::specta]
pub fn state_get(app: AppHandle) -> Result<StateSnapshot, CommandError>
```

TypeScript вызывает сгенерированную обёртку, не строковый `invoke`:

```ts
const snapshot = await commands.stateGet();
```

### 10.2. Генератор

Основной путь — `tauri-specta` v2, зафиксированный точными версиями вместе со
`specta` и TypeScript exporter. Он генерирует типы команд и событий. Интеграция
изолирована в Rust-модуле `ui_contract` и TypeScript-директории `core/ipc`, чтобы
обновление генератора не затрагивало feature-код.

Первая техническая задача миграции обязана доказать:

1. совместимость выбранных точных версий с текущими Tauri и Rust toolchain;
2. корректную обработку `serde(rename_all = "camelCase")`;
3. корректные optional-поля;
4. числовое представление используемых `i64/u64`;
5. generation команд и событий;
6. детерминированный output на CI.

Если текущий project MSRV не поддерживается выбранной версией генератора,
MSRV обновляется отдельным явным commit вместе с CI toolchain. Понижать
типобезопасность ради сохранения декларативного `rust-version` нельзя.

Generated-файл коммитится. CI повторно генерирует его и падает при diff. Так
сломанный Rust/TypeScript контракт виден в том же pull request.

### 10.3. DTO и внутренние модели

UI DTO не обязаны совпадать с внутренними Rust structs. Для больших или
чувствительных моделей вводятся проекции:

- `SessionDto`;
- `TaskBoardDto`;
- `ChatItemDto`;
- `TurnCardDto`;
- `SettingsDto`;
- `ConfigHealthDto`;
- `PluginStatusDto`;
- `UsageSummaryDto`;
- `VoiceStateDto`;
- `InstallProgressDto`;
- `ToastDto`.

DTO:

- содержат только поля, нужные UI;
- используют стабильные machine-readable enum values;
- не содержат secrets;
- избегают неоднозначных `null`/missing semantics;
- имеют документированную единицу для времени и размера;
- имеют отдельный id type alias на TypeScript-стороне.

Динамические plugin payload являются ограниченным исключением. Они
представляются как безопасный `JsonValue` только внутри plugin adapter.
Встроенные плагины получают поверх него typed wrapper, и feature-код не работает
с произвольным JSON.

### 10.4. Ошибки

```rust
#[derive(Serialize, specta::Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CommandError {
    NotFound { resource: String },
    InvalidInput { field: String, message: String },
    Conflict { resource: String, message: String },
    Unavailable { capability: String, message: String, retryable: bool },
    PermissionDenied { capability: String, message: String },
    Io { operation: String, message: String, retryable: bool },
    Internal { message: String, incident_id: String },
}
```

UI обязан исчерпывающе обработать варианты. `Internal` не раскрывает secrets,
пути auth store и raw debug payload. Неожиданная ошибка нормализуется в
`UnknownUiError` внутри `core/errors`, а не распространяется как `any`.

## 11. События и reactive store

### 11.1. Snapshot плюс patch

При инициализации окно получает полный snapshot:

```text
state_get -> StateSnapshot { revision, sessions }
```

Последующие изменения доставляются patch-событием:

```text
sessions:patch
  revision: u64
  upserted: SessionDto[]
  removedIds: SessionId[]
```

Daemon продолжает коалесцировать быстрые изменения. Перед emit он сравнивает
новый UI snapshot с последним опубликованным snapshot и отправляет только
изменившиеся или удалённые сущности.

Если frontend видит пропуск revision, он запрашивает полный snapshot. Это
устраняет зависимость correctness от гарантированной доставки каждого event.

### 11.2. Нормализованное состояние

```ts
type RuntimeState = {
  revision: number;
  sessionsById: Record<SessionId, SessionDto>;
  sessionOrder: SessionId[];
  pluginsById: Record<PluginId, PluginStatusDto>;
  limit: LimitStateDto | null;
  configHealth: ConfigHealthDto | null;
};
```

`SessionRow` подписывается на `sessionsById[id]`. Изменение одной сессии не
вызывает React commit всех остальных строк. Сортировка пересчитывается только
когда меняется поле, реально участвующее в порядке.

Store не содержит:

- значение каждого input;
- hover/pressed state;
- открытость локального accordion;
- temporary form errors;
- незавершённую animation state.

Это локальное component state.

### 11.3. Жизненный цикл событий

Каждая entry point создаёт один `IpcClient` и один набор window-scoped
subscriptions. `listen` возвращает typed unsubscribe, который вызывается при
unmount/HMR. Feature-компоненты не создают дублирующие глобальные слушатели.

События делятся на:

- domain state: sessions, plugins, limits, config health;
- streaming: chat append, summary, install progress, voice/audio phase;
- window commands: panel shown, open session, goto settings;
- notifications: toast add/update/remove/hold/extend.

Имена и payload описаны в generated bindings.

## 12. Данные чата

Чат является отдельным store на активную сессию:

```ts
type ChatState = {
  sessionId: SessionId;
  status: "idle" | "opening" | "ready" | "failed";
  itemIds: ChatItemId[];
  itemsById: Record<ChatItemId, ChatItemDto>;
  turnsById: Record<TurnId, TurnDto>;
  pendingReplies: PendingReply[];
  hasOlder: boolean;
};
```

Изменения:

- первоначально загружается текущий хвост;
- новые items приходят append-событиями;
- item имеет стабильный id, а не определяется индексом массива;
- optimistic user reply имеет client id и заменяется backend echo;
- tool items группируются selector-ом, а не глобальной переменной renderer;
- scroll anchoring оформлен отдельным hook;
- старые сообщения могут подгружаться страницами;
- DOM виртуализируется после установленного порога;
- Markdown и diff chunks загружаются лениво.

Для первой версии сохраняется текущий observable contract: открытие чата
показывает хвост транскрипта и продолжает live append. Pagination старых
сообщений может быть включена после cutover, но модель `hasOlder` закладывается
сразу.

## 13. Markdown и документы

Самописный глобальный Markdown renderer заменяется компонентом с явной схемой:

- `react-markdown`;
- `remark-gfm` для таблиц и списков;
- sanitization без raw HTML;
- отдельные renderers для ссылок, code, table и file references;
- внешняя ссылка вызывает typed `urlOpen`;
- локальная file link вызывает typed `fileOpen`;
- Markdown не использует `dangerouslySetInnerHTML`.

Document viewer:

- читает документ через typed command;
- показывает loading/error/empty состояния;
- diff загружает только при выборе tab;
- тяжёлый diff renderer находится в lazy chunk;
- содержимое документа можно выделять и копировать;
- file access errors остаются внутри viewer и не ломают panel app.

## 14. Design system

### 14.1. Tokens

```text
color      background, surface, overlay, border, text, status, accent
spacing    2, 4, 6, 8, 12, 16, 20, 24, 32
radius     4, 6, 8, 12, 16, pill
type       caption, body, bodyStrong, title, display, mono
shadow     floating, popover, focus
z-index    base, sticky, popover, modal, toast
motion     duration, easing, spring, distance
```

Tokens определяются CSS custom properties и typed TypeScript constants там,
где нужны Motion. Feature не добавляет новый случайный цвет, radius или
duration, если существующий token подходит.

### 14.2. Styling

- global CSS содержит reset, fonts, root geometry, theme tokens и accessibility;
- component styles находятся рядом в CSS Module;
- inline style допускается для действительно динамических значений и Motion;
- utility-class framework не используется;
- native vibrancy и прозрачность остаются ответственностью Tauri window;
- WebView surface не имитирует второй тяжёлый blur поверх всей площади;
- focus-visible состояния обязательны даже при mouse-first UX.

### 14.3. Иконки

Базовые иконки берутся из одного Lucide React entry. Custom Jarvis icons живут
в `shared/icons` как typed components. Inline SVG path literals не копируются
между features.

## 15. Motion system

Motion for React скрывается за `shared/motion`, а не используется с
произвольными значениями во всех features.

### 15.1. Токены

```text
instant   70-90 ms
fast      90-120 ms
normal    160-200 ms
screen    200-240 ms
spring    snappy, low-overshoot
distance  2, 4, 8, 12 px
```

Точные значения калибруются один раз на реальном WKWebView и фиксируются
tokens. Production component не задаёт случайный `duration: 0.37`.

### 15.2. Сценарии

- panel enter/exit;
- screen crossfade/translate;
- shared active tab indicator;
- popover/dialog/palette presence;
- new/remove/reorder session rows;
- new chat messages и tool chips;
- accordion/card expansion;
- loading/success/error transitions;
- status dot и progress state.

### 15.3. Performance policy

- по умолчанию анимируются `transform` и `opacity`;
- изменение layout анимируется через FLIP/layout transform;
- большие `filter`, `backdrop-filter`, shadow и blur не анимируются;
- stream update не запускает entrance animation повторно;
- статус анимируется только при semantic state transition;
- list layout animation не работает на каждом 120 ms patch;
- `will-change` добавляется точечно и снимается после animation;
- Motion загружается через `LazyMotion`;
- root использует `MotionConfig reducedMotion="user"`;
- при Reduced Motion spatial movement заменяется быстрым opacity transition.

SVG-анимации позднее подключаются через `shared/motion/svg`, используют те же
tokens и reduced-motion policy.

## 16. Multi-window build

Vite работает как multi-page application:

```text
index.html       -> entries/panel/main.tsx
onboarding.html  -> entries/onboarding/main.tsx
agent-chat.html  -> entries/agent-chat/main.tsx
toast.html       -> entries/toast/main.tsx
```

Каждая entry point:

- создаёт только нужный app root;
- подключает общие tokens;
- создаёт window-scoped IPC client;
- имеет собственный ErrorBoundary;
- получает только разрешённые для этого окна events;
- не тащит panel-only features в bundle toast.

`toast` должен оставаться минимальным bundle, потому что создаётся на старте и
обслуживает latency-sensitive уведомления. `settings`, Markdown, diff,
statistics и voice-history являются lazy chunks панели.

## 17. Browser harness для разработки и агентов

`src/harness` запускается обычным Vite в браузере и не требует Rust/Tauri.
`MockIpcClient` реализует тот же interface, что production client.

Обязательные scenarios:

- пустая панель;
- несколько сессий во всех статусах;
- частые session patches;
- длинные project names и summaries;
- чат с user/assistant/tool/card/question items;
- чат на 500 и 5 000 элементов;
- task board и subagents;
- document и diff;
- все settings states;
- installation progress/error/success;
- onboarding happy/recovery paths;
- toast stack и voice HUD;
- config health warning/error/repaired;
- reduced motion;
- узкое agent-chat window.

Каждая feature хранит realistic fixtures рядом с тестами. Fixtures используют
generated DTO types, поэтому Rust contract change ломает устаревший mock на
компиляции.

Harness решает три задачи:

1. агент переносит прототип в конкретный компонент и сразу видит результат;
2. Playwright делает visual regression без нестабильного daemon state;
3. performance-сценарии воспроизводятся детерминированно.

Harness не заменяет smoke в настоящем Tauri WKWebView.

## 18. Performance budgets

Перед реализацией на legacy UI записывается baseline на одном и том же Mac,
профиле и наборе fixtures. После этого UI v2 должен пройти абсолютные бюджеты:

| Сценарий | Бюджет |
|---|---:|
| Уже созданное скрытое окно: `panel-shown` → interactive | p95 ≤ 100 ms |
| Session patch → следующий paint | p95 ≤ 32 ms |
| React commit для одиночного session patch | p95 ≤ 8 ms |
| Command palette: keydown → paint | p95 ≤ 50 ms |
| Scroll чата на 500/5 000 fixtures | ≥ 55 FPS на 60 Hz |
| Long tasks в основном panel scenario | 0 задач > 50 ms после warm-up |
| Изменение одной сессии | commit только затронутых rows |
| Initial JS панели без lazy chunks, gzip | ≤ 180 KiB |
| Toast initial JS, gzip | ≤ 60 KiB |

Дополнительно UI v2 не должен быть хуже legacy baseline ни по одному
пользовательскому сценарию. Если абсолютный бюджет не проходит из-за
измеренной особенности WKWebView, изменение бюджета оформляется в spec с
профилем и trace, а не делается молча.

Instrumentation:

- `performance.mark/measure` вокруг IPC receive, store apply и next paint;
- `PerformanceObserver` для long tasks;
- React Profiler callback в performance harness;
- Playwright trace и WebKit screenshots;
- Vite bundle report и CI size gate.

Production diagnostics не логируют содержимое сообщений и secrets.

## 19. Error handling

Каждая entry point имеет root ErrorBoundary с возможностью:

- показать понятную ошибку;
- скопировать incident id;
- перезагрузить текущее WebView;
- открыть recovery/settings, если ошибка конфигурационная.

Feature boundaries изолируют тяжёлые поверхности: Markdown, diff, statistics,
voice history и settings section.

Async UI использует явное состояние:

```ts
type AsyncState<T> =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ready"; data: T }
  | { status: "error"; error: UiError };
```

Loading не представляется пустым массивом, error не представляется `null`.
Кнопка с async action блокирует повторную отправку и отображает локальное
состояние. Retry доступен только для ошибок с `retryable: true`.

## 20. Accessibility и keyboard UX

Сохраняется keyboard-first характер Jarvis:

- tab order и focus-visible;
- Escape закрывает верхний overlay;
- command palette и lists поддерживают arrows/Enter;
- focus возвращается инициатору после dialog/popover;
- screen transition не теряет ожидаемый initial focus;
- status не кодируется только цветом;
- live-region используется только для важных завершённых событий;
- Reduced Motion соблюдается глобально;
- text selection включается в сообщениях, документах и коде;
- hotkeys не перехватывают нативное редактирование input/contenteditable.

Radix wrappers покрываются keyboard tests.

## 21. Тестовая стратегия

### 21.1. Rust contracts

- сериализация каждого UI DTO;
- export bindings;
- command error variants;
- session patch diff;
- revision monotonicity;
- snapshot resync;
- отсутствие secrets в UI DTO;
- backward-compatible disk models не зависят от UI DTO.

### 21.2. TypeScript unit tests

- reducers и selectors;
- sorting и filtering;
- route/overlay transitions;
- optimistic reply reconciliation;
- tool grouping;
- error normalization;
- motion policy;
- Markdown link routing;
- settings adapters.

### 21.3. Component tests

- interaction через доступные роли, а не CSS selectors;
- loading/empty/error/success для каждой async surface;
- keyboard navigation;
- focus restoration;
- event subscription cleanup;
- отсутствие лишних commits в критичных components.

### 21.4. Visual regression

Playwright WebKit открывает browser harness и снимает:

- каждую основную страницу;
- overlays;
- длинный/пустой/ошибочный контент;
- светлые системные эффекты под тёмным vibrancy background;
- reduced motion final states;
- разные размеры agent-chat.

Анимация в screenshot tests переводится в deterministic test mode.

### 21.5. Tauri smoke

На реальной dev-сборке проверяются:

- создание всех четырёх окон;
- native vibrancy/corners/shadow;
- show/hide/focus;
- реальные команды и события;
- clipboard/open URL/open file;
- global shortcuts и text editing;
- notification resize/click;
- onboarding recovery;
- chat live append;
- voice/STT progress;
- update/relaunch actions.

Browser harness не подтверждает корректность native window behavior.

## 22. Functional parity matrix

Каждый пункт должен иметь automated test или записанный live-smoke result.

### 22.1. Main panel

- [ ] show/hide и entering motion;
- [ ] список и фильтрация сессий;
- [ ] selection мышью и keyboard;
- [ ] pin/unpin;
- [ ] status/model/branch/host/usage;
- [ ] config-health banner и repair;
- [ ] provider limit banner;
- [ ] command palette;
- [ ] actions menu;
- [ ] focus terminal и fallback в chat;
- [ ] launch/resume session;
- [ ] tabs и footer actions.

### 22.2. Chat

- [ ] open/close tail;
- [ ] user/assistant/tool items;
- [ ] Markdown, tables, code и links;
- [ ] pasted images;
- [ ] optimistic reply и queue status;
- [ ] model/effort commands;
- [ ] slash/custom command catalog;
- [ ] task board/subagents;
- [ ] question picker и custom answer;
- [ ] turn summary/analysis cards;
- [ ] raw feed toggle;
- [ ] file chips;
- [ ] document viewer и diff;
- [ ] terminal focus/resume fallback;
- [ ] scroll anchoring и virtual list.

### 22.3. Projects and usage

- [ ] project grouping;
- [ ] history;
- [ ] new Claude/Codex session;
- [ ] resume existing session;
- [ ] usage periods;
- [ ] series/chart;
- [ ] totals, billing и limit/reset presentation.

### 22.4. Settings

- [ ] notifications;
- [ ] panel position;
- [ ] startup and diagnostics;
- [ ] all configurable hotkeys;
- [ ] hotkey recording, conflict и steal;
- [ ] integration status/removal;
- [ ] Claude/Codex service backend, model, effort и proxy;
- [ ] auth connect/disconnect;
- [ ] model install/delete/progress;
- [ ] STT engine, device, gate, test и install;
- [ ] voice speaker/rate/mute/duck/Bluetooth;
- [ ] wake enable/threshold/model/install;
- [ ] keep-awake/clamshell plugin surfaces;
- [ ] prompts/smart mode;
- [ ] application metadata/update/relaunch;
- [ ] visible and recoverable errors.

### 22.5. Other windows

- [ ] onboarding normal flow;
- [ ] onboarding config recovery;
- [ ] installer progress/failure;
- [ ] agent-chat send/events/confirmation;
- [ ] toast add/update/remove;
- [ ] toast hold/extend/TTL;
- [ ] sticky question toast;
- [ ] voice HUD phases;
- [ ] dynamic toast window resize;
- [ ] non-focus-stealing toast behavior.

## 23. Миграционные этапы

### Этап 0. Freeze и baseline

1. Зафиксировать legacy feature inventory и screenshots.
2. Записать performance baseline.
3. Ввести frontend feature freeze для `ui/`.
4. Срочные legacy fixes регистрировать в parity matrix и переносить в v2.
5. Создать `ui-next` без изменения production `frontendDist`.
6. Провести browser design pass на mock-клиенте и записать расхождения.

Выход: воспроизводимый baseline, полный parity checklist, legacy продолжает
работать, browser MVP доступен отдельно от production runtime.

### Этап 1. Toolchain и contracts

1. React/TypeScript/Vite multi-entry build.
2. Strict TS и ESLint boundaries.
3. `tauri-specta` spike и pinned integration.
4. Generated bindings drift check.
5. Production и mock IPC clients.
6. Browser harness.
7. CI scripts.

Выход: typed command может быть вызвана из harness и Tauri dev entry; event
подписка корректно очищается.

### Этап 2. Design system и app shells

1. Tokens и root geometry.
2. Shared primitives.
3. MotionConfig и motion tokens.
4. Four entry shells.
5. Error boundaries.
6. Visual fixtures.

Выход: все окна открываются с правильной геометрией и базовыми компонентами,
но ещё без полной бизнес-функциональности.

### Этап 3. Runtime state

1. Typed `StateSnapshot`.
2. `sessions:patch` и revision.
3. Normalized Zustand store.
4. Selectors и ordering.
5. Limit/plugins/config-health domains.
6. Performance instrumentation.

Выход: session patch обновляет только затронутую строку и проходит budget.

### Этап 4. Main panel

1. Session list.
2. Filter/search.
3. Command palette.
4. Actions/footer/tabs.
5. Config and limit banners.
6. Terminal/launch navigation.

Выход: основной daily path функционально эквивалентен legacy.

### Этап 5. Chat vertical

1. Chat store и tail lifecycle.
2. Virtual message list.
3. Composer/images/optimistic reply.
4. Markdown и tool groups.
5. Questions.
6. Task board/subagents.
7. Turn cards.
8. Document/diff viewer.
9. Command/model/effort controls.

Выход: полный chat parity и performance на long fixtures.

### Этап 6. Projects, usage и voice history

1. Projects/history.
2. Session launch/resume.
3. Usage dashboard.
4. Voice history.

Выход: все secondary tabs main panel перенесены.

### Этап 7. Settings

Settings переносится секциями, но доступен пользователю только при общем
cutover:

1. general/notifications/panel;
2. hotkeys;
3. integration/auth/service;
4. models/install;
5. STT;
6. voice/wake;
7. plugins/prompts/about.

Выход: settings parity matrix закрыта, каждый раздел имеет fixtures и tests.

### Этап 8. Onboarding, agent chat и toast

1. Onboarding state machine.
2. Config recovery.
3. Agent chat.
4. Toast stack.
5. Voice HUD.
6. Window-specific bundle budgets.

Выход: все Tauri entry points полностью заменены.

### Этап 9. Hardening

1. Полный Rust и frontend test suite.
2. Visual regression.
3. Performance gates.
4. Accessibility/keyboard pass.
5. Real Tauri smoke.
6. Bundle/security inspection.
7. Release candidate на отдельном dev profile.

Выход: подписанный migration acceptance report без открытых blocker defects.

### Этап 10. Atomic cutover

1. Обновить `frontendDist` на `../ui-next/dist`.
2. Подключить Vite `beforeDevCommand` и `beforeBuildCommand`.
3. Убедиться, что Rust window URLs не изменились.
4. Собрать release artifact.
5. Выполнить runtime smoke именно на release artifact.
6. Удалить legacy `ui/` в cutover commit либо переименовать `ui-next` в
   канонический `ui` до merge.
7. Выпустить версию только после smoke.

После cutover в `main` не остаются две изменяемые реализации UI.

## 24. Rollback

Rollback является release-level, а не runtime feature flag:

1. cutover выполняется отдельным commit;
2. предыдущий release artifact и tag сохраняются;
3. при blocker regression публикуется предыдущий artifact или revert cutover;
4. пользовательские settings/state formats не меняются frontend rewrite,
   поэтому downgrade не требует data migration;
5. новые IPC commands добавляются до удаления legacy commands;
6. legacy IPC удаляется отдельным cleanup только после стабильного UI v2
   release.

Не используется скрытый production toggle между двумя frontend bundles: он
удваивает поверхность тестирования и позволяет legacy жить бесконечно.

## 25. CI и команды

Целевой единый frontend gate:

```text
npm run check:ui
  -> typecheck
  -> generated bindings drift
  -> eslint
  -> format check
  -> unit/component tests
  -> visual smoke
  -> performance budgets
  -> bundle budgets
```

Дополнительные команды:

```text
npm run dev:ui          # browser harness
npm run dev:tauri-next  # Tauri + Vite next UI
npm run test:ui
npm run test:visual
npm run test:perf
npm run build:ui
npm run bindings
```

Root `npm test` после cutover включает frontend gate либо явно вызывает его
перед Rust suite. CI использует зафиксированную Node и Rust toolchain.

## 26. Документация для дальнейшей разработки

До cutover обязательны:

- `ui-next/AGENTS.md` — карта слоёв, правила импортов, where-to-change guide;
- `ui-next/README.md` — запуск harness/Tauri, tests и build;
- `docs/architecture/ui.md` — runtime/data-flow;
- generated contract comments;
- component catalog в harness;
- рецепт добавления command/event;
- рецепт добавления feature;
- performance debugging guide;
- migration parity report.

`AGENTS.md` должен отвечать агенту минимум на вопросы:

1. где находится нужная feature;
2. как открыть её fixture;
3. где взять DTO;
4. как вызвать Rust;
5. где хранить local/global state;
6. какой shared component использовать;
7. какие tests и команды запустить;
8. какие импорты запрещены.

## 27. Риски и меры

### Drift legacy и v2

Мера: feature freeze, parity matrix и обязательное зеркалирование urgent fixes.

### Новый монолит `App.tsx`

Мера: import boundaries, file-size warnings, vertical slices и review checklist.

### Типы есть, но runtime contract остаётся динамическим

Мера: Rust DTO, generated commands/events и запрет UI-facing `Value`.

### React re-render всего дерева

Мера: normalized store, narrow selectors, stable ids, profiler tests и React
Compiler после отдельной проверки совместимости.

React Compiler не является условием correctness или единственным средством
performance. Архитектура должна проходить budgets и без надежды на
автоматическую memoization.

### Motion создаёт новые лаги

Мера: semantic animation policy, transform/opacity, LazyMotion, reduced motion
и trace на WKWebView.

### Tauri Specta остаётся prerelease

Мера: exact pin, generated adapter boundary, drift test и isolated upgrade.

### Browser harness отличается от WKWebView

Мера: Playwright WebKit плюс обязательный live Tauri smoke.

### Settings слишком велики для одного переноса

Мера: независимые settings sections с общими typed adapters и fixtures, но
единый production cutover.

### Длинный rewrite блокирует продукт

Мера: вертикальные mergeable slices внутри `ui-next`; они не активны в
production, но регулярно проходят CI и не живут в одной долгой ветке.

## 28. Критерии готовности к cutover

Cutover разрешён только когда:

1. закрыта вся functional parity matrix;
2. нет прямых `invoke/listen` вне `core/ipc`;
3. нет необоснованных UI-facing `serde_json::Value`;
4. generated bindings актуальны;
5. strict TypeScript проходит без suppressions уровня проекта;
6. все четыре entry bundles собираются;
7. visual regression принят;
8. performance и bundle budgets зелёные;
9. keyboard/reduced-motion проверки пройдены;
10. release artifact проверен в реальном Tauri runtime;
11. rollback release/tag доступен;
12. migration acceptance report не содержит blocker/high issues;
13. документация для агентов и разработчиков написана;
14. legacy frontend не остаётся второй активной реализацией после merge.

## 29. Definition of Done

Миграция завершена, когда пользовательская версия Jarvis использует React/UI v2
во всех окнах, TypeScript получает контракты из Rust, session updates
инкрементальны, длинные коллекции виртуализированы, motion работает по общей
политике, performance budgets проходят, а добавление новой фичи не требует
изменения глобального renderer.

Успех оценивается не количеством переписанных строк, а тем, что:

- лаги измеримо исчезли;
- несовместимый IPC-код не компилируется;
- агент может открыть fixture, найти feature и внести локальное изменение;
- UI состоит из понятных компонентов;
- один domain event обновляет только своих потребителей;
- новый код не восстанавливает старую глобальную архитектуру.

## 30. Справочные материалы

- Tauri frontend configuration:
  <https://v2.tauri.app/start/frontend/>
- Tauri Vite configuration:
  <https://v2.tauri.app/start/frontend/vite/>
- React and TypeScript:
  <https://react.dev/learn/typescript>
- React Compiler:
  <https://react.dev/learn/react-compiler/introduction>
- Tauri Specta:
  <https://github.com/specta-rs/tauri-specta>
- Motion for React:
  <https://motion.dev/docs/react>
- Motion performance:
  <https://motion.dev/docs/performance>
- Motion accessibility:
  <https://motion.dev/docs/react-accessibility>
