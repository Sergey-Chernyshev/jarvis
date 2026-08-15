//! OS-control скилы голосового ассистента: медиа, системная громкость, запуск
//! приложений. Это benign-reversible действия — исполняются БЕЗ confirm (ярус
//! «Free» из плана), но с жёсткой валидацией аргументов (анти-инъекция с
//! недоверенного микрофона: имя приложения без shell/путей; громкость клампится).
//!
//! Чистые функции (нормализация действия, коды команд, валидация имени, разбор и
//! кламп громкости, сборка AppleScript) юнит-тестируемы без процессов; exec —
//! тонкие обёртки поверх `std::process::Command` (НИКОГДА не через shell).

use serde_json::Value;

// ── Медиа ──────────────────────────────────────────────────────────────────

/// Нормализованное медиа-действие (синонимы речи → канон).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaAction {
    Play,
    Pause,
    Toggle,
    Next,
    Prev,
}

/// Распознать медиа-действие из (возможно русского) слова. None — не медиа.
pub fn parse_media_action(action: &str) -> Option<MediaAction> {
    match action.trim().to_lowercase().as_str() {
        "play" | "играй" | "продолжи" | "воспроизведи" | "включи" => Some(MediaAction::Play),
        "pause" | "пауза" | "паузу" | "останови" | "стоп" => Some(MediaAction::Pause),
        "toggle" | "playpause" | "переключи" => Some(MediaAction::Toggle),
        "next" | "skip" | "следующий" | "дальше" | "вперёд" | "вперед" => Some(MediaAction::Next),
        "prev" | "previous" | "back" | "предыдущий" | "назад" => Some(MediaAction::Prev),
        _ => None,
    }
}

/// Исполнить медиа-действие через системный now-playing (любой источник).
pub fn run_media(action: &str) -> Result<(), String> {
    let a = parse_media_action(action).ok_or_else(|| format!("неизвестное медиа-действие: {action}"))?;
    match a {
        MediaAction::Play => crate::platform::media_play(),
        MediaAction::Pause => crate::platform::media_pause(),
        MediaAction::Toggle => crate::platform::media_toggle(),
        MediaAction::Next => crate::platform::media_next(),
        MediaAction::Prev => crate::platform::media_prev(),
    }
    Ok(())
}

// ── Запуск приложений ──────────────────────────────────────────────────────

/// Валидировать имя приложения для `open -a <name>`. Разрешены буквы (в т.ч.
/// Unicode), цифры, пробел и `. - & + _`. ЗАПРЕЩЕНЫ `/` (пути), control-символы,
/// shell-метасимволы — даже несмотря на то, что exec идёт без shell (defense-in-
/// depth против запуска произвольного бинаря по пути или сюрпризов). Длина ≤ 64.
pub fn validate_app_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("пустое имя приложения".into());
    }
    if n.chars().count() > 64 {
        return Err("слишком длинное имя приложения".into());
    }
    let ok = n.chars().all(|c| {
        c.is_alphanumeric() || matches!(c, ' ' | '.' | '-' | '&' | '+' | '_')
    });
    if !ok {
        return Err(format!("недопустимое имя приложения: {name}"));
    }
    Ok(())
}

/// Запустить приложение по имени (без shell).
///
/// macOS ищет бандл через `open -a`. На Linux бандлов нет: сначала пробуем
/// `.desktop`-запись через `gio launch` (так приложение стартует правильно —
/// с иконкой, в своей сессии), потом просто бинарь из PATH.
pub fn run_open_app(name: &str) -> Result<(), String> {
    validate_app_name(name)?;
    imp::open_app(name.trim())
}

#[cfg(target_os = "macos")]
mod imp {
    use std::process::{Command, Stdio};

