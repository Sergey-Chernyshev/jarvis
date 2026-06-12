//! «Крышка»: closed-display mode — мак работает с закрытой крышкой.
//!
//! Механика: `pmset -a disablesleep 1` — запрет на уровне IOPMrootDomain,
//! выше категорий idle/forced. Это root-уровень и термо-риски, поэтому
//! политика плагина: ДЕТЕКТИТЬ И ПОДСКАЗЫВАТЬ, а не молча sudo. Тихое
//! переключение — только после явного опт-ина: установки
//! /etc/sudoers.d/jarvis-pmset (ровно две команды pmset).
//!
//! Fail-safe (урок Amphetamine Enhancer — «мак не должен зажариться в рюкзаке»):
//!   1) маркер ~/.jarvis/clamshell.json: кто и когда поднял флаг;
//!   2) на старте демона: флаг стоит, маркер наш → демон умирал — восстановить;
//!   3) dispose/квит → восстановить;
//!   4) батарейный сторож: armed + батарея ≤ floor → тихий сброс, нельзя
//!      тихо → pmset sleepnow (форс-сон без root: лучше уснуть, чем зажариться;
//!      admin-диалог под закрытой крышкой никто не увидит — его не зовём).

use std::process::Stdio;
use std::time::Duration;

use crate::util::{jarvis_dir, now_ms};

pub const SUDOERS: &str = "/etc/sudoers.d/jarvis-pmset";

/* ================= чистое ядро: парсеры и решения ================= */

#[derive(Debug, PartialEq)]
pub struct LidState {
    pub present: bool,
    pub closed: Option<bool>,
    pub causes_sleep: Option<bool>,
}

