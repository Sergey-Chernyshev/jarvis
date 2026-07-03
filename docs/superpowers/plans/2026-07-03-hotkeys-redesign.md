# Редизайн горячих клавиш — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Единый инлайн-рекордер хоткеев с приостановкой команд на время записи, детектом конфликтов и перехватом по подтверждению.

**Architecture:** Бэкенд получает реестр действий `HkAction` (единственный источник истины: ключ настройки, подпись, дефолт), три новые IPC-команды (`hotkey_bindings`, `hotkey_assign`, `hotkeys_suspend`) и состояние «не назначен» (сентинел `"none"`). UI переходит на один компонент `hotkeyRow` (капсула-рекордер) везде — вкладка «Горячие клавиши» (с группами), «Основное», «Голосовой ввод»; старые `hotkeyField`/`hotkeyEditorField`/`dictationHotkeyField` и пресеты удаляются.

**Tech Stack:** Rust (Tauri 2, tauri-plugin-global-shortcut), vanilla JS (`ui/settings2.js`, scoped CSS в нём же), спека: `docs/superpowers/specs/2026-07-03-hotkeys-redesign-design.md`.

**Ветка:** `feat/hotkeys-redesign` (уже создана, спека закоммичена).

**Сборка/тесты:**
- тесты: `cargo test --manifest-path src-tauri/Cargo.toml` (быстрее: `-- ipc::tests`)
- дев-сборка вживую: `npm start` (собирает с нужными features + codesign — только так, не голый cargo run)

---

## Карта файлов

| Файл | Что происходит |
|---|---|
| `src-tauri/src/ipc.rs` | + `HkAction`, `accel_from_raw`, `action_accel`, `action_shortcuts`, `find_conflict`, `hotkey_bindings`, `hotkey_assign`, `hotkeys_suspend`, suspend-логика; правка `register_*` (пропуск пустых); тесты |
| `src-tauri/src/daemon.rs` | + поля `hk_suspend_gen: AtomicU64`, `hk_select_was_on: AtomicBool` |
| `src-tauri/src/main.rs` | + 3 команды в `generate_handler` |
| `src-tauri/src/windows.rs` | `hide_panel` → ресюм хоткеев |
| `ui/bridge.js` | + `hotkeyBindings`, `hotkeyAssign`, `hotkeysSuspend` |
| `ui/settings2.js` | + `hotkeyRow` (рекордер) и CSS; переписать `renderKeys`; правки `renderGeneral`, `renderStt`; − `hotkeyField`, `hotkeyEditorField`, `dictationHotkeyField`, `HK_DEFAULTS`, CSS чипов/пресетов |

---

### Task 1: Реестр действий `HkAction` + «не назначен»

**Files:**
- Modify: `src-tauri/src/ipc.rs` (после `register_hotkey`, ~строка 88; тесты в `mod tests`, ~строка 1736)

- [ ] **Step 1: Написать падающие тесты**

В конец `mod tests` в `src-tauri/src/ipc.rs`:

```rust
    // --- реестр действий HkAction ---

    #[test]
    fn hk_action_parse_roundtrip() {
        for a in HkAction::ALL {
            assert_eq!(HkAction::parse(a.id()), Some(a));
        }
        assert_eq!(HkAction::parse("bogus"), None);
    }

    #[test]
    fn accel_from_raw_empty_is_default() {
        assert_eq!(
            accel_from_raw("", HkAction::Quiet),
            Some("Command+Alt+J".to_string())
        );
        assert_eq!(accel_from_raw("", HkAction::Dictation), Some("F8".to_string()));
    }

    #[test]
    fn accel_from_raw_none_is_unassigned() {
        assert_eq!(accel_from_raw(HK_NONE, HkAction::Mute), None);
    }

    #[test]
    fn accel_from_raw_select_normalizes() {
        // битый шаблон мягко деградирует в дефолт, как normalize_select_template
        assert_eq!(
            accel_from_raw("Command+Alt+5", HkAction::Select),
            Some(SELECT_TEMPLATE_DEFAULT.to_string())
        );
    }
```

- [ ] **Step 2: Убедиться, что тесты не компилируются (нет HkAction)**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- ipc::tests 2>&1 | tail -5`
Expected: ошибка компиляции `cannot find ... HkAction`

- [ ] **Step 3: Реализация**

В `src-tauri/src/ipc.rs` сразу после функции `register_hotkey` (после строки 88) вставить:

```rust
/* ================= реестр хоткей-действий ================= */

/// Сентинел «хоткей не назначен» в настройке (пустая строка значит «дефолт»,
/// поэтому нужен отдельный маркер — появляется после перехвата сочетания).
pub const HK_NONE: &str = "none";

/// Действие с глобальным хоткеем — единый реестр для назначения, детекта
/// конфликтов и приостановки на время записи сочетания.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HkAction {
    Panel,
    Continue,
    Repeat,
    Mute,
    Quiet,
    Select,
    Dictation,
}

impl HkAction {
    pub const ALL: [HkAction; 7] = [
        HkAction::Panel,
        HkAction::Continue,
        HkAction::Repeat,
        HkAction::Mute,
        HkAction::Quiet,
        HkAction::Select,
        HkAction::Dictation,
    ];

    /// Строковый id в IPC-контракте (bridge.js шлёт его в hotkey_assign).
    pub fn id(self) -> &'static str {
        match self {
            HkAction::Panel => "panel",
            HkAction::Continue => "continue",
            HkAction::Repeat => "repeat",
            HkAction::Mute => "mute",
            HkAction::Quiet => "quiet",
            HkAction::Select => "select",
            HkAction::Dictation => "dictation",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        HkAction::ALL.into_iter().find(|a| a.id() == s)
    }

