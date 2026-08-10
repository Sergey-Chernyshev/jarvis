//! Связка — «в 10 рук» над одним проектом.
//!
//! Рука — обычный чат: человек пишет ей первое сообщение, а ветка и worktree
//! создаются сами. Дальше связка живёт тактом: следит за руками через их же
//! сессии (хуки приносят cwd — по нему рука и узнаётся), перебазирует готовых
//! на свежую базу, гоняет гейты и строит очередь слияний. Кнопка «влить» —
//! только у человека: автоматике принадлежит подготовка, не решение.
//!
//! Конфликт при ребейзе чинит сам агент руки: мы откатываем ребейз, отдаём
//! ему список файлов сообщением в его же чат и ждём. Он знает контекст своих
//! правок — мы нет.

pub mod git;
pub mod host;
pub mod ipc;
pub mod launch;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Где рука сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum HandState {
    /// Создана, но агент ещё не поднят.
    #[default]
    New,
    /// Агент работает над задачей.
    Working,
    /// Готова: перебазирована, гейты зелёные, стоит в очереди.
    Ready,
    /// Выпала из очереди: конфликт при ребейзе, агент чинит.
    Conflict,
    /// Влита в базу.
    Merged,
    /// Не поднялась: причина в событии.
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Hand {
    pub id: String,
    pub name: String,
    /// Задача — она же первое сообщение чата.
    pub task: String,
    pub branch: String,
    pub worktree: String,
    /// tmux-пана руки: через неё уходят сообщения и прерывание.
    pub pane: String,
    pub state: HandState,
    /// Когда встала в очередь: порядок очереди — это порядок готовности.
    pub ready_at: i64,
    /// Голова, на которой гейты гонялись в последний раз: чтобы не гонять
    /// одно и то же.
    pub checked_sha: String,
    pub gates_ok: bool,
    /// Попытка починки конфликта: по ней видно «чинит сам, попытка 2».
    pub attempt: u32,
    pub conflict_files: Vec<String>,
    /// Файлы, которых рука коснулась, — кэш такта для «горячих файлов».
    pub touched: Vec<String>,
    pub merged_at: i64,
}

/// Событие ленты связки.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Event {
    pub at: i64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Bundle {
    pub id: String,
    pub name: String,
    /// Где связка живёт: `local` — эта машина, иначе имя узла из настроек.
    pub machine: String,
    /// Директория проекта. Как в «Проектах»: git она не обязана быть — нет
    /// `.git` или самого каталога, связка инициализирует и создаст сама.
    #[serde(alias = "repo")]
    pub dir: String,
    /// Базовая ветка (main/master) — в неё едет очередь.
    pub base: String,
    pub gates: Vec<crate::loops::model::Gate>,
    /// Ориентир расхода на руку, токены. Показывается, не принуждает.
    pub budget_tokens: u64,
    pub paused: bool,
    pub hands: Vec<Hand>,
    pub events: Vec<Event>,
    pub created_at: i64,
    pub last_merge_at: i64,
}

impl Bundle {
    /// Чего не хватает для старта.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.name.trim().is_empty() {
            out.push("у связки нет имени".into());
        }
        if self.dir.trim().is_empty() {
            out.push("не указана директория".into());
        }
        if self.hands.iter().all(|h| h.task.trim().is_empty()) {
            out.push("ни у одной руки нет задачи".into());
        }
        out
    }

    /// Очередь слияний: готовые руки в порядке готовности.
    pub fn queue(&self) -> Vec<&Hand> {
        let mut q: Vec<&Hand> = self.hands.iter().filter(|h| h.state == HandState::Ready).collect();
        q.sort_by_key(|h| h.ready_at);
        q
    }

    pub fn event(&mut self, text: impl Into<String>) {
        self.events.push(Event { at: crate::util::now_ms(), text: text.into() });
        let extra = self.events.len().saturating_sub(50);
        if extra > 0 {
            self.events.drain(..extra);
        }
    }

    /// Связка ещё живёт: есть кому работать или вливаться.
    pub fn active(&self) -> bool {
        self.hands.iter().any(|h| {
            matches!(h.state, HandState::Working | HandState::Ready | HandState::Conflict)
        })
    }
}

/// Имя руки из задачи, если человек не назвал сам: первые осмысленные слова.
pub fn hand_name(task: &str) -> String {
    let words: Vec<&str> = task.split_whitespace().take(3).collect();
    let joined = words.join(" ");
    crate::util::ellipsize(&joined, 24)
}

/* ================= хранилище ================= */

fn bundles_path(root: &Path) -> PathBuf {
    root.join("bundles.json")
}

fn atomic_write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Реестр связок: как у циклов — целиком в памяти, целиком на диск.
pub struct Store {
    root: PathBuf,
    items: Mutex<Vec<Bundle>>,
}

impl Store {
    pub fn load() -> Self {
        Self::load_at(crate::util::jarvis_dir())
    }

