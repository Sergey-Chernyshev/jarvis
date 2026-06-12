//! «Не спать»: движок грантов. Чистый синхронный модуль — блокер и часы
//! инжектятся, время приходит параметром, фоновых задач нет: дедлайны
//! (таймер, линджер, пульс процесса) проверяет внешний tick() раз в секунду.
//!
//! Инвариант — как у самих IOPMAssertions: assertion активна ⇔ грантов > 0.
//! Грантов максимум два:
//!   auto   — триггер «агенты работают» (аналог Trigger-сессии Amphetamine);
//!   manual — ручной слот (manual | timer | process), новый старт заменяет
//!            предыдущий: kill-then-start, как у Raycast Coffee.

use serde::Serialize;
use serde_json::{json, Value};

use super::assertion::Blocker;

const LINGER_MS: i64 = 60 * 1000;
pub const PRESETS_MIN: [i64; 6] = [15, 30, 60, 120, 240, 480];

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum Manual {
    Manual { label: String },
    Timer { label: String, until: i64 },
    Process { label: String, pid: i64 },
}

/// Что произошло при переходе — реакции (уведомления, обновление трея)
/// исполняет хозяин движка.
#[derive(Debug, PartialEq)]
pub enum Event {
    Changed,
    TimerEnd,
    ProcessDied { label: String },
}

pub struct Engine<B: Blocker> {
    blocker: B,
    pid_alive: Box<dyn Fn(i64) -> bool + Send>,
    auto_enabled: bool,
    display_pref: bool, // тип assertion: не гасить ли экран
    auto_held: bool,
    working_count: usize,
    linger_until: Option<i64>, // отложенное снятие auto-гранта
    manual: Option<Manual>,
    held: Option<(u32, bool)>, // (id блокера, тип) — id бывает 0, Option честнее
}

impl<B: Blocker> Engine<B> {
    pub fn new(blocker: B, auto_enabled: bool, display_pref: bool) -> Self {
        Self {
            blocker,
            pid_alive: Box::new(|pid| unsafe { libc::kill(pid as i32, 0) == 0 }),
            auto_enabled,
            display_pref,
            auto_held: false,
            working_count: 0,
            linger_until: None,
            manual: None,
            held: None,
        }
    }

    #[cfg(test)]
    pub fn with_pid_alive(mut self, f: impl Fn(i64) -> bool + Send + 'static) -> Self {
        self.pid_alive = Box::new(f);
        self
    }

    fn evaluate(&mut self) {
        let need = self.auto_held || self.manual.is_some();
        match (need, self.held) {
            (true, None) => {
                let id = self.blocker.start(self.display_pref);
                self.held = Some((id, self.display_pref));
            }
            (false, Some((id, _))) => {
                self.blocker.stop(id);
                self.held = None;
            }
            _ => {}
        }
    }

    pub fn active(&self) -> bool {
        self.held.is_some()
    }

    pub fn state(&self) -> Value {
        json!({
            "active": self.active(),
            "auto": self.auto_held,
            "autoEnabled": self.auto_enabled,
            "working": self.working_count,
            "lingering": self.linger_until.is_some(),
            "manual": self.manual.as_ref().map(|m| match m {
                Manual::Manual { label } => json!({ "kind": "manual", "label": label }),
                Manual::Timer { label, until } => json!({ "kind": "timer", "label": label, "until": until }),
                Manual::Process { label, pid } => json!({ "kind": "process", "label": label, "pid": pid }),
            }),
        })
    }

    /// Триггер: сколько сессий сейчас working.
    pub fn set_working(&mut self, n: usize, now: i64) -> Vec<Event> {
        self.working_count = n;
        if !self.auto_enabled {
            return vec![];
        }
        if n > 0 {
            self.linger_until = None;
            if !self.auto_held {
                self.auto_held = true;
                self.evaluate();
            }
            vec![Event::Changed] // число working для лейбла могло смениться
        } else if self.auto_held && self.linger_until.is_none() {
            // линджер гасит дребезг working→done→working между ходами и держит
            // мост для авто-циклов, где следующий промпт приходит через секунды
            self.linger_until = Some(now + LINGER_MS);
            vec![Event::Changed]
        } else {
            vec![]
        }
    }

    pub fn set_auto(&mut self, enabled: bool, now: i64) -> Vec<Event> {
        self.auto_enabled = enabled;
        if !enabled {
            self.linger_until = None;
            if self.auto_held {
                self.auto_held = false;
                self.evaluate();
            }
            vec![Event::Changed]
        } else if self.working_count > 0 {
            self.set_working(self.working_count, now)
        } else {
            vec![Event::Changed]
        }
    }

    pub fn start_manual(&mut self, label: Option<String>) -> Vec<Event> {
        self.manual = Some(Manual::Manual {
            label: label.unwrap_or_else(|| "бессрочно".into()),
        });
        self.evaluate();
        vec![Event::Changed]
    }

