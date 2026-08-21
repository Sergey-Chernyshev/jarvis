//! Файловый лог демона: ~/.jarvis/jarvis.log.
//!
//! Нужен, чтобы постфактум разбирать поведение без подключения к stdout:
//! поток событий хуков, статусы доставки/уведомлений, тайминги пайплайна.
//! НЕ пишем конфиденциальное: текст промптов/ответов агента, тело уведомлений,
//! содержимое транскриптов — только метки событий, типы и усечённые id сессий.
//! Best-effort — ошибки записи глотаем, демон от лога не зависит.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::util::jarvis_dir;
use regex::Regex;

const MAX_BYTES: u64 = 4 * 1024 * 1024; // при разрастании — ротация в .old
static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

fn log_path() -> std::path::PathBuf {
    jarvis_dir().join("jarvis.log")
}

/// Локальная метка времени ЧЧ:ММ:СС.мс (chrono уже в зависимостях).
fn stamp() -> String {
    chrono::Local::now().format("%H:%M:%S%.3f").to_string()
}

fn proxy_userinfo_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b((?:https?|socks(?:4|5h?))://)[^/\s:@]+:[^@\s/]+@")
            .expect("valid proxy credential regex")
    })
}

fn bearer_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(Bearer\s+)[A-Za-z0-9._~+/=-]+").expect("valid bearer credential regex")
    })
}

fn named_secret_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(([A-Z0-9_]*(?:API[_-]?KEY|TOKEN|PASSWORD|SECRET))\s*[:=]\s*)(?:"[^"]*"|'[^']*'|[^\s,;]+)"#,
        )
        .expect("valid named secret regex")
    })
}

fn standalone_secret_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:sk-ant-[A-Za-z0-9_-]{8,}|sk-proj-[A-Za-z0-9_-]{8,}|glpat-[A-Za-z0-9_-]{8,}|gh[pousr]_[A-Za-z0-9_]{8,})\b",
        )
        .expect("valid standalone secret regex")
    })
}

/// Последняя страховочная сетка перед stdout/файлом: скрыть распространённые
/// формы прокси-учёток и токенов, которые могли попасть в полезный текст ошибки.
fn sanitize(msg: &str) -> String {
    let safe = proxy_userinfo_re()
        .replace_all(msg, "${1}[REDACTED]@")
        .into_owned();
    let safe = bearer_re()
        .replace_all(&safe, "${1}[REDACTED]")
        .into_owned();
    let safe = named_secret_re()
        .replace_all(&safe, "${1}[REDACTED]")
        .into_owned();
    standalone_secret_re()
        .replace_all(&safe, "[REDACTED]")
        .into_owned()
}

