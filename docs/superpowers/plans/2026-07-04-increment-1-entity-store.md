# Инкремент 1: EntityStore + capability `entities.*` — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Реестр сущностей ядра (HA-модель): плагины публикуют нормализованные данные
(`vm.*`, …), ядро хранит их, отдаёт по `entities.query` и эмитит в панель. Без UI.

**Architecture:** Новый чистый модуль `entities.rs` (store + pure-логика publish, полностью
юнит-тестируемый) + тонкий capability-фасад `native/entities_cap.rs` по образцу
`native/sessions.rs`. Личность вызывающего хендлеры получают через служебный ключ
`_consumer`, который гейт (`capability/gate.rs`) инжектит в args после всех проверок —
подделать нельзя (перезаписывается всегда).

**Tech Stack:** Rust (Tauri), существующая capability-платформа (`Registry`, `invoke`,
`Consumer::plugin`), `serde_json`, инлайн `#[cfg(test)]`-тесты (конвенция репо).

**Spec:** `docs/superpowers/specs/2026-07-03-plugin-system-agent-vm-design.md` §6.4, §6.9(1).

**Рабочая директория:** worktree `.worktrees/agent-vm-plugin`, ветка `feat/agent-vm-plugin`.
Команды тестов запускать из `src-tauri/` (`cd src-tauri`). Не переформатировать чужой код
(CI fmt — информационный, намеренно).

---

### Task 1: Модуль `entities.rs` — типы, store, pure-логика publish

**Files:**
- Create: `src-tauri/src/entities.rs`
- Modify: `src-tauri/src/main.rs` (одна строка `mod entities;`)

- [ ] **Step 1: Создать `src-tauri/src/entities.rs` с типами, сигнатурами (`todo!()`) и тестами**