    pub(super) fn open_app(name: &str) -> Result<(), String> {
        let status = Command::new("open")
            .arg("-a")
            .arg(name)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("не удалось запустить open: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("приложение не найдено: {name}"))
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::process::{Command, Stdio};

    fn spawn_ok(cmd: &str, args: &[&str]) -> bool {
        Command::new(cmd)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    }

    pub(super) fn open_app(name: &str) -> Result<(), String> {
        // 1) .desktop-запись: firefox → firefox.desktop, «Files» → org.gnome.Nautilus…
        //    Точное имя файла угадать нельзя, поэтому пробуем самый частый вид.
        let lower = name.to_lowercase().replace(' ', "-");
        let desktop = format!("{lower}.desktop");
        if spawn_ok("gio", &["launch", &desktop]) {
            return Ok(());
        }
        // 2) просто исполняемый файл из PATH
        if spawn_ok(&lower, &[]) || spawn_ok(name, &[]) {
            return Ok(());
        }
        Err(format!("приложение не найдено: {name}"))
    }
}

// ── Системная громкость ────────────────────────────────────────────────────

/// Операция над системной громкостью (разобранная из args).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeOp {
    /// Установить абсолютный уровень 0..100.
    Set(u8),
    /// Сдвиг относительно текущего (±), резолвится в exec чтением текущего.
    Delta(i64),
    /// Заглушить / снять заглушку.
    Mute(bool),
}

/// Кламп уровня громкости в 0..=100.
pub fn clamp_volume(v: i64) -> u8 {
    v.clamp(0, 100) as u8
}

/// Разобрать операцию громкости из args планировщика. Принимает:
/// `{"set":N}` (0..100), `{"delta":±N}`, `{"mute":true|false}`,
/// плюс речевые `{"action":"mute|unmute|up|down|louder|quieter"}`.
pub fn parse_volume_op(args: &Value) -> Result<VolumeOp, String> {
    if let Some(n) = args.get("set").and_then(Value::as_i64) {
        return Ok(VolumeOp::Set(clamp_volume(n)));
    }
    if let Some(d) = args.get("delta").and_then(Value::as_i64) {
        if d == 0 {
            return Err("нулевой сдвиг громкости".into());
        }
        return Ok(VolumeOp::Delta(d.clamp(-100, 100)));
    }
    if let Some(m) = args.get("mute").and_then(Value::as_bool) {
        return Ok(VolumeOp::Mute(m));
    }
    match args.get("action").and_then(Value::as_str).map(|s| s.to_lowercase()) {
        Some(a) => match a.as_str() {
            "mute" | "заглуши" | "выключи" => Ok(VolumeOp::Mute(true)),
            "unmute" | "включи" => Ok(VolumeOp::Mute(false)),
            "up" | "louder" | "громче" => Ok(VolumeOp::Delta(12)),
            "down" | "quieter" | "тише" => Ok(VolumeOp::Delta(-12)),
            other => Err(format!("неизвестная команда громкости: {other}")),
        },
        None => Err("нужен set/delta/mute/action".into()),
    }
}

/// AppleScript-выражение для абсолютного уровня (0..100) — число уже валидно.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn volume_set_script(level: u8) -> String {
    format!("set volume output volume {level}")
}

/// AppleScript для mute/unmute.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn volume_mute_script(muted: bool) -> String {
    if muted {
        "set volume with output muted".to_string()
    } else {
        "set volume without output muted".to_string()
    }
}

/// Исполнить операцию над системной громкостью.
///
/// Скрипты AppleScript (`volume_set_script`/`volume_mute_script`) остаются
/// общими — они же покрыты тестами; на Linux вместо них зовётся PipeWire/Pulse
/// через `pactl`, с откатом на ALSA `amixer`.
pub fn run_volume(args: &Value) -> Result<(), String> {
    match parse_volume_op(args)? {
        VolumeOp::Set(level) => vol::set(level),
        VolumeOp::Mute(m) => vol::mute(m),
        VolumeOp::Delta(d) => {
            let cur = vol::current().unwrap_or(50);
            vol::set(clamp_volume(cur as i64 + d))
        }
    }
}

#[cfg(target_os = "macos")]
mod vol {
    use super::{clamp_volume, volume_mute_script, volume_set_script};
    use std::process::{Command, Stdio};

    /// Применить AppleScript-команду (без shell).
    fn run_osascript(script: &str) -> Result<(), String> {
        let status = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("osascript: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err("не удалось изменить громкость".into())
        }
    }

    pub(super) fn set(level: u8) -> Result<(), String> {
        run_osascript(&volume_set_script(level))
    }
    pub(super) fn mute(m: bool) -> Result<(), String> {
        run_osascript(&volume_mute_script(m))
    }
    pub(super) fn current() -> Option<u8> {
        let out = Command::new("osascript")
            .arg("-e")
            .arg("output volume of (get volume settings)")
            .output()
            .ok()?;
        out.status.success().then_some(())?;
        String::from_utf8_lossy(&out.stdout).trim().parse::<i64>().ok().map(clamp_volume)
    }
}

#[cfg(not(target_os = "macos"))]
mod vol {
    use super::clamp_volume;
    use std::process::{Command, Stdio};

    const SINK: &str = "@DEFAULT_SINK@";