    /// Подпись для сообщений о конфликте и списка привязок в UI.
    pub fn label(self) -> &'static str {
        match self {
            HkAction::Panel => "Открыть панель",
            HkAction::Continue => "Продолжить сессию",
            HkAction::Repeat => "Повторить",
            HkAction::Mute => "Без звука",
            HkAction::Quiet => "Тихий режим",
            HkAction::Select => "Варианты ответа",
            HkAction::Dictation => "Диктовка",
        }
    }

    pub fn default_accel(self) -> &'static str {
        match self {
            HkAction::Panel => "Command+J",
            HkAction::Continue => "Command+Alt+C",
            HkAction::Repeat => "Command+Alt+R",
            HkAction::Mute => "Command+Alt+M",
            HkAction::Quiet => "Command+Alt+J",
            HkAction::Select => SELECT_TEMPLATE_DEFAULT,
            HkAction::Dictation => "F8",
        }
    }

    /// Ключ в настройках. None — диктовка: живёт в settings.stt.hotkey,
    /// читается/пишется отдельным путём (SttConfig / set_stt).
    pub fn settings_key(self) -> Option<&'static str> {
        match self {
            HkAction::Panel => Some("hotkey"),
            HkAction::Continue => Some("continueHotkey"),
            HkAction::Repeat => Some("repeatHotkey"),
            HkAction::Mute => Some("muteHotkey"),
            HkAction::Quiet => Some("quietHotkey"),
            HkAction::Select => Some("selectHotkeyTemplate"),
            HkAction::Dictation => None,
        }
    }
}

/// Сырое значение настройки → акселератор действия.
/// "" → дефолт; HK_NONE → None («не назначен»); select нормализуется.
pub fn accel_from_raw(raw: &str, a: HkAction) -> Option<String> {
    if raw == HK_NONE {
        return None;
    }
    if raw.is_empty() {
        return Some(a.default_accel().to_string());
    }
    if a == HkAction::Select {
        return Some(normalize_select_template(raw));
    }
    Some(raw.to_string())
}

/// Текущий акселератор действия из настроек; None = «не назначен».
pub fn action_accel(d: &Arc<Daemon>, a: HkAction) -> Option<String> {
    let raw = match a {
        HkAction::Dictation => {
            crate::stt::config::SttConfig::from_settings(&d.settings.load()).hotkey
        }
        _ => d.settings.string(a.settings_key().expect("не-dictation имеет ключ")),
    };
    accel_from_raw(&raw, a)
}
```

- [ ] **Step 4: Переписать существующие аксессоры через реестр**

Там же в `ipc.rs` заменить ТЕЛА функций (сигнатуры не трогаем — их зовёт main.rs):

`quiet_accelerator` (строки ~91-98) →
```rust
/// Аккселератор тумблера тихого режима ("" = не назначен).
pub fn quiet_accelerator(d: &Arc<Daemon>) -> String {
    action_accel(d, HkAction::Quiet).unwrap_or_default()
}
```

Аналогично `continue_accelerator` → `HkAction::Continue`, `repeat_accelerator` → `HkAction::Repeat`, `mute_accelerator` → `HkAction::Mute`, `dictation_accelerator` → `HkAction::Dictation` (докстроки сохранить, добавив «"" = не назначен»).

`select_template` (строки ~243-245) →
```rust
/// Шаблон хоткеев выбора варианта (всегда валидный: normalize внутри).
pub fn select_template(d: &Arc<Daemon>) -> String {
    action_accel(d, HkAction::Select).unwrap_or_else(|| SELECT_TEMPLATE_DEFAULT.to_string())
}
```
(select с HK_NONE → «не назначен»: но `set_select_hotkeys` зовётся с этим шаблоном — поэтому None даёт дефолт ТОЛЬКО тут нельзя; см. Step 5: правильное поведение — при HK_NONE набор 1..9 просто не регистрируется. Меняем `set_select_hotkeys`:)

```rust
pub fn set_select_hotkeys(d: &Arc<Daemon>, on: bool) {
    // «не назначен» → снимать нечего и ставить нечего
    let Some(tpl) = action_accel(d, HkAction::Select) else { return };
    set_select_hotkeys_tpl(d, on, &tpl);
}
```
и `is_select_hotkey`:
```rust
pub fn is_select_hotkey(d: &Arc<Daemon>, shortcut: &Shortcut) -> Option<u32> {
    match_select_template(&action_accel(d, HkAction::Select)?, shortcut)
}
```
а `select_template` тогда УДАЛИТЬ и починить её call-sites: в `settings_set` (строка ~382 `let old = select_template(&d);`) заменить на `let old = action_accel(&d, HkAction::Select).unwrap_or_else(|| SELECT_TEMPLATE_DEFAULT.to_string());`. Другие call-sites найти `grep -n "select_template(" src-tauri/src/ -r` и поправить так же (кроме `normalize_select_template` / `set_select_hotkeys_tpl`).

- [ ] **Step 5: Пропуск пустых акселераторов в register_***

В каждой из `register_quiet_hotkey`, `register_continue_hotkey`, `register_dictation_hotkey`, `register_repeat_hotkey`, `register_mute_hotkey` первой строкой после получения `accel` добавить:

```rust
    if accel.is_empty() {
        return; // «не назначен»
    }
```

В `register_hotkey` (panel, с откатом) после `let current = ...`:
```rust
    if accelerator.is_empty() {
        // «не назначен»: снять текущий, ничего не регистрировать
        if !current.is_empty() {
            let _ = gs.unregister(current.as_str());
        }
        return Ok(());
    }
