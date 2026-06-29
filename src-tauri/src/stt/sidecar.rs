//! Супервизор STT-сайдкара: запускает venv-python с stt-server.py, держит
//! его живым (перезапуск при падении), гасит на выходе. Всё fail-safe —
//! сбой сайдкара не роняет демон, движок просто вернёт ошибку.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// RAII-страж активного использования сайдкара: пока жив хотя бы один — idle-stop
/// не срабатывает (иначе tick мог бы убить сайдкар прямо во время transcribe).
pub struct UseGuard<'a>(&'a AtomicI64);
impl Drop for UseGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct SttSidecar {
    py: PathBuf,      // {dir}/venv/bin/python
    script: PathBuf,  // {dir}/stt-server.py
    model: String,
    pub port: u16,
    child: Mutex<Option<Child>>,
    /// Должен ли сайдкар сейчас работать. true между использованием и idle-stop;
    /// false после простоя. Супервизор НЕ перезапускает сайдкар, пока active=false
    /// (иначе тут же отменил бы idle-stop). MLX-модель резидентна (~1.3 ГБ) только
    /// пока процесс жив → остановка по простою возвращает память.
    active: AtomicBool,
    /// Время последнего использования (transcribe/warm) — отсчёт простоя.
    last_use: Mutex<Instant>,
    /// Число transcribe «в полёте». idle-stop не глушит сайдкар, пока > 0.
    in_use: AtomicI64,
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
            active: AtomicBool::new(false),
            last_use: Mutex::new(Instant::now()),
            in_use: AtomicI64::new(0),
        }
    }

    /// Взять страж использования на время transcribe (см. `UseGuard`).
    pub fn use_guard(&self) -> UseGuard<'_> {
        self.in_use.fetch_add(1, Ordering::SeqCst);
        UseGuard(&self.in_use)
    }

    /// Число transcribe «в полёте» (для тестов/диагностики).
    pub fn in_use_count(&self) -> i64 {
        self.in_use.load(Ordering::SeqCst)
    }

    /// Отметить использование (сдвинуть отсчёт простоя). Зовётся на каждой
    /// диктовке/прогреве, чтобы активный сайдкар не глушился под нагрузкой.
    pub fn touch(&self) {
        if let Ok(mut t) = self.last_use.lock() {
            *t = Instant::now();
        }
    }

    /// Активен ли сайдкар (поднят и не заглушён по простою).
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
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
            // stderr → лог: краш Python (битый venv и т.п.) виден в jarvis.log, а не молча.
            .stderr(Stdio::piped());
        // huggingface.co заблокирован на части сетей (напр. RU) → тянем модель
        // через зеркало, если HF_ENDPOINT не задан явно снаружи. XET-протокол
        // отключаем — он ломал соединение (`[Errno 22]`) на этой сети.
        if std::env::var("HF_ENDPOINT").is_err() {
            cmd.env("HF_ENDPOINT", "https://hf-mirror.com");
        }
        cmd.env("HF_HUB_DISABLE_XET", "1");
        match cmd.spawn() {
            Ok(mut c) => {
                if let Some(err) = c.stderr.take() {
                    std::thread::spawn(move || {
                        use std::io::{BufRead, BufReader};
                        for line in BufReader::new(err).lines().map_while(Result::ok) {
                            if !line.trim().is_empty() {
                                crate::log::line(&format!("[stt] сайдкар stderr: {line}"));
                            }
                        }
                    });
                }
                *g = Some(c);
                crate::log::line(&format!("[stt] сайдкар запущен на :{}", self.port));
                Ok(())
            }
            Err(e) => Err(format!("[stt] сайдкар не запустился: {e}")),
        }
    }

    /// Перезапуск, если процесс умер (вызывается тиком супервизора). НО только
    /// если сайдкар должен работать (active): после idle-stop не воскрешаем —
    /// иначе простой-остановка была бы бессмысленной.
    pub fn restart_if_dead(&self) {
        if !self.active.load(Ordering::SeqCst) {
            return; // заглушён по простою — не поднимаем сами
        }
        let dead = {
            let mut g = self.child.lock().unwrap();
            // нет процесса или try_wait отдал статус завершения → мёртв
            g.as_mut().map(|c| !matches!(c.try_wait(), Ok(None))).unwrap_or(true)
        };
        if dead {
            let _ = self.ensure_started();
        }
    }

    /// Заглушить сайдкар, если он активен и простаивает дольше `limit`.
    /// Возвращает true, если остановили (для лога/метрик). Освобождает
    /// резидентную модель (~1.3 ГБ для qwen3-0.6b).
    pub fn idle_stop_if_due(&self, limit: Duration) -> bool {
        if !self.active.load(Ordering::SeqCst) {
            return false; // уже заглушён
        }
        if self.in_use.load(Ordering::SeqCst) > 0 {
            return false; // идёт transcribe — не глушим под нагрузкой (анти-гонка)
        }
        let idle = self.last_use.lock().map(|t| t.elapsed()).unwrap_or_default();
        if idle >= limit {
            self.stop();
            crate::log::line(&format!(
                "[stt] сайдкар заглушён по простою ({}с) — память модели возвращена",
                idle.as_secs()
            ));
            true
        } else {
            false
        }
    }

    /// Остановить сайдкар и снять флаг active (явная остановка/idle-stop/выход).
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

    #[test]
    fn not_installed_when_paths_missing() {
        let s = SttSidecar::new("/nope", "qwen3-0.6b", 8732);
        assert!(!s.installed());
        // не установлен → ensure_started возвращает Err, не паникует
        let r = s.ensure_started();
        assert!(r.is_err(), "ensure_started должен возвращать Err когда не установлен");
        assert_eq!(s.base(), "http://127.0.0.1:8732");
    }

    #[test]
    fn new_sidecar_is_inactive() {
        let s = SttSidecar::new("/nope", "qwen3-0.6b", 8732);
        assert!(!s.is_active(), "свежий сайдкар не активен (лениво)");
    }

    #[test]
    fn ensure_started_failure_keeps_inactive() {
        // не установлен → ensure_started падает ДО активации → active остаётся false
        let s = SttSidecar::new("/nope", "qwen3-0.6b", 8732);
        let _ = s.ensure_started();
        assert!(!s.is_active());
    }

    #[test]
    fn idle_stop_noop_when_inactive() {
        let s = SttSidecar::new("/nope", "qwen3-0.6b", 8732);
        assert!(!s.idle_stop_if_due(Duration::from_secs(0)), "неактивный сайдкар не глушим");
    }

    #[test]
    fn restart_if_dead_noop_when_inactive() {
        // после idle-stop (active=false) супервизор не воскрешает сайдкар
        let s = SttSidecar::new("/nope", "qwen3-0.6b", 8732);
        s.restart_if_dead();
        assert!(!s.is_active(), "restart_if_dead на неактивном — no-op");
    }

    #[test]
    fn use_guard_counts_in_flight() {
        let s = SttSidecar::new("/nope", "qwen3-0.6b", 8732);
        assert_eq!(s.in_use_count(), 0);
        {
            let _g1 = s.use_guard();
            assert_eq!(s.in_use_count(), 1);
            let _g2 = s.use_guard();
            assert_eq!(s.in_use_count(), 2, "вложенные transcribe считаются");
        }
        assert_eq!(s.in_use_count(), 0, "Drop стражей возвращает счётчик в 0");
    }
}
