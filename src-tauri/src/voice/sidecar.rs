//! Супервизор Silero-сайдкара: запускает venv-python с silero-server.py, держит
//! его живым (перезапуск при падении), гасит на выходе. Всё fail-safe — сбой
//! сайдкара не роняет демон, движок просто вернёт ошибку.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

pub struct Sidecar {
    py: PathBuf,     // ~/.jarvis/silero/venv/bin/python
    script: PathBuf, // ~/.jarvis/silero/silero-server.py
    speaker: String,
    model: String,
    pub port: u16,
    child: Mutex<Option<Child>>,
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
        }
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
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => {
                *g = Some(c);
                crate::log::line(&format!("[voice] silero: сайдкар запущен на :{}", self.port));
            }
            Err(e) => crate::log::line(&format!("[voice] silero: не запустился: {e}")),
        }
    }

    /// Перезапуск, если процесс умер (вызывается тиком супервизора).
    pub fn restart_if_dead(&self) {
        let dead = {
            let mut g = self.child.lock().unwrap();
            // нет процесса или try_wait отдал статус завершения → мёртв
            g.as_mut().map(|c| !matches!(c.try_wait(), Ok(None))).unwrap_or(true)
        };
        if dead {
            self.ensure_started();
        }
    }

    pub fn stop(&self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_installed_when_paths_missing() {
        let s = Sidecar::new(PathBuf::from("/nope"), "baya".into(), "v4_ru".into(), 8731);
        assert!(!s.installed());
        s.ensure_started(); // не паникует — просто пишет в лог
        assert_eq!(s.base(), "http://127.0.0.1:8731");
    }
}