    pub fn load_at(root: PathBuf) -> Self {
        let items: Vec<Bundle> = std::fs::read_to_string(bundles_path(&root))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self { root, items: Mutex::new(items) }
    }

    pub fn all(&self) -> Vec<Bundle> {
        self.items.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn get(&self, id: &str) -> Option<Bundle> {
        self.all().into_iter().find(|b| b.id == id)
    }

    pub fn save(&self, item: Bundle) {
        {
            let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
            match items.iter_mut().find(|b| b.id == item.id) {
                Some(slot) => *slot = item,
                None => items.push(item),
            }
        }
        self.flush();
    }

    /// Изменить связку на месте; вернуть изменённую.
    pub fn with<F: FnOnce(&mut Bundle)>(&self, id: &str, edit: F) -> Option<Bundle> {
        let out = {
            let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
            let slot = items.iter_mut().find(|b| b.id == id)?;
            edit(slot);
            Some(slot.clone())
        };
        self.flush();
        out
    }

    pub fn remove(&self, id: &str) {
        self.items.lock().unwrap_or_else(|e| e.into_inner()).retain(|b| b.id != id);
        self.flush();
    }

    fn flush(&self) {
        let items = self.items.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Ok(text) = serde_json::to_string_pretty(&items) {
            let _ = atomic_write(&bundles_path(&self.root), &text);
        }
    }
}

/// Всё про связки, что живёт в демоне.
pub struct Bundles {
    pub store: Store,
    /// Такт — по одному за раз: он ходит в git и гоняет гейты.
    ticking: AtomicBool,
}

impl Default for Bundles {
    fn default() -> Self {
        Self::new()
    }
}

impl Bundles {
    pub fn new() -> Self {
        Self { store: Store::load(), ticking: AtomicBool::new(false) }
    }

    pub fn claim_tick(&self) -> bool {
        !self.ticking.swap(true, Ordering::SeqCst)
    }

    pub fn release_tick(&self) {
        self.ticking.store(false, Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hand(id: &str, state: HandState, ready_at: i64) -> Hand {
        Hand { id: id.into(), state, ready_at, ..Default::default() }
    }

    #[test]
    fn queue_is_ordered_by_readiness_not_by_list_position() {
        let b = Bundle {
            hands: vec![
                hand("a", HandState::Working, 0),
                hand("b", HandState::Ready, 200),
                hand("c", HandState::Ready, 100),
                hand("d", HandState::Conflict, 50),
                hand("e", HandState::Merged, 10),
            ],
            ..Default::default()
        };
        let q: Vec<&str> = b.queue().iter().map(|h| h.id.as_str()).collect();
        // Конфликтная выпала из очереди, влитая и работающая в ней не стоят.
        assert_eq!(q, vec!["c", "b"]);
    }

    #[test]
    fn problems_name_the_gaps() {
        let empty = Bundle::default();
        let p = empty.problems();
        assert!(p.iter().any(|x| x.contains("имени")));
        assert!(p.iter().any(|x| x.contains("директория")));
        assert!(p.iter().any(|x| x.contains("задачи")));

        let ok = Bundle {
            name: "клевер-релиз".into(),
            dir: "/repo".into(),
            hands: vec![Hand { task: "экран логина".into(), ..Default::default() }],
            ..Default::default()
        };
        assert!(ok.problems().is_empty());
    }

    #[test]
    fn events_are_capped() {
        let mut b = Bundle::default();
        for i in 0..80 {
            b.event(format!("событие {i}"));
        }
        assert_eq!(b.events.len(), 50);
        assert!(b.events[0].text.contains("30"), "старые уходят первыми");
    }

    #[test]
    fn hand_name_comes_from_the_task() {
        let long = hand_name("Экран логина, magic-link и сессии в keychain");
        assert!(long.starts_with("Экран логина"), "{long}");
        assert!(long.chars().count() <= 24, "{long}");
        assert_eq!(hand_name("доки"), "доки");
    }

    #[test]
    fn store_survives_restart() {
        let dir = std::env::temp_dir().join(format!("jarvis-bundles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::load_at(dir.clone());
        store.save(Bundle { id: "b1".into(), name: "клевер-релиз".into(), ..Default::default() });
        store.with("b1", |b| b.event("рука запущена"));

        let again = Store::load_at(dir.clone());
        let b = again.get("b1").unwrap();
        assert_eq!(b.name, "клевер-релиз");
        assert_eq!(b.events.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod compat_tests {
    use super::*;

    /// Старые bundles.json звали директорию «repo» — они обязаны читаться.
    #[test]
    fn old_files_with_repo_field_still_load() {
        let b: Bundle = serde_json::from_str(r#"{ "id": "b1", "name": "x", "repo": "/старый/путь" }"#).unwrap();
        assert_eq!(b.dir, "/старый/путь");
        assert_eq!(b.machine, "");
    }
}
