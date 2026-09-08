//! Хранилище циклов и журналов запусков.
//!
//! Конфигурации — одним файлом `loops.json`, журналы — по файлу на цикл
//! (`loops/<id>.json`). Раздельно потому, что журнал растёт всю ночь, а
//! конфигурацию читает панель на каждый показ: держать их вместе значит
//! перечитывать мегабайты ради семи строк формы.

use super::model::{Loop, Run, RunState, StopReason};
use crate::util::jarvis_dir;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn atomic_write(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::stt::transcripts::write_private_atomic(path, text.as_bytes())
}

fn loops_path(root: &Path) -> PathBuf {
    root.join("loops.json")
}

fn run_path(root: &Path, loop_id: &str) -> PathBuf {
    root.join("loops").join(format!("{loop_id}.json"))
}

/// Реестр циклов в памяти с записью на диск.
///
/// Мьютекс вокруг всего: циклов единицы, а сохранение целиком после каждой
/// правки избавляет от целого класса вопросов «а что если запись частичная».
pub struct Store {
    /// Каталог данных. Хранится полем, а не берётся из окружения на каждый
    /// вызов: `JARVIS_DIR` — процессно-глобальная переменная, и подмена её в
    /// тестах ломает соседние тесты, которые в этот момент читают диск.
    root: PathBuf,
    items: Mutex<Vec<Loop>>,
    runs: Mutex<Vec<Run>>,
    changed: tokio::sync::watch::Sender<u64>,
}

impl Store {
    /// Поднять с диска. Битый файл — не повод падать: циклы важны, но не
    /// настолько, чтобы из-за них не запускалось приложение.
    pub fn load() -> Self {
        Self::load_at(jarvis_dir())
    }

    pub fn load_at(root: PathBuf) -> Self {
        let items: Vec<Loop> = std::fs::read_to_string(loops_path(&root))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let runs = items
            .iter()
            .filter_map(|l| load_run(&root, &l.id))
            .map(|mut run| {
                if run.state == RunState::Running {
                    run.state = RunState::Stopped;
                    run.stop = StopReason::Failed;
                    run.stop_note =
                        "приложение закрылось во время запуска; работа сохранена в песочнице"
                            .into();
                    run.ended_at = crate::util::now_ms();
                }
                run
            })
            .collect();
        let (changed, _) = tokio::sync::watch::channel(0);
        Self {
            root,
            items: Mutex::new(items),
            runs: Mutex::new(runs),
            changed,
        }
    }

    pub fn all(&self) -> Vec<Loop> {
        self.items.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<Loop> {
        self.items
            .lock()
            .unwrap()
            .iter()
            .find(|l| l.id == id)
            .cloned()
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
        let _ = std::fs::remove_file(run_path(&self.root, id));
        self.flush();
        self.notify();
    }

    pub fn run(&self, loop_id: &str) -> Option<Run> {
        self.runs
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.loop_id == loop_id)
            .cloned()
    }

    pub fn runs(&self) -> Vec<Run> {
        self.runs.lock().unwrap().clone()
    }

    /// Записать журнал запуска — и в память, и на диск.
    pub fn put_run(&self, mut run: Run) {
        let items = self.items.lock().unwrap();
        if !items.iter().any(|item| item.id == run.loop_id) {
            return;
        }
        let mut runs = self.runs.lock().unwrap();
        match runs.iter_mut().find(|r| r.loop_id == run.loop_id) {
            Some(slot) => {
                if slot.n > run.n
                    || (slot.n == run.n
                        && slot.state == RunState::Stopped
                        && slot.stop == StopReason::Stopped)
                {
                    return;
                }
                if slot.n == run.n {
                    run.interventions = slot.interventions.clone();
                    for old in slot.iterations.iter().filter(|it| it.reviewed) {
                        if let Some(it) = run.iterations.iter_mut().find(|it| it.n == old.n) {
                            it.reviewed = true;
                            if old.verdict == super::model::Verdict::Returned {
                                let changed = it.verdict != old.verdict || it.critic != old.critic;
                                it.verdict = old.verdict;
                                it.critic = old.critic.clone();
                                if changed {
                                    run.streak = 0;
                                }
                            }
                        }
                    }
                }
                *slot = run.clone();
            }
            None => runs.push(run.clone()),
        }
        if let Ok(text) = serde_json::to_string_pretty(&run) {
            let _ = atomic_write(&run_path(&self.root, &run.loop_id), &text);
        }
        drop(runs);
        drop(items);
        self.notify();
    }

