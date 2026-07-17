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

    /// Удалить свою сущность. Ok(false) — не было; ошибка — чужой владелец.
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

    /// Сущности с фильтрами по kind/owner, отсортированы по id (детерминизм).
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

    /// Пометить все сущности владельца как stale (плагин остановлен).
    /// Возвращает число помеченных. Живой upsert снимает пометку.
    pub fn mark_stale(&self, owner: &str) -> usize {
        let mut items = self.items.lock().unwrap();
        let mut n = 0;
        for e in items.values_mut().filter(|e| e.owner == owner) {
            e.stale = true;
            n += 1;
        }
        n
    }

    pub fn snapshot(&self) -> Vec<Entity> {
        self.query(None, None)
    }
}

/// Pure-логика capability `entities.publish`: разбор args + мутация store.
/// Владелец берётся ТОЛЬКО из `_consumer` (инжектится гейтом, не подделать);
/// доступно только плагинам. Вынесено из хендлера ради юнит-тестов без Daemon.
pub fn apply_publish(store: &EntityStore, args: &Value) -> Result<Value, String> {
    let consumer = args.get("_consumer").and_then(|v| v.as_str()).unwrap_or("");
    if !consumer.starts_with("plugin:") {
        return Err("entities.publish доступна только плагинам".into());
    }
    let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let object_id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if kind.is_empty() || object_id.is_empty() {
        return Err("kind и id обязательны".into());
    }
    if kind.contains('.') {
        return Err(format!("kind не может содержать точку: '{kind}'"));
    }
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
    fn publish_remove_requires_kind_and_id() {
        let s = EntityStore::new();
        s.upsert("plugin:avm", "vm", "my-api", "on", json!({})).unwrap();
        let err = apply_publish(&s, &json!({"_consumer": "plugin:avm", "op": "remove", "id": "my-api"}))
            .unwrap_err();
        assert!(err.contains("обязательны"), "{err}");
        assert_eq!(s.query(None, None).len(), 1, "сущность жива");
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