    fn run(cmd: &str, args: &[&str]) -> bool {
        Command::new(cmd)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    pub(super) fn set(level: u8) -> Result<(), String> {
        let pct = format!("{level}%");
        if run("pactl", &["set-sink-volume", SINK, &pct]) {
            return Ok(());
        }
        if run("amixer", &["-q", "sset", "Master", &pct]) {
            return Ok(());
        }
        Err("не удалось изменить громкость (нужен pactl или amixer)".into())
    }

    pub(super) fn mute(m: bool) -> Result<(), String> {
        let on = if m { "1" } else { "0" };
        if run("pactl", &["set-sink-mute", SINK, on]) {
            return Ok(());
        }
        if run("amixer", &["-q", "sset", "Master", if m { "mute" } else { "unmute" }]) {
            return Ok(());
        }
        Err("не удалось изменить громкость (нужен pactl или amixer)".into())
    }

    /// `pactl get-sink-volume` печатает вид «Volume: front-left: 45875 /  70% / …»
    /// — берём первый процент.
    pub(super) fn current() -> Option<u8> {
        let out = Command::new("pactl")
            .args(["get-sink-volume", SINK])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status.success().then_some(())?;
        let text = String::from_utf8_lossy(&out.stdout);
        let pct = text.split('%').next()?.rsplit(char::is_whitespace).next()?;
        pct.parse::<i64>().ok().map(clamp_volume)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_media_synonyms() {
        assert_eq!(parse_media_action("pause"), Some(MediaAction::Pause));
        assert_eq!(parse_media_action("Паузу"), Some(MediaAction::Pause));
        assert_eq!(parse_media_action("следующий"), Some(MediaAction::Next));
        assert_eq!(parse_media_action("дальше"), Some(MediaAction::Next));
        assert_eq!(parse_media_action("предыдущий"), Some(MediaAction::Prev));
        assert_eq!(parse_media_action("переключи"), Some(MediaAction::Toggle));
        assert_eq!(parse_media_action("играй"), Some(MediaAction::Play));
        assert_eq!(parse_media_action("чтонибудь"), None);
    }

    #[test]
    fn validates_app_name_allows_normal_apps() {
        assert!(validate_app_name("Safari").is_ok());
        assert!(validate_app_name("Visual Studio Code").is_ok());
        assert!(validate_app_name("Google Chrome").is_ok());
        assert!(validate_app_name("Яндекс Музыка").is_ok());
        assert!(validate_app_name("IINA+").is_ok());
    }

    #[test]
    fn validates_app_name_rejects_injection() {
        assert!(validate_app_name("").is_err());
        assert!(validate_app_name("   ").is_err());
        assert!(validate_app_name("/bin/sh").is_err()); // путь
        assert!(validate_app_name("Safari; rm -rf /").is_err()); // shell
        assert!(validate_app_name("app`whoami`").is_err());
        assert!(validate_app_name("a\nb").is_err()); // control
        assert!(validate_app_name(&"x".repeat(65)).is_err()); // длина
    }

    #[test]
    fn clamps_volume_range() {
        assert_eq!(clamp_volume(-10), 0);
        assert_eq!(clamp_volume(0), 0);
        assert_eq!(clamp_volume(55), 55);
        assert_eq!(clamp_volume(100), 100);
        assert_eq!(clamp_volume(250), 100);
    }

    #[test]
    fn parses_volume_set_delta_mute() {
        assert_eq!(parse_volume_op(&json!({"set": 30})).unwrap(), VolumeOp::Set(30));
        assert_eq!(parse_volume_op(&json!({"set": 300})).unwrap(), VolumeOp::Set(100));
        assert_eq!(parse_volume_op(&json!({"delta": -20})).unwrap(), VolumeOp::Delta(-20));
        assert_eq!(parse_volume_op(&json!({"mute": true})).unwrap(), VolumeOp::Mute(true));
        assert_eq!(parse_volume_op(&json!({"action": "громче"})).unwrap(), VolumeOp::Delta(12));
        assert_eq!(parse_volume_op(&json!({"action": "тише"})).unwrap(), VolumeOp::Delta(-12));
        assert_eq!(parse_volume_op(&json!({"action": "mute"})).unwrap(), VolumeOp::Mute(true));
    }

    #[test]
    fn rejects_bad_volume_args() {
        assert!(parse_volume_op(&json!({})).is_err());
        assert!(parse_volume_op(&json!({"delta": 0})).is_err());
        assert!(parse_volume_op(&json!({"action": "wat"})).is_err());
    }

    #[test]
    fn volume_scripts_are_well_formed() {
        assert_eq!(volume_set_script(40), "set volume output volume 40");
        assert!(volume_mute_script(true).contains("with output muted"));
        assert!(volume_mute_script(false).contains("without output muted"));
    }
}
