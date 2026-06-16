# macOS-онбординг Jarvis — Фаза 2 (умный первый запуск) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans. Steps use checkbox (`- [ ]`).

**Goal:** При первом запуске Jarvis.app красиво предлагает доустановить интеграцию с Claude Code и голос Silero — окно с живым прогрессом по шагам. Та же логика доступна из CLI и из пункта меню «Переустановить интеграцию».

**Architecture:** Логика установки выносится из `bin/setup.rs` в самодостаточный `src/install/mod.rs` (только std/serde_json/regex/chrono). `install()` принимает колбэк прогресса и шлёт шаги. Приложение зовёт её в фоне, стримит шаги событием `onboarding:progress` в отдельное стеклянное окно `onboarding.html`. CLI-бинарь `setup` становится тонкой обёрткой, печатающей те же шаги.

**Tech Stack:** Rust (Tauri 2 commands/events/windows), ванильный JS+CSS (дизайн-токены панели Jarvis).

---

## Структура файлов

- Create: `src-tauri/src/install/mod.rs` — `status()`, `install(progress)`, `uninstall(progress)`, типы `Status`/`Step`/`StepState`. Сюда переезжают хелперы и `include_str!`-ассеты из `setup.rs`.
- Modify: `src-tauri/src/bin/setup.rs` — тонкая CLI-обёртка: `#[path="../install/mod.rs"] mod install;`, печать шагов.
- Modify: `src-tauri/src/main.rs` — `mod install;`, регистрация команд онбординга, первый запуск открывает окно.
- Create: `src-tauri/src/onboarding.rs` — Tauri-команды `onboarding_status`, `onboarding_run`; запуск install в потоке + emit шагов.
- Modify: `src-tauri/src/windows.rs` — `create_onboarding()`.
- Modify: `src-tauri/src/tray.rs` — пункт «Переустановить интеграцию».
- Create: `ui/onboarding.html` — стеклянное окно (токены панели), hero+иконка, кнопка, живой список шагов.
- Create: `ui/onboarding.js` — invoke `onboarding_run`, listen `onboarding:progress`, рендер шагов.

---

### Task 1: Модуль install — типы и status()

**Files:** Create `src-tauri/src/install/mod.rs`; Modify `src-tauri/src/main.rs`.

Интерфейс (самодостаточный, без зависимостей от модулей приложения):
```rust
//! Установка интеграции Jarvis ⇄ Claude Code: общая логика для CLI и приложения.
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize)]
pub enum StepState { Start, Done, Warn }

#[derive(Debug, Clone, Serialize)]
pub struct Step { pub phase: String, pub state: StepState, pub msg: String }

#[derive(Debug, Clone, Serialize, Default)]
pub struct Status {
    pub hooks: bool,
    pub shim: bool,
    pub tmux_conf: bool,
    pub path_block: bool,
    pub silero: bool,
}
impl Status {
    /// Интеграция считается стоящей, если есть хуки и шим.
    pub fn integrated(&self) -> bool { self.hooks && self.shim }
}

pub type Progress<'a> = dyn Fn(Step) + 'a;
```

Шаги:
- [ ] Перенести из `setup.rs` все пути-хелперы, `MARKER`, `EVENTS`, `include_str!`-ассеты (HOOK/SHIM/TMUX/SILERO), хелперы `atomic_write`/`backup`/`read_settings`/`event_installed`/`merge_block`/`remove_block`/rc-логику, Silero-инсталл. Пути `include_str!` не меняются (та же глубина каталога).
- [ ] Реализовать `pub fn status() -> Status` (без печати — проверяет файлы/хуки).
- [ ] В `main.rs` добавить `mod install;` (рядом с прочими `mod`).
- [ ] Тест `status_detects_missing` на временном `$HOME` (через `JARVIS_DIR`/`HOME` override): пустой каталог → `integrated()==false`.
- [ ] `cargo test -p jarvis install::` зелёный. Commit.

### Task 2: install()/uninstall() с прогрессом + тонкий CLI

**Files:** Modify `src-tauri/src/install/mod.rs`, `src-tauri/src/bin/setup.rs`.