```rust
//! Реестр сущностей ядра (спека §6.4, HA-модель): плагины публикуют
//! нормализованные данные (`vm.<name>`, …), любой потребитель читает их через
//! ядро, не зная источника. Store чистый (без AppHandle) — эмит в панель
//! делает capability-фасад (native/entities_cap.rs).

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{json, Value};

use crate::util::now_ms;

/// Одна сущность. `id` = `<kind>.<object_id>`, владелец — только пишущий
/// (`plugin:<id>`). `stale` = владелец остановлен, данные могли устареть.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    pub id: String,
    pub kind: String,
    pub owner: String,
    pub state: String,
    pub attrs: Value,
    pub updated_at: i64,
    pub stale: bool,
}

#[derive(Default)]
pub struct EntityStore {
    items: Mutex<HashMap<String, Entity>>,
}

impl EntityStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Создать/обновить сущность. Ошибка: пустые kind/object_id, точка в kind
    /// (ломает формат id), attrs не объект, чужой владелец.
    pub fn upsert(
        &self,
        owner: &str,
        kind: &str,
        object_id: &str,
        state: &str,
        attrs: Value,
    ) -> Result<Entity, String> {
        todo!()
    }

    /// Удалить свою сущность. Ok(false) — не было; ошибка — чужой владелец.
    pub fn remove(&self, owner: &str, id: &str) -> Result<bool, String> {
        todo!()
    }

    /// Сущности с фильтрами по kind/owner, отсортированы по id (детерминизм).
    pub fn query(&self, kind: Option<&str>, owner: Option<&str>) -> Vec<Entity> {
        todo!()
    }

    /// Пометить все сущности владельца как stale (плагин остановлен).
    /// Возвращает число помеченных. Живой upsert снимает пометку.
    pub fn mark_stale(&self, owner: &str) -> usize {
        todo!()
    }

    pub fn snapshot(&self) -> Vec<Entity> {
        self.query(None, None)
    }
}

/// Pure-логика capability `entities.publish`: разбор args + мутация store.
/// Владелец берётся ТОЛЬКО из `_consumer` (инжектится гейтом, не подделать);
/// доступно только плагинам. Вынесено из хендлера ради юнит-тестов без Daemon.
pub fn apply_publish(store: &EntityStore, args: &Value) -> Result<Value, String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_creates_then_updates_and_clears_stale() {
        let s = EntityStore::new();
        let e = s.upsert("plugin:avm", "vm", "my-api", "running", json!({"cpus": 4})).unwrap();
        assert_eq!(e.id, "vm.my-api");
        assert_eq!(e.owner, "plugin:avm");
        assert!(!e.stale);

        assert_eq!(s.mark_stale("plugin:avm"), 1);
        assert!(s.query(None, None)[0].stale);

        let e2 = s.upsert("plugin:avm", "vm", "my-api", "stopped", json!({})).unwrap();
        assert_eq!(e2.state, "stopped");
        assert!(!e2.stale, "живой upsert снимает stale");
        assert_eq!(s.query(None, None).len(), 1, "upsert не плодит дубликат");
    }

    #[test]
    fn upsert_rejects_foreign_owner() {
        let s = EntityStore::new();
        s.upsert("plugin:avm", "vm", "x", "running", json!({})).unwrap();
        let err = s.upsert("plugin:other", "vm", "x", "stopped", json!({})).unwrap_err();
        assert!(err.contains("plugin:avm"), "ошибка называет владельца: {err}");
    }

    #[test]
    fn upsert_validates_input() {
        let s = EntityStore::new();
        assert!(s.upsert("plugin:avm", "", "x", "on", json!({})).is_err(), "пустой kind");
        assert!(s.upsert("plugin:avm", "vm", "", "on", json!({})).is_err(), "пустой object_id");
        assert!(s.upsert("plugin:avm", "a.b", "x", "on", json!({})).is_err(), "точка в kind");
        assert!(s.upsert("plugin:avm", "vm", "x", "on", json!([1])).is_err(), "attrs не объект");
        assert!(s.upsert("plugin:avm", "vm", "x", "on", Value::Null).is_ok(), "null → пустой объект");
    }

    #[test]
    fn remove_own_only() {
        let s = EntityStore::new();
        s.upsert("plugin:avm", "vm", "x", "on", json!({})).unwrap();
        assert!(s.remove("plugin:other", "vm.x").is_err(), "чужую нельзя");
        assert_eq!(s.remove("plugin:avm", "vm.x").unwrap(), true);
        assert_eq!(s.remove("plugin:avm", "vm.x").unwrap(), false, "повторно — не было");
    }

    #[test]
    fn query_filters_and_sorts() {
        let s = EntityStore::new();
        s.upsert("plugin:avm", "vm", "b", "on", json!({})).unwrap();
        s.upsert("plugin:avm", "vm", "a", "on", json!({})).unwrap();
        s.upsert("plugin:other", "agent", "z", "idle", json!({})).unwrap();

        let all = s.query(None, None);
        assert_eq!(all.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["agent.z", "vm.a", "vm.b"]);
        assert_eq!(s.query(Some("vm"), None).len(), 2);
        assert_eq!(s.query(None, Some("plugin:other")).len(), 1);
        assert_eq!(s.query(Some("vm"), Some("plugin:other")).len(), 0);
    }

    #[test]
    fn publish_requires_plugin_consumer() {
        let s = EntityStore::new();
        let err = apply_publish(&s, &json!({"kind": "vm", "id": "x"})).unwrap_err();
        assert!(err.contains("плагин"), "без _consumer — отказ: {err}");
        let err = apply_publish(
            &s,
            &json!({"_consumer": "agent", "kind": "vm", "id": "x"}),
        )
        .unwrap_err();
        assert!(err.contains("плагин"), "агент — отказ: {err}");
    }

    #[test]
    fn publish_upsert_and_remove_roundtrip() {
        let s = EntityStore::new();
        let out = apply_publish(
            &s,
            &json!({
                "_consumer": "plugin:avm", "kind": "vm", "id": "my-api",
                "state": "provisioning", "attrs": {"modules": ["claude"]}
            }),
        )
        .unwrap();
        assert_eq!(out["entity"]["id"], "vm.my-api");
        assert_eq!(s.query(Some("vm"), None)[0].state, "provisioning");

        let out = apply_publish(
            &s,
            &json!({"_consumer": "plugin:avm", "op": "remove", "kind": "vm", "id": "my-api"}),
        )
        .unwrap();
        assert_eq!(out["removed"], true);
        assert!(s.query(None, None).is_empty());
    }

    #[test]
    fn publish_rejects_unknown_op() {
        let s = EntityStore::new();
        let err = apply_publish(
            &s,
            &json!({"_consumer": "plugin:avm", "op": "explode", "kind": "vm", "id": "x"}),
        )
        .unwrap_err();
        assert!(err.contains("op"), "{err}");
    }
}
```

- [ ] **Step 2: Подключить модуль в `src-tauri/src/main.rs`**

После строки `mod daemon;` (алфавитный порядок соседних `mod`) добавить:

```rust
mod entities; // реестр сущностей ядра (спека plugin-system §6.4)
```

- [ ] **Step 3: Прогнать тесты — убедиться, что падают на `todo!()`**

