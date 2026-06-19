//! Супервизор STT-сайдкара: запускает venv-python с stt-server.py, держит
//! его живым (перезапуск при падении), гасит на выходе. Всё fail-safe —
//! сбой сайдкара не роняет демон, движок просто вернёт ошибку.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

pub struct SttSidecar {
    py: PathBuf,      // {dir}/venv/bin/python
    script: PathBuf,  // {dir}/stt-server.py
    model: String,
    pub port: u16,
    child: Mutex<Option<Child>>,
}

impl SttSidecar {
    pub fn new(dir: &str, model: &str, port: u16) -> Self {
        let base = PathBuf::from(dir);
        SttSidecar {
            py: base.join("venv").join("bin").join("python"),
            script: base.join("stt-server.py"),
            model: model.to_string(),
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
    pub fn ensure_started(&self) -> Result<(), String> {
        if !self.installed() {
            return Err(format!(
                "[stt] сайдкар не установлен (py={:?}, script={:?})",
                self.py, self.script
            ));
        }
        let mut g = self.child.lock().unwrap();
        // ещё жив? try_wait → None значит процесс не завершился
        if g
            .as_mut()
            .map(|c| matches!(c.try_wait(), Ok(None)))
            .unwrap_or(false)
        {
            return Ok(());
        }
        let mut cmd = Command::new(&self.py);
        cmd.arg(&self.script)
            .arg("--port")
            .arg(self.port.to_string())
            .arg("--model")
            .arg(&self.model)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // huggingface.co заблокирован на части сетей (напр. RU) → тянем модель
        // через зеркало, если HF_ENDPOINT не задан явно снаружи. XET-протокол
        // отключаем — он ломал соединение (`[Errno 22]`) на этой сети.
        if std::env::var("HF_ENDPOINT").is_err() {
            cmd.env("HF_ENDPOINT", "https://hf-mirror.com");
        }
        cmd.env("HF_HUB_DISABLE_XET", "1");
        match cmd.spawn() {
            Ok(c) => {
                *g = Some(c);
                crate::log::line(&format!("[stt] сайдкар запущен на :{}", self.port));
                Ok(())
            }
            Err(e) => Err(format!("[stt] сайдкар не запустился: {e}")),
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
            let _ = self.ensure_started();
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
        let s = SttSidecar::new("/nope", "qwen3-0.6b", 8732);
        assert!(!s.installed());
        // не установлен → ensure_started возвращает Err, не паникует
        let r = s.ensure_started();
        assert!(r.is_err(), "ensure_started должен возвращать Err когда не установлен");
        assert_eq!(s.base(), "http://127.0.0.1:8732");
    }
}
