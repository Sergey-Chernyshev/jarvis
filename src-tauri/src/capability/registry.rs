//! Реестр капабилити — источник истины (§4). Универсален по контексту `C`,
//! который прокидывается хендлеру: боевой реестр — `Registry<Arc<Daemon>>`,
//! тесты ядра — `Registry<()>`. Гейт-логика от `C` не зависит, только форвардит.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use serde_json::{json, Value};

use super::contract::CapabilityMeta;
use super::grant::Grant;

pub type HandlerFut = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;
pub type Handler<C> = Box<dyn Fn(C, Value) -> HandlerFut + Send + Sync>;

pub struct Entry<C> {
    pub meta: CapabilityMeta,
    pub handler: Handler<C>,
}

pub struct Registry<C> {
    entries: HashMap<&'static str, Entry<C>>,
}

impl<C> Default for Registry<C> {
    fn default() -> Self {
        Registry { entries: HashMap::new() }
    }
}

impl<C> Registry<C> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, meta: CapabilityMeta, handler: Handler<C>) {
        debug_assert!(!self.entries.contains_key(meta.id), "дубликат капабилити: {}", meta.id);
        self.entries.insert(meta.id, Entry { meta, handler });
    }

    pub fn get(&self, id: &str) -> Option<&Entry<C>> {
        self.entries.get(id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Список метаданных, отфильтрованный грантом (для tools/list).
    pub fn list_for(&self, grant: &Grant) -> Vec<&CapabilityMeta> {
        self.entries.values().map(|e| &e.meta).filter(|m| grant.allows_id(m.id, m.class)).collect()
    }

    /// Проекция в MCP tool-определения, отфильтрованная грантом (§5).
    pub fn tools_json(&self, grant: &Grant) -> Value {
        let mut tools: Vec<Value> = self
            .list_for(grant)
            .iter()
            .map(|m| {
                json!({
                    "name": m.id,
                    "description": m.description,
                    "inputSchema": m.input_schema.clone(),
                })
            })
            .collect();
        // детерминированный порядок (HashMap не упорядочен)
        tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Value::Array(tools)
    }
}

/// Обернуть async-функцию `Fn(C, Value) -> Future<Result<Value,String>>` в
/// `Handler<C>`. Снимает шум `Box::pin` на каждой регистрации.
pub fn make_handler<C, F, Fut>(f: F) -> Handler<C>
where
    F: Fn(C, Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    Box::new(move |c, v| Box::pin(f(c, v)))
}
