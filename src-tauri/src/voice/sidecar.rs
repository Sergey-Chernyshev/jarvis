//! Супервизор Silero-сайдкара: запускает venv-python с silero-server.py, держит
//! его живым (перезапуск при падении), гасит на выходе. Всё fail-safe — сбой
//! сайдкара не роняет демон, движок просто вернёт ошибку.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// RAII-страж активного синтеза: пока жив — idle-stop не глушит сайдкар.
pub struct UseGuard<'a>(&'a AtomicI64);
impl Drop for UseGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Sidecar {
    py: PathBuf,     // ~/.jarvis/silero/venv/bin/python
    script: PathBuf, // ~/.jarvis/silero/silero-server.py
    speaker: String,
    model: String,
    pub port: u16,
    child: Mutex<Option<Child>>,
    /// Должен ли сайдкар работать. true между синтезом и idle-stop; false после
    /// простоя. Супервизор не воскрешает заглушённый. Озвучка частая в активные
    /// периоды, но в долгие тихие (нет агентов) — глушим и возвращаем процесс.
    active: AtomicBool,
    last_use: Mutex<Instant>,
    in_use: AtomicI64,
}

impl Sidecar {
    pub fn new(dir: PathBuf, speaker: String, model: String, port: u16) -> Self {
        Sidecar {
            py: dir.join("venv").join("bin").join("python"),
            script: dir.join("silero-server.py"),
            speaker,
            model,
            port,
            child: Mutex::new(None),
            active: AtomicBool::new(false),
            last_use: Mutex::new(Instant::now()),
            in_use: AtomicI64::new(0),
        }
    }

    /// Отметить использование (сдвинуть отсчёт простоя).
    pub fn touch(&self) {
        if let Ok(mut t) = self.last_use.lock() {
            *t = Instant::now();
        }
    }

    /// Активен ли сайдкар (поднят и не заглушён по простою). Диагностика/тесты.
    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Страж синтеза «в полёте» — idle-stop не глушит, пока он жив.
    pub fn use_guard(&self) -> UseGuard<'_> {
        self.in_use.fetch_add(1, Ordering::SeqCst);
        UseGuard(&self.in_use)
    }

    /// Число синтезов «в полёте» (для тестов/диагностики).
    #[allow(dead_code)]
    pub fn in_use_count(&self) -> i64 {
        self.in_use.load(Ordering::SeqCst)
    }

    pub fn installed(&self) -> bool {
        self.py.exists() && self.script.exists()
    }

    /// PID живого сайдкара (для метрик).
    pub fn pid(&self) -> Option<u32> {
        self.child.lock().unwrap().as_ref().map(|c| c.id())
    }

    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Запустить, если установлен и ещё не запущен. Не блокирует на загрузке
    /// модели — health-check движка сам подождёт готовности.
    pub fn ensure_started(&self) {
        if !self.installed() {
            crate::log::line("[voice] silero: сайдкар не установлен");
            return;
        }
        // намерение работать: активируем и сдвигаем отсчёт простоя
        self.active.store(true, Ordering::SeqCst);
        self.touch();
        let mut g = self.child.lock().unwrap();
        // ещё жив? try_wait → None значит процесс не завершился
        if g
            .as_mut()
            .map(|c| matches!(c.try_wait(), Ok(None)))
            .unwrap_or(false)
        {
            return;
        }
        match Command::new(&self.py)
            .arg(&self.script)
            .arg("--port")
            .arg(self.port.to_string())
            .arg("--speaker")
            .arg(&self.speaker)
            .arg("--model")
            .arg(&self.model)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // stderr → лог: чтобы краш Python (например, битый venv: ModuleNotFoundError)
            // был виден в ~/.jarvis/jarvis.log, а не терялся молча («не говорит»).
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut c) => {
                if let Some(err) = c.stderr.take() {
                    std::thread::spawn(move || {
                        use std::io::{BufRead, BufReader};
                        for line in BufReader::new(err).lines().map_while(Result::ok) {
                            if !line.trim().is_empty() {
                                crate::log::line(&format!("[voice] silero stderr: {line}"));
                            }
                        }
                    });
                }
                *g = Some(c);
                crate::log::line(&format!("[voice] silero: сайдкар запущен на :{}", self.port));
            }
            Err(e) => crate::log::line(&format!("[voice] silero: не запустился: {e}")),
        }
    }

    /// Перезапуск, если процесс умер (тик супервизора). Только если сайдкар
    /// должен работать (active): после idle-stop не воскрешаем.
    pub fn restart_if_dead(&self) {
        if !self.active.load(Ordering::SeqCst) {
            return;
        }
        let dead = {
            let mut g = self.child.lock().unwrap();
            // нет процесса или try_wait отдал статус завершения → мёртв
            g.as_mut().map(|c| !matches!(c.try_wait(), Ok(None))).unwrap_or(true)
        };
        if dead {
            self.ensure_started();
        }
    }

    /// Заглушить, если активен, не идёт синтез и простой ≥ `limit`. Возвращает
    /// процесс системе (Silero ~38МБ + питон). Озвучка возобновляется лениво.
    pub fn idle_stop_if_due(&self, limit: Duration) -> bool {
        if !self.active.load(Ordering::SeqCst) {
            return false;
        }
        if self.in_use.load(Ordering::SeqCst) > 0 {
            return false; // идёт синтез — не глушим (анти-гонка)
        }
        let idle = self.last_use.lock().map(|t| t.elapsed()).unwrap_or_default();
        if idle >= limit {
            self.stop();
            crate::log::line(&format!(
                "[voice] silero: сайдкар заглушён по простою ({}с)",
                idle.as_secs()
            ));
            true
        } else {
            false
        }
    }

    /// Остановить и снять active (явная остановка/idle-stop/выход).
    pub fn stop(&self) {
        self.active.store(false, Ordering::SeqCst);
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake() -> Sidecar {
        Sidecar::new(PathBuf::from("/nope"), "baya".into(), "v4_ru".into(), 8731)
    }

    #[test]
    fn not_installed_when_paths_missing() {
        let s = fake();
        assert!(!s.installed());
        s.ensure_started(); // не паникует — просто пишет в лог
        assert_eq!(s.base(), "http://127.0.0.1:8731");
    }

    #[test]
    fn ensure_started_failure_keeps_inactive() {
        // не установлен → ensure_started выходит ДО активации
        let s = fake();
        s.ensure_started();
        assert!(!s.is_active());
    }

    #[test]
    fn idle_stop_and_restart_noop_when_inactive() {
        let s = fake();
        assert!(!s.idle_stop_if_due(Duration::from_secs(0)), "неактивный — не глушим");
        s.restart_if_dead(); // не воскрешает заглушённый
        assert!(!s.is_active());
    }

    #[test]
    fn use_guard_counts_in_flight() {
        let s = fake();
        assert_eq!(s.in_use_count(), 0);
        {
            let _g = s.use_guard();
            assert_eq!(s.in_use_count(), 1);
        }
        assert_eq!(s.in_use_count(), 0);
    }
}