    /// Изменить журнал на месте. Возвращает изменённый — его же и рассылают в панель.
    pub fn with_run<F: FnOnce(&mut Run)>(&self, loop_id: &str, edit: F) -> Option<Run> {
        let mut runs = self.runs.lock().unwrap();
        let run = runs.iter_mut().find(|r| r.loop_id == loop_id)?;
        edit(run);
        if let Ok(text) = serde_json::to_string_pretty(run) {
            let _ = atomic_write(&run_path(&self.root, loop_id), &text);
        }
        let result = run.clone();
        drop(runs);
        self.notify();
        Some(result)
    }

    fn flush(&self) {
        let items = self.items.lock().unwrap();
        if let Ok(text) = serde_json::to_string_pretty(&*items) {
            let _ = atomic_write(&loops_path(&self.root), &text);
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn notify(&self) {
        self.changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    pub fn take_interventions(&self, id: &str) -> Vec<String> {
        let mut taken = Vec::new();
        self.with_run(id, |run| taken = std::mem::take(&mut run.interventions));
        taken
    }

    pub async fn cancelled(&self, id: &str, n: u32) {
        let mut changed = self.changed.subscribe();
        loop {
            let cancelled = self.run(id).map_or(true, |run| {
                run.n != n || (run.state == RunState::Stopped && run.stop == StopReason::Stopped)
            });
            if cancelled {
                return;
            }
            if changed.changed().await.is_err() {
                return;
            }
        }
    }
}

fn load_run(root: &Path, loop_id: &str) -> Option<Run> {
    let text = std::fs::read_to_string(run_path(root, loop_id)).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::super::model::{Iteration, Loop, Run, RunState};
    use super::*;

    /// Каталог на тест. Никакого `JARVIS_DIR`: он процессно-глобальный, и
    /// подмена его здесь роняла бы соседние тесты, читающие диск в тот же миг.
    fn scoped(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jarvis-loops-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn stale_worker_cannot_undo_stop_review_intervention_or_removal() {
        let dir = scoped("stale");
        let store = Store::load_at(dir.clone());
        store.save(Loop {
            id: "a".into(),
            ..Default::default()
        });
        let stale = Run {
            loop_id: "a".into(),
            n: 1,
            state: RunState::Running,
            iterations: vec![Iteration {
                n: 1,
                ..Default::default()
            }],
            ..Default::default()
        };
        store.put_run(stale.clone());
        store.with_run("a", |r| {
            r.interventions.push("new instruction".into());
            r.iterations[0].reviewed = true;
            r.iterations[0].verdict = super::super::model::Verdict::Returned;
            r.iterations[0].critic = "human review".into();
        });
        store.put_run(stale.clone());
        let live = store.run("a").unwrap();
        assert!(live.iterations[0].reviewed);
        assert_eq!(live.iterations[0].critic, "human review");
        assert_eq!(store.take_interventions("a"), vec!["new instruction"]);
        assert!(store.take_interventions("a").is_empty());
        store.with_run("a", |r| {
            r.state = RunState::Stopped;
            r.stop = StopReason::Stopped;
        });
        store.put_run(stale.clone());
        assert_eq!(store.run("a").unwrap().stop, StopReason::Stopped);
        store.remove("a");
        store.put_run(stale);
        assert!(store.run("a").is_none());
        assert!(!run_path(&dir, "a").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saved_loops_survive_a_restart() {
        let dir = scoped("save");
        let store = Store::load_at(dir.clone());
        assert!(store.all().is_empty());

        store.save(Loop {
            id: "a".into(),
            name: "ночной test-fix".into(),
            ..Default::default()
        });
        store.save(Loop {
            id: "b".into(),
            name: "триаж".into(),
            ..Default::default()
        });
        // Правка не плодит дубликат.
        store.save(Loop {
            id: "a".into(),
            name: "test-fix v2".into(),
            ..Default::default()
        });

        let again = Store::load_at(dir.clone());
        assert_eq!(again.all().len(), 2);
        assert_eq!(again.get("a").unwrap().name, "test-fix v2");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_journal_is_kept_per_loop() {
        let dir = scoped("run");
        let store = Store::load_at(dir.clone());
        store.save(Loop {
            id: "a".into(),
            name: "a".into(),
            ..Default::default()
        });
        store.put_run(Run {
            loop_id: "a".into(),
            n: 1,
            state: RunState::Running,
            iterations: vec![Iteration {
                n: 1,
                ..Default::default()
            }],
            ..Default::default()
        });

        let again = Store::load_at(dir.clone());
        let run = again.run("a").expect("журнал поднимается вместе с циклом");
        assert_eq!(run.iterations.len(), 1);

        // Удаление цикла уносит и журнал: иначе он всплывёт у следующего цикла
        // с тем же id.
        again.remove("a");
        assert!(!run_path(&dir, "a").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_file_does_not_break_startup() {
        let dir = scoped("broken");
        std::fs::write(loops_path(&dir), "{ это не json").unwrap();
        assert!(
            Store::load_at(dir.clone()).all().is_empty(),
            "битый файл не должен ронять приложение"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