    pub fn start_timer(&mut self, ms: i64, label: String, now: i64) -> Vec<Event> {
        self.manual = Some(Manual::Timer { label, until: now + ms });
        self.evaluate();
        vec![Event::Changed]
    }

    pub fn start_process(&mut self, pid: i64, label: String) -> Vec<Event> {
        self.manual = Some(Manual::Process { label, pid });
        self.evaluate();
        vec![Event::Changed]
    }

    pub fn stop_manual(&mut self) -> Vec<Event> {
        self.manual = None;
        self.evaluate();
        vec![Event::Changed]
    }

    /// Preference типа assertion сменился (тумблер «не гасить экран»).
    pub fn set_display_pref(&mut self, keep_display_on: bool) -> Vec<Event> {
        self.display_pref = keep_display_on;
        if let Some((id, held_type)) = self.held {
            if held_type != keep_display_on {
                self.blocker.stop(id);
                let id = self.blocker.start(keep_display_on);
                self.held = Some((id, keep_display_on));
                return vec![Event::Changed];
            }
        }
        vec![]
    }

    /// Внешний пульс: дедлайны таймера/линджера и живость процесса.
    pub fn tick(&mut self, now: i64) -> Vec<Event> {
        let mut events = Vec::new();
        if let Some(until) = self.linger_until {
            if now >= until {
                self.linger_until = None;
                self.auto_held = false;
                self.evaluate();
                events.push(Event::Changed);
            }
        }
        match &self.manual {
            Some(Manual::Timer { until, .. }) if now >= *until => {
                self.manual = None;
                self.evaluate();
                events.push(Event::TimerEnd);
                events.push(Event::Changed);
            }
            Some(Manual::Process { pid, label }) if !(self.pid_alive)(*pid) => {
                let label = label.clone();
                self.manual = None;
                self.evaluate();
                events.push(Event::ProcessDied { label });
                events.push(Event::Changed);
            }
            _ => {}
        }
        events
    }

    pub fn dispose(&mut self) {
        self.linger_until = None;
        self.manual = None;
        self.auto_held = false;
        self.evaluate();
    }
}

/// «ещё 47м» — остаток таймера для строки статуса.
pub fn fmt_left(until: i64, now: i64) -> String {
    let m = (((until - now) as f64) / 60_000.0).round().max(1.0) as i64;
    if m < 60 {
        return format!("{m}м");
    }
    let (h, mm) = (m / 60, m % 60);
    if mm == 0 {
        format!("{h}ч")
    } else {
        format!("{h}ч {mm}м")
    }
}

pub fn preset_label(min: i64) -> String {
    if min < 60 {
        return format!("{min} минут");
    }
    let h = min / 60;
    match h {
        1 => "1 час".into(),
        2..=4 => format!("{h} часа"),
        _ => format!("{h} часов"),
    }
}