Run: `cd src-tauri && cargo test entities::`
Expected: компилируется, тесты `entities::tests::*` PANIC «not yet implemented» (кроме,
возможно, `publish_requires_plugin_consumer` — он тоже упадёт на `todo!()`).

- [ ] **Step 4: Реализовать методы store и `apply_publish`**

Заменить `todo!()`-тела:

```rust
    pub fn upsert(
        &self,
        owner: &str,
        kind: &str,
        object_id: &str,
        state: &str,
        attrs: Value,
    ) -> Result<Entity, String> {
        if kind.is_empty() || object_id.is_empty() {
            return Err("kind и id обязательны".into());
        }
        if kind.contains('.') {
            return Err(format!("kind не может содержать точку: '{kind}'"));
        }
        let attrs = match attrs {
            Value::Null => json!({}),
            Value::Object(_) => attrs,
            _ => return Err("attrs должен быть объектом".into()),
        };
        let id = format!("{kind}.{object_id}");
        let mut items = self.items.lock().unwrap();
        if let Some(prev) = items.get(&id) {
            if prev.owner != owner {
                return Err(format!("сущность '{id}' принадлежит {}", prev.owner));
            }
        }
        let e = Entity {
            id: id.clone(),
            kind: kind.to_string(),
            owner: owner.to_string(),
            state: state.to_string(),
            attrs,
            updated_at: now_ms(),
            stale: false,
        };
        items.insert(id, e.clone());
        Ok(e)
    }

    pub fn remove(&self, owner: &str, id: &str) -> Result<bool, String> {
        let mut items = self.items.lock().unwrap();
        match items.get(id) {
            None => Ok(false),
            Some(e) if e.owner != owner => {
                Err(format!("сущность '{id}' принадлежит {}", e.owner))
            }
            Some(_) => {
                items.remove(id);
                Ok(true)
            }
        }
    }

    pub fn query(&self, kind: Option<&str>, owner: Option<&str>) -> Vec<Entity> {
        let items = self.items.lock().unwrap();
        let mut out: Vec<Entity> = items
            .values()
            .filter(|e| kind.is_none_or(|k| e.kind == k))
            .filter(|e| owner.is_none_or(|o| e.owner == o))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn mark_stale(&self, owner: &str) -> usize {
        let mut items = self.items.lock().unwrap();
        let mut n = 0;
        for e in items.values_mut().filter(|e| e.owner == owner) {
            e.stale = true;
            n += 1;
        }
        n
    }
```

(Если `is_none_or` недоступен в текущем MSRV — заменить на
`kind.map_or(true, |k| e.kind == k)`; проверится компиляцией.)

```rust
pub fn apply_publish(store: &EntityStore, args: &Value) -> Result<Value, String> {
    let consumer = args.get("_consumer").and_then(|v| v.as_str()).unwrap_or("");
    if !consumer.starts_with("plugin:") {
        return Err("entities.publish доступна только плагинам".into());
    }
    let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let object_id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
    match args.get("op").and_then(|v| v.as_str()).unwrap_or("upsert") {
        "upsert" => {
            let state = args.get("state").and_then(|v| v.as_str()).unwrap_or("");
            let attrs = args.get("attrs").cloned().unwrap_or(Value::Null);
            let e = store.upsert(consumer, kind, object_id, state, attrs)?;
            Ok(json!({ "entity": e }))
        }
        "remove" => {
            let removed = store.remove(consumer, &format!("{kind}.{object_id}"))?;
            Ok(json!({ "removed": removed }))
        }
        other => Err(format!("неизвестный op '{other}' (upsert|remove)")),
    }
}
```

- [ ] **Step 5: Прогнать тесты модуля — все зелёные**