```
и обёртку от паники на пустом current: строку `let _ = gs.unregister(current.as_str());` внутри `if accelerator != current` дополнить гардом `if !current.is_empty()`.

В `main.rs:274` вызов `ipc::register_hotkey(&d, &d.settings.string("hotkey"))` заменить на
```rust
            let hk0 = ipc::action_accel(&d, ipc::HkAction::Panel).unwrap_or_default();
            if let Err(e) = ipc::register_hotkey(&d, &hk0) {
```
(иначе сырое `"none"` уйдёт в регистрацию как акселератор).

- [ ] **Step 6: Тесты зелёные**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- ipc::tests`
Expected: PASS все (старые 5 + новые 4)

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/ipc.rs src-tauri/src/main.rs
git commit -m "feat(hotkeys): реестр действий HkAction + состояние «не назначен» (сентинел none)"
```

---

### Task 2: Детект конфликтов (чистые функции)

**Files:**
- Modify: `src-tauri/src/ipc.rs`

- [ ] **Step 1: Падающие тесты**

В `mod tests`:

```rust
    // --- детект конфликтов ---

    fn b(a: HkAction, acc: &str) -> (HkAction, String) {
        (a, acc.to_string())
    }

    #[test]
    fn conflict_direct_hit() {
        let bindings = vec![b(HkAction::Mute, "Command+Alt+M")];
        assert_eq!(
            find_conflict(&bindings, HkAction::Quiet, "Command+Alt+M"),
            Some(HkAction::Mute)
        );
    }

    #[test]
    fn conflict_ignores_self_and_free() {
        let bindings = vec![b(HkAction::Quiet, "Command+Alt+J"), b(HkAction::Mute, "Command+Alt+M")];
        // то же действие — не конфликт (перезапись самого себя)
        assert_eq!(find_conflict(&bindings, HkAction::Quiet, "Command+Alt+J"), None);
        // свободное сочетание — не конфликт
        assert_eq!(find_conflict(&bindings, HkAction::Quiet, "Command+Alt+X"), None);
    }

    #[test]
    fn conflict_with_select_instance() {
        // ⌘⌥3 бьётся с экземпляром шаблона ⌘⌥{n}
        let bindings = vec![b(HkAction::Select, "Command+Alt+{n}")];
        assert_eq!(
            find_conflict(&bindings, HkAction::Dictation, "Command+Alt+3"),
            Some(HkAction::Select)
        );
    }

    #[test]
    fn conflict_new_select_template_vs_plain() {
        // новый шаблон ⌘⌃{n} бьётся с уже занятым ⌘⌃5
        let bindings = vec![b(HkAction::Repeat, "Command+Control+5")];
        assert_eq!(
            find_conflict(&bindings, HkAction::Select, "Command+Control+{n}"),
            Some(HkAction::Repeat)
        );
    }

    #[test]
    fn conflict_skips_broken_bindings() {
        let bindings = vec![b(HkAction::Mute, "Bogus+Nope")];
        assert_eq!(find_conflict(&bindings, HkAction::Quiet, "Command+Alt+M"), None);
    }
```

- [ ] **Step 2: Убедиться, что не компилируется**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- ipc::tests 2>&1 | tail -5`
Expected: `cannot find function find_conflict`

- [ ] **Step 3: Реализация**

После `action_accel` в `ipc.rs`:

```rust
/// Акселератор действия → конкретные шорткаты (select → до 9 экземпляров).
/// Битые части молча выпадают — битое не конфликтует.
pub fn action_shortcuts(a: HkAction, accel: &str) -> Vec<Shortcut> {
    if a == HkAction::Select {
        (1..=9)
            .filter_map(|n| select_accel(accel, n).parse::<Shortcut>().ok())
            .collect()
    } else {
        accel.parse::<Shortcut>().ok().into_iter().collect()
    }
}

/// Конфликт нового сочетания действия `a` с текущими привязками ОСТАЛЬНЫХ
/// действий. bindings — (действие, акселератор), «не назначенные» не передавать.
/// Чистая функция — покрыта юнитами без Daemon.
pub fn find_conflict(
    bindings: &[(HkAction, String)],
    a: HkAction,
    accel: &str,
) -> Option<HkAction> {
    let new = action_shortcuts(a, accel);
    bindings.iter().find_map(|(other, cur)| {
        if *other == a {
            return None;
        }
        let cur_sc = action_shortcuts(*other, cur);
        new.iter().any(|n| cur_sc.contains(n)).then_some(*other)
    })
}
```

- [ ] **Step 4: Тесты зелёные**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- ipc::tests`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/ipc.rs
git commit -m "feat(hotkeys): детект конфликтов между своими хоткеями (включая шаблон 1..9)"
```

---

### Task 3: Команды `hotkey_bindings` + `hotkey_assign`

**Files:**
- Modify: `src-tauri/src/ipc.rs`, `src-tauri/src/main.rs` (~строка 178), `ui/bridge.js` (~строка 75)

- [ ] **Step 1: Вспомогательные функции регистрации**

В `ipc.rs` после `find_conflict`:

```rust
/// Снять регистрацию текущего сочетания действия (select — весь набор).
fn unregister_action(d: &Arc<Daemon>, a: HkAction) {
    let Some(accel) = action_accel(d, a) else { return };
    let gs = d.app.global_shortcut();
    match a {
        HkAction::Select => {
            for n in 1..=9 {
                let _ = gs.unregister(select_accel(&accel, n).as_str());
            }
        }
        _ => {
            let _ = gs.unregister(accel.as_str());
        }
    }
}

/// Зарегистрировать сочетание действия. select регистрируется ТОЛЬКО при
/// активном вопросе (набор динамический — см. set_select_hotkeys), поэтому
/// принимает флаг. Err = сочетание занято системой.
fn register_action_accel(
    d: &Arc<Daemon>,
    a: HkAction,
    accel: &str,
    select_active: bool,
) -> Result<(), ()> {
    let gs = d.app.global_shortcut();
    match a {
        HkAction::Select => {
            if !select_active {
                return Ok(());
            }
            for n in 1..=9 {
                if gs.register(select_accel(accel, n).as_str()).is_err() {
                    for k in 1..n {
                        let _ = gs.unregister(select_accel(accel, k).as_str());
                    }
                    return Err(());
                }
            }
            Ok(())
        }
        _ => gs.register(accel).map_err(|_| ()),
    }
}

/// Сохранить сырое значение акселератора действия (HK_NONE = «не назначен»).
async fn persist_accel(d: &Arc<Daemon>, a: HkAction, raw: &str) {
    match a.settings_key() {
        Some(key) => {
            let _ = via_gate_panel(d, "settings.set", json!({ "patch": { key: raw } })).await;
        }
        None => {
            // диктовка: settings.stt.hotkey
            let mut patch = serde_json::Map::new();
            patch.insert("hotkey".into(), Value::String(raw.to_string()));
            d.settings.set_stt(patch);
        }
    }
}
```

- [ ] **Step 2: Команды**

Там же:

```rust
/// Привязки всех действий для UI настроек: id, подпись, текущее сочетание
/// (null = не назначен), дефолт.
#[tauri::command]
pub fn hotkey_bindings(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    let list: Vec<Value> = HkAction::ALL
        .iter()
        .map(|a| {
            json!({
                "action": a.id(),
                "label": a.label(),
                "accel": action_accel(&d, *a),
                "default": a.default_accel(),
            })
        })
        .collect();
    json!({ "ok": true, "bindings": list })
}

/// Назначить хоткей действию. Валидация → конфликт со своими (steal=false →
/// { ok:false, conflict } и ничего не меняется; steal=true → у конфликтующего
/// действия хоткей снимается в «не назначен») → перерегистрация с откатом
/// («занято системой» — как раньше).
#[tauri::command]
pub async fn hotkey_assign(
    app: AppHandle,
    action: String,
    accel: String,
    steal: Option<bool>,
) -> Value {
    let d = Daemon::get(&app);
    let Some(a) = HkAction::parse(&action) else {
        return err(format!("Неизвестное действие: {action}"));
    };
    let accel = accel.trim().to_string();
    if accel.is_empty() {
        return err("Пустое сочетание");
    }
    if a == HkAction::Select {
        if normalize_select_template(&accel) != accel {
            return err(format!("Битый шаблон «{accel}» — нужен вид Command+Alt+{{n}}"));
        }
    } else if accel.parse::<Shortcut>().is_err() {
        return err(format!("Не разобрал сочетание: {accel}"));
    }

    let old = action_accel(&d, a);
    if old.as_deref() == Some(accel.as_str()) {
        return json!({ "ok": true, "accel": accel });
    }

    // конфликты со своими хоткеями; перехват может каскадом задеть несколько
    // действий (напр. новый шаблон {n} бьётся с двумя) — снимаем в цикле
    let steal = steal.unwrap_or(false);
    loop {
        let bindings: Vec<(HkAction, String)> = HkAction::ALL
            .iter()
            .filter_map(|o| action_accel(&d, *o).map(|acc| (*o, acc)))
            .collect();
        let Some(other) = find_conflict(&bindings, a, &accel) else { break };
        if !steal {
            return json!({ "ok": false, "conflict": { "action": other.id(), "label": other.label() } });
        }
        unregister_action(&d, other);
        persist_accel(&d, other, HK_NONE).await;
        crate::log::line(&format!(
            "[hotkeys] перехват: «{}» остался без сочетания",
            other.label()
        ));
    }

    // активность набора 1..9 фиксируем ДО снятия старого
    let select_active = a == HkAction::Select
        && old
            .as_ref()
            .map(|o| {
                d.app
                    .global_shortcut()
                    .is_registered(select_accel(o, 1).as_str())
            })
            .unwrap_or(false);
    unregister_action(&d, a);
    if register_action_accel(&d, a, &accel, select_active).is_err() {
        if let Some(oldacc) = &old {
            let _ = register_action_accel(&d, a, oldacc, select_active);
        }
        return err(format!("Сочетание {accel} занято системой"));
    }
    persist_accel(&d, a, &accel).await;
    json!({ "ok": true, "accel": accel })
}
```

- [ ] **Step 3: Регистрация команд в main.rs**

В `generate_handler![` после `ipc::stt_set_hotkey,` (строка ~178):

```rust
            ipc::hotkey_bindings,
            ipc::hotkey_assign,
```

- [ ] **Step 4: Мост в bridge.js**

После строки `sttSetHotkey: ...` (~75):

```js
    hotkeyBindings: () => invoke('hotkey_bindings'),
    hotkeyAssign: (action, accel, steal) => invoke('hotkey_assign', { action, accel, steal: !!steal }),
```

- [ ] **Step 5: Компиляция + тесты**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- ipc::tests`
Expected: PASS, без warnings о неиспользуемом (команды подключены)

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/ipc.rs src-tauri/src/main.rs ui/bridge.js
git commit -m "feat(hotkeys): hotkey_bindings + hotkey_assign (конфликт/steal/откат) — IPC и мост"
```

---

### Task 4: Приостановка хоткеев на время записи

**Files:**
- Modify: `src-tauri/src/daemon.rs` (~строки 95-100 и конструктор ~224-232), `src-tauri/src/ipc.rs`, `src-tauri/src/windows.rs:259`, `src-tauri/src/main.rs`, `ui/bridge.js`

- [ ] **Step 1: Поля в Daemon**

В `pub struct Daemon` после поля `pub quiet: AtomicBool,`:

```rust
    /// Хоткеи приостановлены на время записи сочетания в настройках:
    /// 0 = работают; иначе поколение приостановки (для авто-ресюма).
    pub hk_suspend_gen: AtomicU64,
    /// Был ли активен набор 1..9 в момент приостановки (вернуть при ресюме).
    pub hk_select_was_on: AtomicBool,
```

В конструкторе рядом с `quiet: AtomicBool::new(quiet0),`:

```rust
            hk_suspend_gen: AtomicU64::new(0),
            hk_select_was_on: AtomicBool::new(false),
```

- [ ] **Step 2: Логика suspend/resume в ipc.rs**

После `hotkey_assign`:

```rust
/// Приостановить/вернуть ВСЕ глобальные хоткеи Jarvis — режим записи
/// сочетания в настройках: пока пользователь жмёт комбо, команды не должны
/// срабатывать (и наши же шорткаты не должны съедать keydown у webview).
/// Идемпотентно. Страховки от «умершего» UI: авто-ресюм через 15 с
/// (повторный suspend продлевает) и ресюм при скрытии панели.
pub fn hotkeys_set_suspended(d: &Arc<Daemon>, on: bool) {
    use std::sync::atomic::Ordering;
    let was = d.hk_suspend_gen.load(Ordering::SeqCst) != 0;
    if on {
        if !was {
            // активность набора 1..9 запоминаем ДО снятия
            let select_on = action_accel(d, HkAction::Select)
                .map(|t| {
                    d.app
                        .global_shortcut()
                        .is_registered(select_accel(&t, 1).as_str())
                })
                .unwrap_or(false);
            d.hk_select_was_on.store(select_on, Ordering::SeqCst);
            for a in HkAction::ALL {
                unregister_action(d, a);
            }
            crate::log::line("[hotkeys] приостановлены (запись сочетания)");
        }
        let gen = d.hk_suspend_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let d2 = d.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(15));
            if d2.hk_suspend_gen.load(Ordering::SeqCst) == gen {
                crate::log::line("[hotkeys] авто-ресюм по таймауту — UI не вернул хоткеи");
                hotkeys_set_suspended(&d2, false);
            }
        });
    } else {
        if !was {
            return;
        }
        d.hk_suspend_gen.store(0, Ordering::SeqCst);
        if let Err(e) = register_hotkey(d, &action_accel(d, HkAction::Panel).unwrap_or_default()) {
            crate::log::line(&format!("[hotkeys] ресюм панели: {e}"));
        }
        register_quiet_hotkey(d);
        register_continue_hotkey(d);
        register_dictation_hotkey(d);
        register_repeat_hotkey(d);
        register_mute_hotkey(d);
        if d.hk_select_was_on.load(Ordering::SeqCst) {
            set_select_hotkeys(d, true);
        }
        crate::log::line("[hotkeys] возвращены");
    }
}

#[tauri::command]
pub fn hotkeys_suspend(app: AppHandle, on: bool) -> Value {
    hotkeys_set_suspended(&Daemon::get(&app), on);
    ok()
}
```

Примечание: `register_hotkey` сравнивает с `settings.string("hotkey")` и выйдет по ветке `accelerator == current && is_registered` — после ресюма current совпадает, но `is_registered=false`, поэтому регистрация состоится; ветка `accelerator != current` не сработает — лишнего unregister не будет. Проверить глазами при реализации.

- [ ] **Step 3: Ресюм при скрытии панели**

`src-tauri/src/windows.rs:259`:

```rust
pub fn hide_panel(d: &Arc<Daemon>) {
    // запись сочетания не должна пережить панель — вернуть хоткеи
    crate::ipc::hotkeys_set_suspended(d, false);
    if let Some(panel) = d.app.get_webview_window("main") {
        let _ = panel.hide();
    }
}
```

- [ ] **Step 4: Команда в main.rs + мост**

`main.rs` в `generate_handler!` после `ipc::hotkey_assign,`:
```rust
            ipc::hotkeys_suspend,
```

`ui/bridge.js` после `hotkeyAssign`:
```js
    hotkeysSuspend: (on) => invoke('hotkeys_suspend', { on }),
```

- [ ] **Step 5: Компиляция + тесты**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- ipc::tests`
Expected: PASS (логика suspend юнитами не покрывается — нет AppHandle; проверяется вживую в Task 5/7)

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/daemon.rs src-tauri/src/ipc.rs src-tauri/src/windows.rs src-tauri/src/main.rs ui/bridge.js
git commit -m "feat(hotkeys): приостановка всех хоткеев на время записи + авто-ресюм (таймаут, скрытие панели)"
```

---

### Task 5: СПАЙК — отдаёт ли webview ⌘/⌥-комбо при приостановленных шорткатах

Судьбоносный для UI: если да — рекордер ловит keydown как у диктовки; если нет — нужен запасной путь (Task 5b).

**Files:** нет изменений кода (проверка вживую)

- [ ] **Step 1: Собрать и запустить дев**

Run (фоном): `npm start`
Дождаться в логе `[jarvis] слушаю ... run.sock`.

- [ ] **Step 2: Проверить руками**

1. Открыть панель Jarvis (⌘J) → Настройки → «Голосовой ввод».
2. В webview-консоли панели (Safari → Develop → jarvis, либо временно `console.log` в `dictationHotkeyField`) выполнить `await window.jarvis.hotkeysSuspend(true)`.
3. Кликнуть в поле «Клавиша диктовки» (старый рекордер) и нажать `⌘⌥D`.
4. Смотреть: дошёл ли keydown (поле применит сочетание или покажет «не удалось» от бэкенда — оба исхода значат, что ДОШЁЛ). Если поле осталось в «Нажмите сочетание…» — комбо съедено.
5. `await window.jarvis.hotkeysSuspend(false)`; проверить, что ⌘J снова открывает/скрывает панель.

- [ ] **Step 3: Зафиксировать исход**

- Дошло → Task 5b пропускается, идём в Task 6.
- Съедено → выполняется Task 5b (NSEvent-монитор), Task 6 подключает его событие вместо keydown.

Результат спайка дописать в спеку (`docs/superpowers/specs/2026-07-03-hotkeys-redesign-design.md`, раздел «Решение — техника», п.2) одной строкой и закоммитить вместе со следующим таском.

---

### Task 5b (УСЛОВНЫЙ — только если спайк провалился): NSEvent-монитор записи

**Files:**
- Modify: `src-tauri/src/macos.rs`, `src-tauri/src/ipc.rs`

- [ ] **Step 1: Локальный монитор клавиатуры**

В `src-tauri/src/macos.rs` (там уже есть objc2-код — следовать его стилю импортов) добавить функцию, которая через `NSEvent.addLocalMonitorForEventsMatchingMask(NSEventMaskKeyDown)` при включении шлёт каждое нажатие событием `hk-rec-key` окну `main`:

```rust
/// Локальный монитор keyDown на время записи сочетания: webview на macOS
/// может не отдавать ⌘/⌥-комбо в JS, монитор ловит их на уровне NSApp и
/// шлёт в UI событием `hk-rec-key` { mods: [...], key: "D" | "F8" | ... }.
/// Возвращает токен монитора; remove_key_monitor(token) снимает.
pub fn add_key_monitor(app: &tauri::AppHandle) -> Option<KeyMonitorToken> { ... }
pub fn remove_key_monitor(token: KeyMonitorToken) { ... }
```

Маппинг keyCode→имя клавиши — только [A-Z], [0-9], F1-F24, Space (зеркало `eventToAccel`). Хранение токена — в `Mutex<Option<...>>` рядом с suspend-состоянием; `hotkeys_set_suspended(true)` ставит монитор, `(false)` — снимает.

- [ ] **Step 2: UI слушает событие**

В рекордере (Task 6) вместо `document.addEventListener('keydown', ...)` подписка `window.__TAURI__.event.listen('hk-rec-key', ...)` тем же обработчиком (форма `{mods, key}` совпадает с `eventToAccel`). Точный код — по месту, интерфейс тот же.

- [ ] **Step 3: Компиляция + ручная проверка + commit**

```bash
git add src-tauri/src/macos.rs src-tauri/src/ipc.rs ui/settings2.js
git commit -m "feat(hotkeys): NSEvent-монитор записи сочетаний (webview ест ⌘/⌥-комбо)"
```

---

### Task 6: UI — компонент `hotkeyRow` и перестройка панелей

**Files:**
- Modify: `ui/settings2.js`:
  - удалить: `HK_DEFAULTS` (~31-39), `hotkeyField` (~353-366), `hotkeyEditorField` (~368-482), `dictationHotkeyField` (~514-586)
  - добавить: `hotkeyRow` (на место `hotkeyField`)
  - CSS в `injectStyle` (~734-751): заменить блок хоткеев
  - `renderGeneral` (~842-846), `renderStt` (~925-927), `renderKeys` (~1331-1363)

- [ ] **Step 1: Новый компонент hotkeyRow**

Вместо удалённого `hotkeyField` (само место в файле — после `segmented`):

```js
  /* ── Строка хоткея с инлайн-рекордером (Raycast-style) ────────────────────
   * b: { action, label, accel, default } из hotkeyBindings() (accel: null =
   * «не назначен»). Клик по капсуле → запись: бэкенд снимает ВСЕ глобальные
   * хоткеи (hotkeysSuspend — команды не срабатывают, и наши шорткаты не
   * съедают keydown), жмёшь комбо целиком → hotkeyAssign. Esc / клик мимо /
   * 12 с тишины — отмена (бэкенд сам вернёт хоткеи через 15 с, если UI умер).
   * Конфликт со своим хоткеем → красная строка + «Всё равно назначить»
   * (steal: у конфликтующего действия сочетание снимается в «не назначен»).
   * action='select': основная клавиша фиксирована «1…9» — в записи нужна
   * любая цифра, в акселератор идёт {n}. opts.after() — после успешного
   * применения (перерисовать пары-дубли в других вкладках). */
  function hotkeyRow(b, desc, opts) {
    const isSel = b.action === 'select';
    let acc = b.accel; // string | null
    const row = el('div.drow');
    const left = el('div.grow');
    left.appendChild(el('div.dt', { text: b.label }));
    if (desc) left.appendChild(el('div.dd', { text: desc }));
    const errBox = el('div.hkerr');
    errBox.style.display = 'none';
    left.appendChild(errBox);
    const cap = el('div.hkey.rec', { title: 'Кликни и нажми сочетание' });
    const rb = el('button.hkreset', { title: 'Сбросить' }, icon('rotate-ccw'));
    const ctl = el('div.dctl.hk', null, [cap, rb]);
    row.appendChild(left);
    row.appendChild(ctl);

    const clearErr = () => { row.classList.remove('conflict'); errBox.style.display = 'none'; errBox.replaceChildren(); };
    const paint = () => {
      clearErr();
      cap.classList.remove('recording');
      cap.classList.toggle('none', !acc);
      cap.replaceChildren();
      if (!acc) { cap.appendChild(el('span.hknone', { text: 'не назначен' })); return; }
      if (isSel) {
        for (const k of hotkeyKeys(acc.replace('+{n}', ''))) cap.appendChild(el('kbd', { text: k }));
        cap.appendChild(el('kbd.fix', { text: '1…9' }));
      } else {
        for (const k of hotkeyKeys(acc)) cap.appendChild(el('kbd', { text: k }));
      }
    };
    const note = (txt) => { cap.replaceChildren(el('span.ph', { text: txt })); };
    const done = () => { paint(); if (opts && opts.after) opts.after(); };

    const showConflict = (conf, next) => {
      paint();
      row.classList.add('conflict');
      const shown = isSel ? displayHotkey(next.replace('{n}', '1…9')) : displayHotkey(next);
      errBox.appendChild(el('span', { text: '⚠ ' + shown + ' занято «' + conf.label + '» · ' }));
      const steal = el('button.hksteal', { text: 'Всё равно назначить' });
      steal.addEventListener('click', async (e) => {
        e.stopPropagation();
        const res = await safe(() => window.jarvis.hotkeyAssign(b.action, next, true), null);
        if (res && res.ok) { acc = res.accel; done(); }
        else { note((res && res.error) || 'не удалось'); setTimeout(paint, 1600); }
      });
      errBox.appendChild(steal);
      errBox.style.display = '';
    };

    const applyAccel = async (next) => {
      const res = await safe(() => window.jarvis.hotkeyAssign(b.action, next, false), null);
      if (res && res.ok) { acc = res.accel; done(); return; }
      if (res && res.conflict) { showConflict(res.conflict, next); return; }
      note((res && res.error) || 'не удалось');
      setTimeout(paint, 1600);
    };

    let recording = false, onKey = null, recTimer = 0;
    function stopRec() {
      if (!recording) return;
      recording = false;
      clearTimeout(recTimer);
      if (onKey) { document.removeEventListener('keydown', onKey, true); onKey = null; }
      document.removeEventListener('click', onAway, true);
      fire(() => window.jarvis.hotkeysSuspend(false));
      paint();
    }
    function onAway(e) { if (!cap.contains(e.target)) stopRec(); }
    function startRec() {
      if (recording) return;
      recording = true;
      clearErr();
      fire(() => window.jarvis.hotkeysSuspend(true));
      cap.classList.add('recording');
      cap.classList.remove('none');
      note(isSel ? 'Нажмите сочетание с цифрой…' : 'Нажмите сочетание…');
      recTimer = setTimeout(stopRec, 12000); // раньше авто-ресюма бэкенда (15 с)
      onKey = (e) => {
        e.preventDefault(); e.stopPropagation();
        if (e.key === 'Escape') { stopRec(); return; }
        if (['Shift', 'Control', 'Alt', 'Meta'].includes(e.key)) return; // ждём основную
        const { mods, key, isFn } = eventToAccel(e);
        if (!key) { note('Эта клавиша не поддерживается'); return; }
        if (isSel) {
          if (!/^\d$/.test(key)) { note('Нужна цифра 1–9'); return; }
          if (!mods.length) { note('Нужен модификатор (⌘/⌥/⌃)'); return; }
        } else if (!isFn && mods.length === 0) {
          note('Нужен модификатор (⌘/⌥/⌃) или F-клавиша'); return;
        }
        const next = mods.concat(isSel ? '{n}' : key).join('+');
        recording = false;
        clearTimeout(recTimer);
        document.removeEventListener('keydown', onKey, true); onKey = null;
        document.removeEventListener('click', onAway, true);
        fire(() => window.jarvis.hotkeysSuspend(false));
        applyAccel(next);
      };
      document.addEventListener('keydown', onKey, true);
      document.addEventListener('click', onAway, true);
    }
    cap.addEventListener('click', (e) => { e.stopPropagation(); startRec(); });
    rb.addEventListener('click', (e) => { e.stopPropagation(); applyAccel(b.default); });
    paint();
    return row;
  }
```

Примечание: `el('div.dctl.hk', null, [cap, rb])` — проверить сигнатуру `el` в файле (используется как `el('div.drow', null, [...])` в других местах — форма та же).

- [ ] **Step 2: CSS**

В `injectStyle` блок «Хоткей-поле (Raycast-style)» (строки ~734-751) заменить на:

```css
/* ── Хоткей-поле (Raycast-style, инлайн-рекордер) ─────────────────────── */
#settings2 .dctl.hk { gap:6px; }
#settings2 .hkey { display:inline-flex; align-items:center; gap:8px; padding:8px 13px; border-radius:8px; background:rgba(255,255,255,0.05); border:1px solid rgba(255,255,255,0.08); transition:background .15s ease, box-shadow .15s ease; }
#settings2 .hkey kbd { font:500 13px/1 var(--s2-font); color:var(--text,#e7e7ea); background:transparent; border:0; padding:0; }
#settings2 .hkey kbd.fix { color:var(--working,#6ca0ff); }
#settings2 .hkey.rec { background:rgba(108,160,255,0.1); border-color:rgba(108,160,255,0.25); cursor:pointer; }
#settings2 .hkey.rec:hover { border-color:rgba(108,160,255,0.45); }
#settings2 .hkey.rec kbd { color:var(--working,#6ca0ff); }
#settings2 .hkey .ph { font:500 12px/1 var(--s2-font); color:var(--working,#6ca0ff); }
#settings2 .hkey.recording { background:rgba(108,160,255,0.18); border-color:var(--working,#6ca0ff); animation:s2hkpulse 1.2s ease-in-out infinite; }
@keyframes s2hkpulse { 0%,100% { box-shadow:0 0 0 3px rgba(108,160,255,.10); } 50% { box-shadow:0 0 0 6px rgba(108,160,255,.22); } }
#settings2 .hkey.none { border-style:dashed; }
#settings2 .hknone { font:400 12px/1 var(--s2-font); color:var(--faint,#55555c); font-style:italic; }
#settings2 .hkreset { width:32px; height:32px; border-radius:8px; display:grid; place-items:center; background:transparent; border:0; color:var(--faint,#55555c); cursor:default; visibility:hidden; }
#settings2 .drow:hover .hkreset { visibility:visible; }
#settings2 .hkreset:hover { color:var(--text-body,#d6d6db); background:rgba(255,255,255,0.06); }
#settings2 .hkreset svg.lucide { width:15px; height:15px; }
#settings2 .drow.conflict { background:rgba(242,97,92,.05); }
#settings2 .drow.conflict .hkey { border-color:rgba(242,97,92,.55); }
#settings2 .hkerr { display:flex; align-items:center; gap:6px; margin-top:7px; font-size:11.5px; color:var(--danger,#f2615c); flex-wrap:wrap; }
#settings2 .hksteal { appearance:none; border:0; background:transparent; padding:0; font:500 11.5px/1 var(--s2-font); color:var(--waiting,#f2a33c); text-decoration:underline; cursor:pointer; }
```

(`.hkpresets`, `.hkchip`, `.hkmods` — удалены.)

- [ ] **Step 3: renderKeys — группы**

Заменить целиком (строки ~1331-1363):

```js
  // 7. Горячие клавиши (keys) — hotkey_bindings (единый реестр действий)
  async function renderKeys(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Горячие клавиши' }));
    const _sk = skelGroup(4); pane.appendChild(_sk);
    const r = await safe(() => window.jarvis.hotkeyBindings(), null);
    _sk.remove();
    if (!r || !r.ok) {
      pane.appendChild(el('div.dgroup', null, [drow('Недоступно', 'Не удалось получить привязки хоткеев.', [])]));
      return;
    }
    const by = {};
    for (const x of r.bindings || []) by[x.action] = x;
    const DESC = {
      panel: 'Показать или скрыть Jarvis.',
      continue: 'Возобновить последнюю сессию.',
      repeat: 'Повторить последнее уведомление.',
      select: 'Выбрать вариант активного вопроса — сочетание + цифра.',
      mute: 'Заглушить уведомления и голос.',
      quiet: 'Копить статистику без тостов.',
      dictation: 'Зажми и говори (push-to-talk). Дублируется в «Голосовом вводе».',
    };
    const GROUPS = [
      ['Панель и сессии', ['panel', 'continue', 'repeat', 'select']],
      ['Звук и уведомления', ['mute', 'quiet']],
      ['Голос', ['dictation']],
    ];
    // перехват меняет ЧУЖУЮ строку («не назначен») — перерисовать вкладку
    const after = () => reRenderPane('keys');
    for (const [title, actions] of GROUPS) {
      pane.appendChild(el('div.dsection', { text: title }));
      const g = el('div.dgroup');
      for (const id of actions) if (by[id]) g.appendChild(hotkeyRow(by[id], DESC[id], { after }));
      pane.appendChild(g);
    }
  }
```

- [ ] **Step 4: renderGeneral — редактируемый глобальный хоткей**

Заменить блок «глобальный хоткей (read-only капсы) + сброс» (строки ~842-846):

```js
    // глобальный хоткей — тот же рекордер, что во вкладке «Горячие клавиши»
    const hkr = await safe(() => window.jarvis.hotkeyBindings(), null);
    const pb = hkr && hkr.ok && (hkr.bindings || []).find((x) => x.action === 'panel');
    if (pb) group.appendChild(hotkeyRow(pb, 'Открыть панель Jarvis из любого места.', {}));
```

- [ ] **Step 5: renderStt — диктовка на общем рекордере**

Заменить строку с `dictationHotkeyField` (строки ~925-927):

```js
    // клавиша диктовки — общий рекордер (пресеты убраны: запись работает)
    const hkr = await safe(() => window.jarvis.hotkeyBindings(), null);
    const db = hkr && hkr.ok && (hkr.bindings || []).find((x) => x.action === 'dictation');
    if (db) group.appendChild(hotkeyRow(db, 'Зажми и говори (push-to-talk). Кликни и нажми новое сочетание.', {}));
```

- [ ] **Step 6: Удалить мёртвый код**

Удалить: `HK_DEFAULTS` (31-39), `hotkeyField` (353-366), `hotkeyEditorField` (368-482), `dictationHotkeyField` (514-586). Проверить, что не осталось ссылок:

Run: `grep -n "HK_DEFAULTS\|hotkeyField\|hotkeyEditorField\|dictationHotkeyField\|hkchip\|hkpresets\|hkmods\|sttSetHotkey" ui/settings2.js`
Expected: пусто (`eventToAccel`, `hotkeyKeys`, `displayHotkey` остаются — их зовёт `hotkeyRow`).

- [ ] **Step 7: Commit**

```bash
git add ui/settings2.js
git commit -m "feat(hotkeys): единый инлайн-рекордер hotkeyRow — группы, конфликт+перехват, «не назначен»; чипы и пресеты удалены"
```

---

### Task 7: Живая проверка и финал

- [ ] **Step 1: Полные тесты**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: все PASS

- [ ] **Step 2: Дев-сборка и ручной прогон**

Run (фоном): `npm start`. Проверить по чек-листу:

1. «Горячие клавиши»: три группы, капсулы с символами, ↺ по ховеру.
2. Запись: клик → пульсация; ⌘⇧Y на «Продолжить сессию» → применилось.
3. Во время записи ⌘⌥M (mute) НЕ срабатывает (хоткеи приостановлены), а записывается.
4. Esc в записи → откат; клик мимо → откат; хоткеи снова работают (⌘J).
5. Конфликт: назначить на «Без звука» сочетание «Тихого режима» → красная строка, «Всё равно назначить» → перехват, у «Тихого режима» — «не назначен».
6. Сброс ↺ у «Тихого режима» → дефолт вернулся (и конфликт-детект: если дефолт занят — красная строка).
7. «Варианты ответа»: запись ⌘⌃5 → капсула ⌘⌃ 1…9.
8. «Основное»: глобальный хоткей редактируется, синхронно меняется во вкладке хоткеев.
9. «Голосовой ввод»: диктовка записывается рекордером, пресетов нет; PTT работает после смены.
10. Запись начата и панель скрыта (⌘J не сработает — приостановлен; закрыть кликом в трей/Esc) → хоткеи вернулись (лог `[hotkeys] возвращены`).
11. Ждать 15+ с в записи → авто-ресюм в логе, UI сам вышел из записи (12 с).
12. Метрики/лог без паник.

- [ ] **Step 3: Спека — отметка об исходе спайка** (если не сделано в Task 5)

- [ ] **Step 4: Финальный коммит ветки и PR**

```bash
git add -A && git commit -m "docs(plan): чек-лист живой проверки хоткеев пройден" # если были правки
git push -u origin feat/hotkeys-redesign
gh pr create --title "feat(hotkeys): инлайн-рекордер, конфликты с перехватом, приостановка команд при записи" --body "..."
```
---

## Самопроверка плана

- Покрытие спеки: рекордер (T6), приостановка+страховки (T4), спайк+запасной путь (T5/5b), конфликт+steal (T2/T3/T6), «не назначен» (T1/T3/T6), группы вкладки (T6), редактируемый «Основное» (T6), диктовка без пресетов (T6), тесты (T1/T2/T7), живая проверка (T7).
- Типы согласованы: `hotkey_assign(action: String, accel: String, steal: Option<bool>)` ↔ `hotkeyAssign(action, accel, steal)`; `bindings[].{action,label,accel,default}` ↔ `hotkeyRow(b, …)`.
- Без плейсхолдеров: код полный, кроме Task 5b (условный, интерфейс зафиксирован — деталь objc2 по месту, допустимо: может не понадобиться).
