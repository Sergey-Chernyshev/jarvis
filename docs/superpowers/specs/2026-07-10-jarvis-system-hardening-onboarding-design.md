# Jarvis system hardening and readiness onboarding

Дата: 2026-07-10. Статус: утверждено запросом на автономную реализацию.

## 1. Проблема

Jarvis уже покрывает большой набор сценариев, но первый опыт и несколько runtime-контрактов расходятся с реальным состоянием системы:

- обычный чат содержит устаревший `Диалог / Саммари` placeholder, а `master` дополнительно включает неверно понятое turn-summary представление;
- onboarding смешивает обязательную интеграцию с многогигабайтными optional-моделями, теряет события между окнами и показывает успех без проверки core readiness;
- Claude/Codex hooks, tmux transport и service sidecars имеют несколько подтверждённых race/security/drift рисков;
- тесты хорошо покрывают чистую Rust-логику, но почти не проверяют webview state machine и живой installed runtime.

## 2. Цели

1. Вернуть session chat к одному источнику правды: живой transcript + composer, без summary mode.
2. Сделать onboarding красивым, компактным и основанным на проверяемой готовности.
3. Отделить core integration от optional capabilities и исключить скрытые загрузки.
4. Устранить подтверждённые дефекты hooks/tmux/runtime, не меняя границу продукта «monitor and remote, not orchestrator».
5. Добавить UI state tests, feature-complete Rust checks и живой smoke активного dev-профиля.
6. Сохранить пользовательские незакоммиченные STT-правки и внешний diff `bin/jarvis-hook`.

## 3. Не-цели

- Jarvis не получает планировщик/оркестратор агентов.
- В session chat не создаётся новый полноразмерный summary/phase view.
- Не устанавливаются модели без явного выбора.
- Не автоматизируется доверие сторонним Codex hooks через глобальный bypass.
- Не обещается полная поддержка Intel/Windows/Linux в этом цикле.

## 4. Целевая архитектура

### 4.1 Session chat

`ui/index.html` содержит один `.chatlog`. Header показывает проект, agent/model, channel, tasks/questions, usage и status. Legacy `chatModeSeg`, `chatSummary` и `setChatMode()` удаляются. Merge `master` сопровождается откатом `1a0b2c7`, чтобы turn-summary implementation не вернулась скрыто через веточный drift.

Короткий `Session.summary` остаётся headline в списке сессий и основой done-toast. Это отдельный продуктовый контракт, не альтернативный режим чата.

### 4.2 Readiness model

Backend возвращает один сериализуемый snapshot:

```text
ReadinessSnapshot
├── coreReady
├── agents[]        Claude / Codex: detected, hooks, trusted, shim
├── transport       hook binary, tmux, config, PATH, socket
├── capabilities[]  TTS / Whisper / Qwen / wake / service LLM
├── job             idle | running | done | failed + steps
└── warnings[]      actionable, user-facing
```

Core считается готовым только когда для доступного агента установлены корректные hooks, hook binary и shim; tmux/PATH показываются отдельными обязательными transport checks. Optional capability никогда не блокирует вход в панель.

### 4.3 Installer boundary

`install::install_core()` выполняет только локальные быстрые операции: hook binary/MCP, Claude/Codex hook registration, shim/tmux config/PATH и media adapter. Существующий CLI `install()` может вызывать core и legacy optional setup для обратной совместимости, но onboarding вызывает только core.

Модели устанавливаются через явный plan. В процессе может существовать только один install job. Его snapshot хранится в Rust, поэтому повторно открытый webview восстанавливает прогресс и не запускает дубль.

События install job транслируются и в `main`, и в `onboarding`; событие является сигналом обновить snapshot, а не единственным хранилищем состояния.

### 4.4 Onboarding FSM

```text
checking -> welcome -> agents -> capabilities -> verify -> ready
                    \-> repairing -> agents
capabilities -> installing -> capabilities
any state -> degraded / failed -> retry
```

Экран фиксирован в пределах нативного окна, но центральная область скроллится, footer остаётся sticky. Все статусы имеют текст и иконку, `aria-live`, видимый focus и reduced-motion fallback.

### 4.5 Визуальная система

- Палитра: Obsidian `#0B0D10`, Graphite `#151920`, Frost `#E8ECF2`, Signal blue `#79A7FF`, Ready mint `#55D6A4`, Attention amber `#F2B66D`.
- Типографика: SF Pro Display для короткого hero, SF Pro Text для интерфейса, SF Mono только для shortcuts/status facts.
- Signature: «signal rail» — тонкая вертикальная линия readiness, которая заполняется по мере готовности реальных подсистем. Это не декоративный progress bar, а карта этапов.
- Один спокойный ambient highlight за логотипом; вся остальная поверхность строгая и функциональная.

### 4.6 Hooks and transport hardening

- Claude notifications учитывают `notification_type`; `auth_success` не переводит сессию в Waiting.
- Claude SubagentStart/SubagentStop подключаются к уже существующему reducer path.
- Codex `last_assistant_message` из Stop используется раньше потенциально не успевшего flush rollout.
- Codex hook trust bypass не добавляется автоматически в каждый запуск. Readiness показывает, что доверие требует подтверждения.
- tmux использует уникальный buffer на отправку и `paste-buffer -d`, исключая cross-session race.
- proxy/env передаётся через `tmux update-environment`, а не в shell-command/argv долгоживущего клиента.
- terminal focus всегда адресует отдельный server `-L jarvis`.

## 5. Error handling

- Core install возвращает фактический post-install snapshot. UI пишет «Готово» только при `coreReady=true`.
- Ошибка одной optional-модели не прерывает остальные, но остаётся в snapshot до retry.
- Повторный start при активной job возвращает существующую job, не создаёт thread.
- Закрытие окна не отменяет job; повторное открытие восстанавливает её состояние.
- Hook остаётся fail-silent и всегда возвращает нейтральный JSON согласно текущему Claude/Codex контракту.

## 6. Проверка

- Node `node:test` для onboarding reducer/FSM и UI contract checks.
- Rust unit tests для core plan, job singleton, actionable notifications, subagent event list, unique tmux buffers и no-secret command construction.
- `cargo test` и `cargo clippy` с release features `wakeword-ort,whisper-native,stt-vad`.
- Playwright preview 480×600: welcome, partial, installing, error, ready; keyboard and reduced motion.
- Изолированный HOME/JARVIS_DIR smoke для installer/hook envelope.
- Реальный dev-profile rebuild, reinstall, start, socket/status/log checks.

## 7. Бизнесовые критерии успеха

- Time-to-core-ready не включает загрузку моделей.
- Пользователь всегда понимает, что обязательно, что optional и почему нужен доступ.
- Ни proxy credentials, ни tokens не появляются в UI, logs или process argv.
- Любой красный readiness status содержит одно конкретное следующее действие.
- Коммерчески несовместимые модели явно помечены до загрузки.

