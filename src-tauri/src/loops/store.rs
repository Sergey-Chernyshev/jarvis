//! Хранилище циклов и журналов запусков.
//!
//! Конфигурации — одним файлом `loops.json`, журналы — по файлу на цикл
//! (`loops/<id>.json`). Раздельно потому, что журнал растёт всю ночь, а
//! конфигурацию читает панель на каждый показ: держать их вместе значит
//! перечитывать мегабайты ради семи строк формы.

use super::model::{Loop, Run};
use crate::util::jarvis_dir;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn atomic_write(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

pub fn loops_path() -> PathBuf {
    jarvis_dir().join("loops.json")
}

pub fn run_path(loop_id: &str) -> PathBuf {
    jarvis_dir().join("loops").join(format!("{loop_id}.json"))
}

/// Реестр циклов в памяти с записью на диск.
///
/// Мьютекс вокруг всего: циклов единицы, а сохранение целиком после каждой
/// правки избавляет от целого класса вопросов «а что если запись частичная».
#[derive(Default)]
pub struct Store {
    items: Mutex<Vec<Loop>>,
    runs: Mutex<Vec<Run>>,
}

impl Store {
    /// Поднять с диска. Битый файл — не повод падать: циклы важны, но не
    /// настолько, чтобы из-за них не запускалось приложение.
    pub fn load() -> Self {
        let items: Vec<Loop> = std::fs::read_to_string(loops_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let runs = items.iter().filter_map(|l| load_run(&l.id)).collect();
        Self { items: Mutex::new(items), runs: Mutex::new(runs) }
    }

    pub fn all(&self) -> Vec<Loop> {
        self.items.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<Loop> {
        self.items.lock().unwrap().iter().find(|l| l.id == id).cloned()
    }

    /// Добавить или заменить по id.
    pub fn save(&self, item: Loop) {
        {
            let mut items = self.items.lock().unwrap();
            match items.iter_mut().find(|l| l.id == item.id) {
                Some(slot) => *slot = item,
                None => items.push(item),
            }
        }
        self.flush();
    }

    pub fn remove(&self, id: &str) {
        self.items.lock().unwrap().retain(|l| l.id != id);
        self.runs.lock().unwrap().retain(|r| r.loop_id != id);
        let _ = std::fs::remove_file(run_path(id));
        self.flush();
    }

    pub fn run(&self, loop_id: &str) -> Option<Run> {
        self.runs.lock().unwrap().iter().find(|r| r.loop_id == loop_id).cloned()
    }

    pub fn runs(&self) -> Vec<Run> {
        self.runs.lock().unwrap().clone()
    }

    /// Записать журнал запуска — и в память, и на диск.
    pub fn put_run(&self, run: Run) {
        {
            let mut runs = self.runs.lock().unwrap();
            match runs.iter_mut().find(|r| r.loop_id == run.loop_id) {
                Some(slot) => *slot = run.clone(),
                None => runs.push(run.clone()),
            }
        }
        if let Ok(text) = serde_json::to_string_pretty(&run) {
            let _ = atomic_write(&run_path(&run.loop_id), &text);
        }
    }

    /// Изменить журнал на месте. Возвращает изменённый — его же и рассылают в панель.
    pub fn with_run<F: FnOnce(&mut Run)>(&self, loop_id: &str, edit: F) -> Option<Run> {
        let mut run = self.run(loop_id)?;
        edit(&mut run);
        self.put_run(run.clone());
        Some(run)
    }

    fn flush(&self) {
        let items = self.items.lock().unwrap().clone();
        if let Ok(text) = serde_json::to_string_pretty(&items) {
            let _ = atomic_write(&loops_path(), &text);
        }
    }
}

fn load_run(loop_id: &str) -> Option<Run> {
    let text = std::fs::read_to_string(run_path(loop_id)).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::super::model::{Iteration, Loop, Run, RunState};
    use super::*;

    /// Каждому тесту — свой JARVIS_DIR: они бегут в одном процессе и в общий
    /// каталог писали бы друг поверх друга.
    fn scoped(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jarvis-loops-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("JARVIS_DIR", &dir);
        dir
    }

    #[test]
    fn saved_loops_survive_a_restart() {
        let dir = scoped("save");
        let store = Store::load();
        assert!(store.all().is_empty());

        store.save(Loop { id: "a".into(), name: "ночной test-fix".into(), ..Default::default() });
        store.save(Loop { id: "b".into(), name: "триаж".into(), ..Default::default() });
        // Правка не плодит дубликат.
        store.save(Loop { id: "a".into(), name: "test-fix v2".into(), ..Default::default() });

        let again = Store::load();
        assert_eq!(again.all().len(), 2);
        assert_eq!(again.get("a").unwrap().name, "test-fix v2");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_journal_is_kept_per_loop() {
        let dir = scoped("run");
        let store = Store::load();
        store.save(Loop { id: "a".into(), name: "a".into(), ..Default::default() });
        store.put_run(Run {
            loop_id: "a".into(),
            n: 1,
            state: RunState::Running,
            iterations: vec![Iteration { n: 1, ..Default::default() }],
            ..Default::default()
        });

        let again = Store::load();
        let run = again.run("a").expect("журнал поднимается вместе с циклом");
        assert_eq!(run.iterations.len(), 1);

        // Удаление цикла уносит и журнал: иначе он всплывёт у следующего цикла
        // с тем же id.
        again.remove("a");
        assert!(!run_path("a").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_file_does_not_break_startup() {
        let dir = scoped("broken");
        std::fs::write(loops_path(), "{ это не json").unwrap();
        assert!(Store::load().all().is_empty(), "битый файл не должен ронять приложение");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