fn open_secure_append(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

/// Одна запись за раз: ротация и сама строка — под общим замком.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Дописать готовую запись одним write(2).
///
/// `writeln!` по `File` шлёт каждый кусок формата отдельным системным вызовом,
/// и сосед вклинивался между меткой времени и текстом: в файле оставалось
/// «…сайдкар запущен на :873202:05:48.585» — конец одной строки и начало
/// другой в одной. Собираем строку целиком в буфер: один write(2) в режиме
/// O_APPEND атомарен, а замок держит порядок внутри процесса и заодно не даёт
/// двум потокам одновременно ротировать файл.
fn append(path: &std::path::Path, stamp: &str, msg: &str) {
    let _lock = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if std::fs::metadata(path)
        .map(|m| m.len() > MAX_BYTES)
        .unwrap_or(false)
    {
        let old_path = path.with_extension("log.old");
        let _ = std::fs::rename(path, &old_path);
        let _ = std::fs::set_permissions(&old_path, std::fs::Permissions::from_mode(0o600));
    }
    if let Ok(mut f) = open_secure_append(path) {
        let mut buf = String::with_capacity(stamp.len() + msg.len() + 2);
        buf.push_str(stamp);
        buf.push(' ');
        buf.push_str(msg);
        buf.push('\n');
        let _ = f.write_all(buf.as_bytes());
    }
}

/// Дописать строку в лог (и продублировать в stdout — его ловит nohup).
pub fn line(msg: &str) {
    if cfg!(test) || !ENABLED.load(Ordering::Relaxed) {
        return; // юнит-тесты не должны писать в боевой ~/.jarvis/jarvis.log
    }
    let msg = sanitize(msg);
    println!("{msg}"); // stdout → daemon.log при запуске под nohup
    let _ = std::fs::create_dir_all(jarvis_dir());
    append(&log_path(), &stamp(), &msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_redacts_proxy_userinfo() {
        let message = "connect HTTP_PROXY=http://alice:very-secret@proxy.example:8080/path";

        let safe = sanitize(message);

        assert_eq!(
            safe,
            "connect HTTP_PROXY=http://[REDACTED]@proxy.example:8080/path"
        );
        assert!(!safe.contains("alice"));
        assert!(!safe.contains("very-secret"));
    }

    #[test]
    fn sanitize_redacts_tokens_and_bearer_credentials() {
        let message = "ANTHROPIC_API_KEY=sk-ant-api-secret Authorization: Bearer abc.def-123";

        let safe = sanitize(message);

        assert!(!safe.contains("sk-ant-api-secret"));
        assert!(!safe.contains("abc.def-123"));
        assert!(safe.contains("ANTHROPIC_API_KEY=[REDACTED]"));
        assert!(safe.contains("Bearer [REDACTED]"));
    }

    #[test]
    fn secure_append_restricts_an_existing_file_to_owner_only() {
        let path = std::env::temp_dir().join(format!(
            "jarvis-log-permissions-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(&path, b"legacy\n").expect("create fixture");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("set insecure fixture mode");

        let file = open_secure_append(&path).expect("open log");
        drop(file);

        let mode = std::fs::metadata(&path)
            .expect("read fixture metadata")
            .permissions()
            .mode()
            & 0o777;
        let _ = std::fs::remove_file(&path);
        assert_eq!(mode, 0o600);
    }

    /// Лог читают глазами и грепают по времени, поэтому «две записи в одной
    /// строке» — не косметика: метка времени внутри чужого текста ломает и то,
    /// и другое.
    #[test]
    fn parallel_writers_do_not_glue_records_together() {
        const STAMP: &str = "02:05:48.585";
        const WRITERS: usize = 4;
        const EACH: usize = 200;
        let path = std::env::temp_dir().join(format!("jarvis-log-parallel-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let hands: Vec<_> = (0..WRITERS)
            .map(|w| {
                let path = path.clone();
                std::thread::spawn(move || {
                    // длинный текст: короткий влезал в один write и без замка
                    let tail = "сайдкар запущен на :8732 ".repeat(8);
                    for i in 0..EACH {
                        append(&path, STAMP, &format!("[{w}/{i}] {tail}"));
                    }
                })
            })
            .collect();
        for h in hands {
            h.join().expect("поток писателя упал");
        }

        let text = std::fs::read_to_string(&path).expect("прочитать лог");
        let _ = std::fs::remove_file(&path);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), WRITERS * EACH, "записи потерялись или удвоились");
        let glued = lines
            .iter()
            .filter(|l| !l.starts_with(STAMP) || l.matches(STAMP).count() != 1)
            .count();
        assert_eq!(glued, 0, "склеенные строки: {glued}");
    }
}

/// Замер долгого шага: пишем в лог, только если он и правда долгий.
///
/// «Зависло» без цифр неотличимо от «медленно», а два этих случая чинятся
/// по-разному. Порог такой, чтобы обычная работа молчала: всё, что человек
/// успевает заметить глазом, начинается примерно отсюда.
pub struct Step {
    what: &'static str,
    at: std::time::Instant,
}

impl Step {
    pub fn new(what: &'static str) -> Self {
        Self { what, at: std::time::Instant::now() }
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        let ms = self.at.elapsed().as_millis();
        if ms >= 300 {
            line(&format!("[slow] {} — {ms} мс", self.what));
        }
    }
}

/// Паники — в общий лог.
///
/// По умолчанию они уходят в stderr, то есть мимо файла, который человек и
/// присылает. А паника внутри асинхронной команды не просто теряется: задача
/// умирает, обещание в панели не завершается ни успехом, ни отказом, и раздел
/// висит белым навсегда. Без этой строки такое неотличимо от «просто пусто».
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let what = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "паника без описания".into());
        let at = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "?".into());
        line(&format!("[panic] {at} — {what}"));
        prev(info);
    }));
}
