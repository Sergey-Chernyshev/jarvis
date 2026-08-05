//! «Крышка» (closed-display mode) — понятие ноутбуков Apple: pmset, disablesleep,
//! IOPMrootDomain и sudoers-правило под них. Прямого аналога на Linux нет:
//! поведением крышки распоряжается logind (`HandleLidSwitch` в
//! `/etc/systemd/logind.conf`), править который приложению не положено —
//! это системная настройка, а не переключатель в трее.
//!
//! Поэтому здесь честная заглушка с тем же API, что у macOS-версии: плагин
//! отвечает «не поддерживается», UI видит `sudoers_installed() == false` и сам
//! прячет режим. Ничего не молчит и не притворяется работающим.
//!
//! Чистое ядро (парсеры и `decide_suggest`) осталось бы кросс-платформенным, но
//! без источника данных оно бессмысленно, поэтому не дублируется.

/* ================= типы, общие с macOS-версией ================= */

#[derive(Debug, PartialEq)]
pub struct LidState {
    pub present: bool,
    pub closed: Option<bool>,
    pub causes_sleep: Option<bool>,
}

#[derive(Debug, PartialEq)]
pub struct Battery {
    pub pct: Option<u32>,
    pub on_battery: Option<bool>,
    pub charging: Option<bool>,
}

#[derive(Debug, PartialEq)]
pub enum Suggest {
    No,
    /// Предложить disablesleep.
    Arm,
    /// Есть внешний дисплей — рассказать про родной clamshell-режим.
    Native,
}

/// Пути к sudoers-правилу на Linux нет — режим недоступен целиком.
pub const SUDOERS: &str = "";

/* ================= заглушки ================= */

/// Крышки в терминах macOS не знаем: ни ioreg, ни AppleClamshellState.
pub async fn read_lid() -> LidState {
    LidState { present: false, closed: None, causes_sleep: None }
}

/// Заряд можно было бы взять из `/sys/class/power_supply`, но он нужен только
/// для подсказок про closed-display, которых здесь нет.
pub async fn read_battery() -> Battery {
    Battery { pct: None, on_battery: None, charging: None }
}

/// Никогда не предлагаем режим, которого нет.
pub fn decide_suggest(
    _working_at_sleep: usize,
    _armed: bool,
    _external_display: bool,
    _last_suggest_at: i64,
    _now: i64,
    _min_gap_ms: i64,
) -> Suggest {
    Suggest::No
}

/// Правило sudoers не ставится — значит, режим выключен и в UI не появится.
pub fn sudoers_installed() -> bool {
    false
}

pub fn sudoers_content(_user: &str) -> Result<String, String> {
    Err("closed-display mode доступен только на macOS".into())
}

pub async fn pmset_quiet(_on: bool) -> bool {
    false
}

pub async fn pmset_ask(_on: bool) -> bool {
    false
}

pub fn pmset_quiet_sync(_on: bool) -> bool {
    false
}

pub async fn read_sleep_disabled() -> Option<bool> {
    None
}

/// Форс-сон есть и на Linux (`systemctl suspend`), но зовётся он только из
/// аварийной ветки closed-display — а её здесь нет.
pub async fn force_sleep_now() {}

/// MacBook Air определяем только на macOS: нужен лишь для тамошних подсказок.
pub async fn detect_is_air() -> bool {
    false
}

/// Внешний дисплей на Linux определить можно, но используется признак
/// исключительно в подсказках про clamshell.
pub fn external_display_present() -> bool {
    false
}

pub fn write_marker(_by: &str) {}

pub fn read_marker() -> Option<serde_json::Value> {
    None
}

pub fn clear_marker() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_is_reported_unavailable() {
        assert!(!sudoers_installed());
        assert!(sudoers_content("user").is_err());
        assert_eq!(decide_suggest(3, false, false, 0, 999_999, 0), Suggest::No);
    }
}