Run: `cd src-tauri && cargo test entities::`
Expected: PASS, 8 тестов.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/entities.rs src-tauri/src/main.rs
git commit -m "feat(plugins): EntityStore — реестр сущностей ядра (инкремент 1, спека §6.4)"
```

---

### Task 2: Гейт инжектит личность вызывающего (`_consumer`)

**Files:**
- Modify: `src-tauri/src/capability/gate.rs`

Хендлеры capability получают `(ctx, args)` без потребителя. Для ownership-логики
(`entities.publish`) гейт добавляет в args служебный ключ `_consumer` = `consumer.id`.
Инъекция — ПОСЛЕ проверки самоэскалации (шаг 2 гейта: `touched_key` смотрит ключи args —
`_consumer` не должен ложно светиться как «изменяемый ключ» для `settings.set`), с
безусловной перезаписью (spoof-защита).

- [ ] **Step 1: Написать падающие тесты в конец `gate.rs`**

Добавить в конец файла:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::audit::MemAudit;
    use crate::capability::confirm::AutoApprove;
    use crate::capability::contract::{CapabilityMeta, Provenance};
    use crate::capability::grant::ConfirmPolicy;
    use crate::capability::registry::{make_handler, Registry};
    use serde_json::json;

    /// Реестр с одной Read-капабилити, возвращающей свои args как есть.
    fn echo_registry() -> Registry<()> {
        let mut reg = Registry::new();
        reg.register(
            CapabilityMeta {
                id: "test.echo",
                class: RiskClass::Read,
                provenance: Provenance::Trusted,
                description: "эхо аргументов (тест)",
                input_schema: json!({ "type": "object" }),
            },
            make_handler(|_: (), args| async move { Ok(args) }),
        );
        reg
    }

    #[tokio::test]
    async fn injects_consumer_identity_into_args() {
        let reg = echo_registry();
        let c = Consumer::custom("plugin:test", &[RiskClass::Read], ConfirmPolicy::Never);
        let out = invoke(
            &reg, (), &c, "test.echo", json!({ "x": 1 }),
            &AutoApprove, &MemAudit::new(), GateConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.value["_consumer"], "plugin:test");
        assert_eq!(out.value["x"], 1, "остальные args не тронуты");
    }

    #[tokio::test]
    async fn overwrites_spoofed_consumer() {
        let reg = echo_registry();
        let c = Consumer::custom("plugin:test", &[RiskClass::Read], ConfirmPolicy::Never);
        let out = invoke(
            &reg, (), &c, "test.echo", json!({ "_consumer": "panel" }),
            &AutoApprove, &MemAudit::new(), GateConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.value["_consumer"], "plugin:test", "подделка перезаписана");
    }
}
```

- [ ] **Step 2: Прогнать — убедиться, что падают**

Run: `cd src-tauri && cargo test capability::gate::`
Expected: FAIL — `out.value["_consumer"]` равен `null` (первый тест) / `"panel"` (второй).

- [ ] **Step 3: Реализовать инъекцию в `invoke`**

В `gate.rs::invoke` — два изменения.

(а) Фабрика аудита захватывает `args` по ссылке и мешает мутации ниже. Снять клон ДО
фабрики: заменить

```rust
    // фабрика записи аудита с уже известными meta
    let entry_for = |outcome: String, ms: u128| AuditEntry {
        consumer: consumer.id.clone(),
        id: meta.id.to_string(),
        class: meta.class.as_str(),
        args: args.clone(),
```

на

```rust
    // фабрика записи аудита с уже известными meta. Аргументы снимаем до инъекции
    // _consumer: в аудите потребитель и так пишется отдельным полем.
    let audit_args = args.clone();
    let entry_for = |outcome: String, ms: u128| AuditEntry {
        consumer: consumer.id.clone(),
        id: meta.id.to_string(),
        class: meta.class.as_str(),
        args: audit_args.clone(),
```

(б) Между шагом 2 (самоэскалация, блок `if meta.class == RiskClass::Settings { … }`) и
шагом 3 (`// 3. Подтверждение side-effect…`) вставить:

```rust
    // 2б. Личность вызывающего для consumer-aware капабилити (entities.publish):
    // ключ служебный, перезаписывается всегда — подделать нельзя. Инъекция после
    // проверки самоэскалации, чтобы _consumer не считался «изменяемым ключом».
    let mut args = args;
    if let Value::Object(ref mut m) = args {
        m.insert("_consumer".into(), Value::String(consumer.id.clone()));
    }
```

- [ ] **Step 4: Прогнать тесты гейта и весь capability-модуль**

Run: `cd src-tauri && cargo test capability::`
Expected: PASS — новые 2 теста зелёные, существующие тесты гейта/грантов не сломаны.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/capability/gate.rs
git commit -m "feat(capability): гейт инжектит _consumer в args — личность для consumer-aware капабилити"
```

---

### Task 3: Capability-фасад `entities.publish` / `entities.query` + поле в Daemon

**Files:**
- Create: `src-tauri/src/capability/native/entities_cap.rs`
- Modify: `src-tauri/src/capability/native/mod.rs`
- Modify: `src-tauri/src/daemon.rs` (поле + инициализация)

- [ ] **Step 1: Добавить поле store в `Daemon`**

В `src-tauri/src/daemon.rs`, в `pub struct Daemon` после поля
`pub caps: crate::capability::DaemonRegistry,` добавить:

```rust
    /// Реестр сущностей плагинов (спека plugin-system §6.4): vm.*, agent.* …
    pub entities: crate::entities::EntityStore,