- [ ] `pub fn install(progress: &Progress)` — те же действия, что были в `setup::install`, но вместо `println!` зовёт `progress(Step{phase, state, msg})`. Фазы: `"Хуки"`, `"Транспорт"` (шим+tmux+PATH), `"Голос"` (Silero). Каждая: `Start` → … → `Done`/`Warn`.
- [ ] `pub fn uninstall(progress: &Progress)` аналогично.
- [ ] `setup.rs`: заменить тело на `#[path = "../install/mod.rs"] mod install;` и `main()` диспетчер `install/uninstall/status`, печатающий шаги (`▸ phase: msg`), для `status` — человекочитаемый дамп `Status`.
- [ ] Существующие 6 тестов setup (merge/remove/ours) переезжают в `install/mod.rs` и проходят.
- [ ] `npm run status`/`setup` работают как раньше (ручная проверка вывода). Commit.

### Task 3: Tauri-команды онбординга

**Files:** Create `src-tauri/src/onboarding.rs`; Modify `src-tauri/src/main.rs`.

```rust
//! Команды окна онбординга: статус + запуск установки со стримом шагов.
use crate::install::{self, Step};
use tauri::{AppHandle, Emitter};

#[tauri::command]
pub fn onboarding_status() -> install::Status { install::status() }

#[tauri::command]
pub fn onboarding_run(app: AppHandle) {
    std::thread::spawn(move || {
        install::install(&|step: Step| { let _ = app.emit_to("onboarding", "onboarding:progress", step); });
        let _ = app.emit_to("onboarding", "onboarding:done", ());
    });
}
```
- [ ] `mod onboarding;` в main.rs; добавить обе команды в `invoke_handler![]`.
- [ ] `cargo build -p jarvis` зелёный. Commit.

### Task 4: Окно онбординга (Rust) + первый запуск + пункт меню

**Files:** Modify `src-tauri/src/windows.rs`, `src-tauri/src/main.rs`, `src-tauri/src/tray.rs`.

- [ ] `windows::create_onboarding(app) -> Result<WebviewWindow>`: `WebviewUrl::App("onboarding.html")`, размер ~480×560, decorations(false), transparent(true), resizable(false), Theme::Dark, центр по экрану, visible(true).
- [ ] В `main.rs .setup`: после трея — `if !install::status().integrated() { windows::create_onboarding(app.handle())?; }`.
- [ ] `tray.rs`: пункт `MenuItem::with_id(app, "reinstall", "Переустановить интеграцию", true, None)`, в `on_menu` → `windows::create_onboarding` (или показать существующее).
- [ ] `cargo build` зелёный. Commit.

### Task 5: Красивый UI онбординга

**Files:** Create `ui/onboarding.html`, `ui/onboarding.js`.

Дизайн (токены панели Jarvis): тёмное стекло `rgba(10,10,12,.92)`, blur, radius 16; hero с иконкой приложения (`icons/128x128@2x.png`), заголовок «Настроим Jarvis», подзаголовок одной строкой; primary-кнопка (акцент `--working #6ca0ff`); список шагов — строки с иконкой состояния (○ ожидание · ◌ спиннер · ✓ `--done #41c98e` · ⚠ `--waiting`). Silero-шаг помечен «ставит PyTorch, это надолго». По `onboarding:done` — состояние «Готово», кнопка «Закрыть».

- [ ] `onboarding.html`: самодостаточный (CSP как в index.html), инлайн-CSS с токенами, разметка hero+steps+actions.
- [ ] `onboarding.js`: при загрузке `invoke('onboarding_status')` → показать, что уже стоит; кнопка → `invoke('onboarding_run')`; `listen('onboarding:progress')` → upsert строки шага по `phase`+`state`; `listen('onboarding:done')` → финал. Спиннер — CSS-анимация.
- [ ] Ручная проверка: собрать `npm run bundle`, удалить `~/.jarvis/shims` + хуки на тест-окружении (или временный $HOME), запустить .app → окно появляется, кнопка ставит, шаги бегут, по концу «Готово»; в `~/.claude/settings.json` появились хуки. Commit.

### Task 6: Полировка и финал

- [ ] `npm run bundle` собирается с онбордингом; дымовой запуск.
- [ ] `cargo test` весь воркспейс зелёный.
- [ ] README: отметить, что первый запуск ставит интеграцию (убрать «Фаза 2» из TODO).
- [ ] Commit.

---

## Самопроверка
- Вынос логики (спека §Фаза2.1) → Task 1–2. Окно+прогресс (§2.2) → Task 3–5. Пункт меню (§2.3) → Task 4.
- Риск: перенос ~400 строк из setup.rs. Митигация: переносим как есть, тесты setup переезжают и должны пройти без изменений (Task 2).
- `#[path]`-include в setup.rs не дублирует код и сохраняет один источник истины install-логики.