/// Строка статуса «агенты работают (2) · ещё 47м» для трея/панели/футера.
pub fn status_line(st: &Value, now: i64) -> Option<String> {
    if !st["active"].as_bool().unwrap_or(false) {
        return None;
    }
    let mut parts = Vec::new();
    if st["auto"].as_bool().unwrap_or(false) {
        if st["lingering"].as_bool().unwrap_or(false) {
            parts.push("агенты затихли — держу ещё минуту".to_string());
        } else {
            parts.push(format!("агенты работают ({})", st["working"].as_u64().unwrap_or(0)));
        }
    }
    if let Some(manual) = st["manual"].as_object() {
        match manual.get("kind").and_then(Value::as_str) {
            Some("timer") => parts.push(format!(
                "ещё {}",
                fmt_left(manual.get("until").and_then(Value::as_i64).unwrap_or(now), now)
            )),
            Some("process") => parts.push(format!(
                "пока жив {}",
                manual.get("label").and_then(Value::as_str).unwrap_or("?")
            )),
            _ => parts.push("бессрочно".into()),
        }
    }
    Some(parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Считаем start/stop; id=0 — ловушка на проверку truthiness из JS-версии.
    #[derive(Clone, Default)]
    struct FakeBlocker {
        starts: Arc<Mutex<Vec<bool>>>,
        stops: Arc<Mutex<u32>>,
        next: Arc<Mutex<u32>>,
    }

    impl Blocker for FakeBlocker {
        fn start(&mut self, display: bool) -> u32 {
            self.starts.lock().unwrap().push(display);
            let mut n = self.next.lock().unwrap();
            let id = *n;
            *n += 1;
            id
        }
        fn stop(&mut self, _id: u32) {
            *self.stops.lock().unwrap() += 1;
        }
    }

    fn engine(auto: bool) -> (Engine<FakeBlocker>, FakeBlocker) {
        let b = FakeBlocker::default();
        (Engine::new(b.clone(), auto, false), b)
    }

    #[test]
    fn working_takes_and_holds_assertion() {
        let (mut e, b) = engine(true);
        assert!(!e.active());
        e.set_working(2, 0);
        assert!(e.active());
        assert_eq!(b.starts.lock().unwrap().len(), 1);
        e.set_working(3, 0); // больше working — но грант уже есть
        assert_eq!(b.starts.lock().unwrap().len(), 1, "повторный working не плодит блокеры");
    }

    #[test]
    fn linger_bridges_short_gaps() {
        let (mut e, b) = engine(true);
        e.set_working(1, 0);
        e.set_working(0, 1000);
        assert!(e.active(), "сразу после working=0 ещё активен");
        e.tick(1000 + LINGER_MS);
        assert!(!e.active(), "линджер вышел — assertion снята");
        assert_eq!(*b.stops.lock().unwrap(), 1);

        e.set_working(1, 70_000);
        e.set_working(0, 71_000);
        e.set_working(1, 72_000); // вернулся в работу внутри линджера
        e.tick(72_000 + LINGER_MS + 1);
        assert!(e.active(), "working вернулся в линджер — грант жив");
        assert_eq!(b.starts.lock().unwrap().len(), 2, "возврат в линджере не перезапускает блокер");
    }

    #[test]
    fn auto_toggle() {
        let (mut e, _) = engine(true);
        e.set_working(2, 0);
        e.set_auto(false, 0);
        assert!(!e.active(), "setAuto(false) снимает auto-грант сразу, без линджера");
        e.set_working(5, 0);
        assert!(!e.active(), "при выключенном авто working игнорируется");
        e.set_auto(true, 0);
        e.set_working(1, 0);
        assert!(e.active(), "авто включили обратно — триггер работает");
    }

    #[test]
    fn timer_semantics() {
        let (mut e, _) = engine(true);
        e.start_timer(40_000, "40с".into(), 0);
        assert!(e.active());
        let st = e.state();
        assert_eq!(st["manual"]["kind"], "timer");
        assert!(st["manual"]["until"].as_i64().unwrap() > 0);
        let events = e.tick(40_001);
        assert!(events.contains(&Event::TimerEnd));
        assert!(!e.active(), "таймер вышел — assertion снята");
    }

    #[test]
    fn manual_slot_is_single() {
        let (mut e, b) = engine(true);
        e.start_timer(5_000_000, "час".into(), 0);
        e.start_manual(None);
        assert_eq!(e.state()["manual"]["kind"], "manual", "manual заменил timer в слоте");
        assert_eq!(b.starts.lock().unwrap().len(), 1);
        assert_eq!(*b.stops.lock().unwrap(), 0, "замена слота не дёргает блокер");
        let events = e.tick(6_000_000); // протухший таймер после замены не стреляет
        assert!(!events.contains(&Event::TimerEnd));
        assert!(e.active());
    }

    #[test]
    fn process_grant_follows_pid() {
        let alive = Arc::new(Mutex::new(true));
        let alive2 = alive.clone();
        let b = FakeBlocker::default();
        let mut e = Engine::new(b, true, false).with_pid_alive(move |_| *alive2.lock().unwrap());
        e.start_process(12345, "Safari".into());
        assert!(e.active());
        assert!(e.tick(15_000).is_empty(), "процесс жив — assertion держится");
        *alive.lock().unwrap() = false;
        let events = e.tick(30_000);
        assert!(matches!(&events[0], Event::ProcessDied { label } if label == "Safari"));
        assert!(!e.active());
    }

    #[test]
    fn auto_and_manual_are_independent() {
        let (mut e, b) = engine(true);
        e.set_working(1, 0);
        e.start_manual(None);
        e.stop_manual();
        assert!(e.active(), "stopManual не трогает auto-грант");
        e.set_auto(false, 0);
        assert!(!e.active(), "оба гранта сняты — assertion ушла");
        assert_eq!(*b.stops.lock().unwrap(), 1, "блокер остановлен ровно один раз");
    }

    #[test]
    fn blocker_id_zero_is_valid() {
        let (mut e, b) = engine(true);
        e.start_manual(None); // FakeBlocker отдаёт id=0 первым
        assert!(e.active(), "blocker id=0 считается активным");
        e.stop_manual();
        assert_eq!(*b.stops.lock().unwrap(), 1, "blocker id=0 корректно останавливается");
    }

    #[test]
    fn display_pref_restarts_blocker() {
        let (mut e, b) = engine(true);
        e.start_manual(None);
        e.set_display_pref(true);
        let starts = b.starts.lock().unwrap();
        assert_eq!(*b.stops.lock().unwrap(), 1);
        assert_eq!(starts.as_slice(), &[false, true], "перезапуск с новым типом");
        drop(starts);
        e.set_display_pref(true); // тип не менялся — ничего не происходит
        assert_eq!(*b.stops.lock().unwrap(), 1);
    }
}