```

В `Daemon::new`, в литерале `Self { … }` после `caps: crate::capability::build_registry(),`:

```rust
            entities: crate::entities::EntityStore::new(),
```

- [ ] **Step 2: Создать `src-tauri/src/capability/native/entities_cap.rs`**

```rust
//! Капабилити реестра сущностей (спека plugin-system §6.4). Тонкий фасад над
//! `entities::EntityStore`: логика в apply_publish (pure, тестируется там),
//! здесь — регистрация, эмит в панель и провенанс.
//!
//! Классы риска: обе — Read. publish = вход данных в ядро без side-effect'ов
//! на системе пользователя (confirm на каждом событии убил бы телеметрию);
//! query отдаёт данные чужих процессов → провенанс Untrusted.
//!
//! Фильтрация query по consumes-грантам читателя — инкремент 7 (спека §6.9).

use std::sync::Arc;

use serde_json::{json, Value};

use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::Daemon;

pub fn register(reg: &mut DaemonRegistry) {
    reg.register(
        CapabilityMeta {
            id: "entities.publish",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "Опубликовать (op=upsert) или убрать (op=remove) сущность в реестре ядра. Только для плагинов: владелец — личность вызывающего.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "op":    { "type": "string", "enum": ["upsert", "remove"] },
                    "kind":  { "type": "string", "description": "тип сущности, напр. vm" },
                    "id":    { "type": "string", "description": "object_id внутри kind" },
                    "state": { "type": "string" },
                    "attrs": { "type": "object" }
                },
                "required": ["kind", "id"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let out = crate::entities::apply_publish(&d.entities, &args)?;
            crate::daemon::emit_to_panel(&d.app, "entities", &d.entities.snapshot());
            Ok(out)
        }),
    );

    reg.register(
        CapabilityMeta {
            id: "entities.query",
            class: RiskClass::Read,
            provenance: Provenance::Untrusted,
            description: "Сущности реестра ядра (vm.* и др.), опционально фильтры kind/owner.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "kind":  { "type": "string" },
                    "owner": { "type": "string" }
                }
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let kind = args.get("kind").and_then(|v| v.as_str());
            let owner = args.get("owner").and_then(|v| v.as_str());
            serde_json::to_value(d.entities.query(kind, owner)).map_err(|e| e.to_string())
        }),
    );
}
```

- [ ] **Step 3: Зарегистрировать в `native/mod.rs`**

В `src-tauri/src/capability/native/mod.rs`: к списку `mod …;` добавить (по алфавиту, после
`mod control;`):

```rust
mod entities_cap;
```

В `register_all`, в блок «фаза 2 — read» после `chats::register(reg);`:

```rust
    entities_cap::register(reg); // реестр сущностей плагинов (plugin-system, инкр. 1)
```

- [ ] **Step 4: Сборка и полный прогон тестов**

Run: `cd src-tauri && cargo test`
Expected: компилируется, все тесты зелёные (входящая база + 10 новых). Если
`debug_assert` реестра ругнётся на дубликат id — проверить, что `entities_cap::register`
вызван один раз.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/capability/native/entities_cap.rs src-tauri/src/capability/native/mod.rs src-tauri/src/daemon.rs
git commit -m "feat(plugins): капабилити entities.publish/query + EntityStore в Daemon (инкремент 1)"
```

---

### Task 4: Финальная проверка инкремента

- [ ] **Step 1: Полный тестовый прогон + clippy по затронутым модулям**

Run: `cd src-tauri && cargo test && cargo clippy -- -D warnings 2>&1 | tail -5`
Expected: тесты PASS. Если clippy падает на НЕ тронутых нами файлах — игнорировать
(чинить чужое не входит в инкремент); на наших — исправить.

- [ ] **Step 2: Проверить, что публичный контракт соответствует спеке §6.4**

Чек-лист (глазами, без кода):
- `entities.publish` пишет только под owner'ом вызывающего плагина — да (apply_publish).
- Панель видит все сущности (`Consumer::panel` имеет Read без denylist) — да.
- `stale` при остановке владельца — метод `mark_stale` готов, вызов добавит PluginHost
  (инкремент 2, там же `entities.subscribe` через `/plugin/events`).
- UI-событие `entities` эмитится при каждой публикации — да (фасад).

- [ ] **Step 3: Push ветки (PR не открывать без запроса)**

```bash
git push -u origin feat/agent-vm-plugin
```
