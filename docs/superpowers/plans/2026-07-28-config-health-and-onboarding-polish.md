# Jarvis Config Health and Onboarding Polish — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Проверять `settings.json` Jarvis при запуске, безопасно исправлять его по явному нажатию, устранить ложное отсутствие Claude/Codex и привести весь onboarding к одному читаемому визуальному масштабу.

**Architecture:** Чистый модуль `config_health` валидирует и нормализует JSON без файлового ввода-вывода. `settings::Store` отвечает за чтение, backup `0600`, атомарную запись и сброс кэша. Tauri-команды отдают отчёт onboarding и основной панели. Обнаружение CLI использует единый resolver, а `JARVIS_HEADLESS=1` запрещает smoke-процессам создавать окна. UI остаётся на vanilla JS/CSS.

**Tech Stack:** Rust, Tauri 2, serde_json, vanilla JavaScript/CSS, Node test runner.

---

## Task 1: Чистая валидация и исправление JSON

**Files:**
- Create: `src-tauri/src/config_health.rs`
- Modify: `src-tauri/src/main.rs`
- Modify: `src-tauri/src/settings.rs`

1. Добавить падающие unit-тесты для отсутствующего файла, malformed JSON, корня не-object, неверных типов/enum/range, неизвестных полей и сохранения валидных значений.
2. Запустить:
   `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features config_health::tests`
   Ожидание: тесты сначала не компилируются без нового модуля.
3. Реализовать `ConfigHealth`, `ConfigIssue`, `RepairMode`, `validate_raw` и `repair_raw`.
4. Повторить команду и получить зелёные тесты.

## Task 2: Безопасная файловая операция в Store

**Files:**
- Modify: `src-tauri/src/settings.rs`

1. Добавить тесты: backup байт-в-байт совпадает с исходником, неизвестные поля остаются, повреждённый JSON восстанавливается, итоговый файл и backup имеют `0600`.
2. Добавить `Store::health()` и `Store::repair()`.
3. Перед записью создавать timestamped backup рядом с `settings.json`; писать результат существующим atomic writer; после записи инвалидировать кэш и повторно валидировать.
4. Запустить:
   `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features settings::tests`

## Task 3: Единое обнаружение CLI и честные ошибки

**Files:**
- Modify: `src-tauri/src/install/mod.rs`
- Modify: `src-tauri/src/backend/codex.rs`
- Modify: `src-tauri/src/claude_bin.rs`
- Modify: `src-tauri/src/onboarding.rs`

1. Добавить unit-тесты resolver: находит бинарник в расширенном PATH, игнорирует Jarvis shim, не требует интерактивного shell.
2. Вынести public resolver в `install`, использовать его и readiness, и runtime-бэкендами.
3. В `onboarding_run` сохранять конкретную безопасную ошибку; при panic извлекать `&str`/`String`.
4. Заменить ожидаемые проблемы окружения installer на `Result`, не терять причину `HOME`/PATH.
5. Запустить точечные Rust-тесты onboarding/install.

## Task 4: Headless smoke и startup config recovery

**Files:**
- Modify: `src-tauri/src/main.rs`
- Modify: `src-tauri/src/onboarding.rs`
- Modify: `src-tauri/src/ipc.rs`

1. Добавить тесты pure helper для `JARVIS_HEADLESS`.
2. При `JARVIS_HEADLESS=1` не создавать panel, toast, tray и onboarding.
3. Добавить Tauri-команды `settings_health` и `settings_repair`.
4. Перед выбором стартового окна проверить сырой конфиг; ошибки должны открывать config recovery раньше agent recovery.
5. После успешного repair вернуть путь backup и свежий health report.

## Task 5: UI config recovery и основной баннер

**Files:**
- Modify: `ui/bridge.js`
- Modify: `ui/onboarding-state.js`
- Modify: `ui/onboarding-state.test.mjs`
- Modify: `ui/onboarding.js`
- Modify: `ui/index.html`
- Modify: `ui/renderer.js`

1. Добавить падающие JS-тесты приоритета config recovery и состояний repair/restart.
2. Подключить команды health/repair к bridge.
3. В onboarding показать пути и сообщения проблем без значений, действия «Исправить конфиг» и «Продолжить без исправления».
4. В основной панели показывать постоянный баннер до здоровой ревалидации.
5. Запустить:
   `node --test ui/onboarding-state.test.mjs`

## Task 6: Полный визуальный polish onboarding

**Files:**
- Modify: `ui/onboarding.html`
- Modify: `ui/onboarding.js`
- Modify: `src-tauri/src/windows.rs`

1. Синхронизировать радиус native window effect и CSS shell; оставить одну видимую рамку и обрезать содержимое в углах.
2. Привести welcome, agents, capabilities, recovery и ready к общей ширине, типографике и вертикальному ритму.
3. Увеличить мелкий body/error текст, сократить hero и footer.
4. Заменить текст на «Нужен прокси?».
5. Задать независимые `normal`, `hover`, `focus-visible`, `active`, `disabled`, `busy`; disabled primary не должен наследовать яркий фон с прозрачностью.
6. Добавить DOM/CSS smoke assertions для копирайта и button states.

## Task 7: Полная проверка и запуск

1. Запустить:
   `node --test ui/onboarding-state.test.mjs`
2. Запустить:
   `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features`
3. Запустить проектные lint/format проверки, найденные в `package.json`.
4. Собрать и запустить только через:
   `npm start`
5. Проверить `~/.jarvis-dev/run.sock`, integration log и отсутствие ложного `codex_present=false`/`claude_hooks=false` у основного процесса.
6. Завершить оставшийся smoke/debug-процесс после подтверждения его PID.
7. Открыть onboarding реального release-процесса и проверить скриншотами:
   - углы без двойного контура;
   - читаемые списки и recovery;
   - корректные hover/disabled кнопки;
   - текст «Нужен прокси?»;
   - config recovery и backup flow.