/// ioreg -r -k AppleClamshellState -d 4 → состояние крышки.
/// causesSleep учитывает и родной clamshell-режим, и disablesleep —
/// macOS сама говорит, уснёт ли мак от закрытия крышки прямо сейчас.
pub fn parse_clamshell_state(out: &str) -> LidState {
    let grab = |key: &str| -> Option<bool> {
        let re = regex::RegexBuilder::new(&format!(r#""{key}"\s*=\s*(Yes|No)"#))
            .case_insensitive(true)
            .build()
            .unwrap();
        re.captures(out).map(|c| c[1].eq_ignore_ascii_case("yes"))
    };
    let closed = grab("AppleClamshellState");
    let causes_sleep = grab("AppleClamshellCausesSleep");
    LidState {
        present: closed.is_some() || causes_sleep.is_some(),
        closed,
        causes_sleep,
    }
}

/// pmset -g → стоит ли сейчас флаг disablesleep (строка SleepDisabled).
pub fn parse_sleep_disabled(out: &str) -> Option<bool> {
    regex::Regex::new(r"SleepDisabled\s+(\d)")
        .unwrap()
        .captures(out)
        .map(|c| &c[1] == "1")
}

#[derive(Debug, PartialEq)]
pub struct Battery {
    pub pct: Option<u32>,
    pub on_battery: Option<bool>,
    pub charging: Option<bool>,
}

/// pmset -g batt → процент и источник питания (десктоп без батареи → None).
pub fn parse_battery(out: &str) -> Battery {
    let pct = regex::Regex::new(r"(\d{1,3})%")
        .unwrap()
        .captures(out)
        .and_then(|c| c[1].parse::<u32>().ok())
        .map(|p| p.min(100));
    let on_battery = regex::Regex::new(r"Now drawing from '([^']+)'")
        .unwrap()
        .captures(out)
        .map(|c| c[1].to_lowercase().contains("battery"));
    let charging = if regex::RegexBuilder::new(r";\s*charging").case_insensitive(true).build().unwrap().is_match(out) {
        Some(true)
    } else if out.to_lowercase().contains("discharging") {
        Some(false)
    } else {
        None
    };
    Battery { pct, on_battery, charging }
}

#[derive(Debug, PartialEq)]
pub enum Suggest {
    No,
    /// Предложить disablesleep.
    Arm,
    /// Есть внешний дисплей — рассказать про родной clamshell-режим (root не нужен).
    Native,
}

/// Проснулись после сна: предлагать ли closed-display?
pub fn decide_suggest(
    working_at_sleep: usize,
    armed: bool,
    external_display: bool,
    last_suggest_at: i64,
    now: i64,
    min_gap_ms: i64,
) -> Suggest {
    if working_at_sleep == 0 || armed {
        return Suggest::No;
    }
    if now - last_suggest_at < min_gap_ms {
        return Suggest::No;
    }
    if external_display {
        Suggest::Native
    } else {
        Suggest::Arm
    }
}

/// /etc/sudoers.d/jarvis-pmset: тихий доступ ровно к двум командам.
/// Имя юзера валидируем жёстко — содержимое уходит в sudoers.
pub fn sudoers_content(user: &str) -> Result<String, String> {
    let valid = regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_.-]*$").unwrap();
    if !valid.is_match(user) {
        return Err(format!("недопустимое имя пользователя для sudoers: {user:?}"));
    }
    Ok([
        "# Jarvis: тихое переключение closed-display mode (плагин clamshell).",
        "# Разрешает БЕЗ пароля ровно две команды — включить/выключить disablesleep.",
        &format!("{user} ALL=(root) NOPASSWD: /usr/bin/pmset -a disablesleep 1, /usr/bin/pmset -a disablesleep 0"),
        "",
    ]
    .join("\n"))
}

/* ================= системные обвязки ================= */

async fn run(cmd: &str, args: &[&str], timeout: Duration) -> Option<std::process::Output> {
    let mut c = tokio::process::Command::new(cmd);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(timeout, c.output()).await.ok()?.ok()
}

pub fn sudoers_installed() -> bool {
    std::path::Path::new(SUDOERS).exists()
}

/// Тихий путь: sudo -n работает только с установленным sudoers-правилом.
pub async fn pmset_quiet(on: bool) -> bool {
    run(
        "sudo",
        &["-n", "/usr/bin/pmset", "-a", "disablesleep", if on { "1" } else { "0" }],
        Duration::from_secs(8),
    )
    .await
    .is_some_and(|o| o.status.success())
}

/// Честный путь: сначала тихо, не вышло — admin-диалог (юзер видит и решает).
pub async fn pmset_ask(on: bool) -> bool {
    if pmset_quiet(on).await {
        return true;
    }
    let script = format!(
        "do shell script \"/usr/bin/pmset -a disablesleep {}\" with administrator privileges with prompt \"Jarvis {} closed-display mode\"",
        if on { 1 } else { 0 },
        if on { "включает" } else { "выключает" },
    );
    run("osascript", &["-e", &script], Duration::from_secs(120))
        .await
        .is_some_and(|o| o.status.success())
}

pub async fn read_sleep_disabled() -> Option<bool> {
    let out = run("pmset", &["-g"], Duration::from_secs(4)).await?;
    parse_sleep_disabled(&String::from_utf8_lossy(&out.stdout))
}

pub async fn read_battery() -> Battery {
    match run("pmset", &["-g", "batt"], Duration::from_secs(4)).await {
        Some(out) => parse_battery(&String::from_utf8_lossy(&out.stdout)),
        None => Battery { pct: None, on_battery: None, charging: None },
    }
}

pub async fn read_lid() -> LidState {
    match run("ioreg", &["-r", "-k", "AppleClamshellState", "-d", "4"], Duration::from_secs(4)).await {
        Some(out) => parse_clamshell_state(&String::from_utf8_lossy(&out.stdout)),
        None => LidState { present: false, closed: None, causes_sleep: None },
    }
}

pub async fn force_sleep_now() {
    run("pmset", &["sleepnow"], Duration::from_secs(4)).await;
}

/// MacBook Air без вентилятора — под крышкой троттлит, предупреждаем.
pub async fn detect_is_air() -> bool {
    run("sysctl", &["-n", "hw.model"], Duration::from_secs(3))
        .await
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout).to_lowercase().contains("macbookair")
        })
}

/// Есть ли внешний дисплей (для подсказки про родной clamshell-режим).
pub fn external_display_present() -> bool {
    core_graphics::display::CGDisplay::active_displays()
        .map(|ids| {
            ids.iter()
                .any(|&id| !core_graphics::display::CGDisplay::new(id).is_builtin())
        })
        .unwrap_or(false)
}

/* ---- маркер fail-safe ---- */

fn marker_file() -> std::path::PathBuf {
    jarvis_dir().join("clamshell.json")
}

pub fn write_marker(by: &str) {
    let _ = std::fs::create_dir_all(jarvis_dir());
    let _ = std::fs::write(
        marker_file(),
        serde_json::json!({ "pid": std::process::id(), "by": by, "at": now_ms() }).to_string() + "\n",
    );
}

pub fn read_marker() -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(marker_file()).ok()?).ok()
}

pub fn clear_marker() {
    let _ = std::fs::remove_file(marker_file());
}

/// Синхронное тихое восстановление сна — для пути выхода из приложения,
/// где промисов ждать нельзя (как execFileSync в Electron-версии).
pub fn pmset_quiet_sync(on: bool) -> bool {
    std::process::Command::new("sudo")
        .args(["-n", "/usr/bin/pmset", "-a", "disablesleep", if on { "1" } else { "0" }])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioreg_lid_states() {
        let open = "+-o IOPMrootDomain  <class IOPMrootDomain>\n  |   \"AppleClamshellCausesSleep\" = Yes\n  |   \"AppleClamshellState\" = No\n";
        let lid = parse_clamshell_state(open);
        assert_eq!(lid, LidState { present: true, closed: Some(false), causes_sleep: Some(true) });

        let closed = "  |   \"AppleClamshellCausesSleep\" = No\n  |   \"AppleClamshellState\" = Yes";
        let lid = parse_clamshell_state(closed);
        assert_eq!(lid, LidState { present: true, closed: Some(true), causes_sleep: Some(false) });

        assert!(!parse_clamshell_state("что-то без ключей").present, "нет ключей — крышки нет (десктоп)");
    }

    #[test]
    fn pmset_sleep_disabled() {
        assert_eq!(parse_sleep_disabled(" SleepDisabled\t\t1\n standby 1"), Some(true));
        assert_eq!(parse_sleep_disabled(" SleepDisabled\t\t0"), Some(false));
        assert_eq!(parse_sleep_disabled("мусор"), None);
    }

    #[test]
    fn battery_parsing() {
        let batt = parse_battery("Now drawing from 'Battery Power'\n -InternalBattery-0 (id=23396451)\t37%; discharging; 4:27 remaining present: true");
        assert_eq!(batt.pct, Some(37));
        assert_eq!(batt.on_battery, Some(true));
        let ac = parse_battery("Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t95%; charging; 0:40 remaining present: true");
        assert_eq!(ac.pct, Some(95));
        assert_eq!(ac.on_battery, Some(false));
        assert_eq!(parse_battery("garbage").pct, None, "десктоп без батареи");
    }

    #[test]
    fn suggest_decision_matrix() {
        let now = 10 * 3600 * 1000;
        let gap = 3600 * 1000;
        assert_eq!(decide_suggest(2, false, false, 0, now, gap), Suggest::Arm);
        assert_eq!(decide_suggest(2, false, true, 0, now, gap), Suggest::Native);
        assert_eq!(decide_suggest(0, false, false, 0, now, gap), Suggest::No, "работы не было — молчим");
        assert_eq!(decide_suggest(2, true, false, 0, now, gap), Suggest::No, "уже armed — молчим");
        assert_eq!(decide_suggest(2, false, false, now - 1000, now, gap), Suggest::No, "недавно подсказывали");
    }

    #[test]
    fn sudoers_is_strict() {
        let content = sudoers_content("se.chernyshev").unwrap();
        assert!(content.contains("se.chernyshev ALL=(root) NOPASSWD:"));
        assert!(content.contains("/usr/bin/pmset -a disablesleep 1"));
        assert!(content.contains("/usr/bin/pmset -a disablesleep 0"));
        assert!(sudoers_content("user name; ALL").is_err(), "кривое имя не пролазит");
    }
}
