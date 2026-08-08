//! Когда цикл проснётся.
//!
//! Время местное, а не UTC: «в 02:00» человек имеет в виду свою ночь, и цикл,
//! проснувшийся в 02:00 UTC, разбудит его вентилятором посреди вечера.

use super::model::{Loop, Wake};
use chrono::{Duration, Local, NaiveTime, TimeZone};

/// Ближайшее пробуждение после `after` (миллисекунды эпохи). None — только руками.
pub fn next_wake(item: &Loop, after: i64) -> Option<i64> {
    match &item.schedule.wake {
        Wake::Manual => None,
        Wake::Every { minutes } => {
            if *minutes == 0 {
                return None;
            }
            let step = *minutes as i64 * 60_000;
            // От последнего запуска, а не от «сейчас»: иначе цикл, который
            // только что отработал, проснулся бы снова через полный интервал
            // от момента показа экрана.
            let base = if item.last_run_at > 0 { item.last_run_at } else { after };
            let mut at = base + step;
            if at <= after {
                // Пропущенные интервалы не догоняем пачкой: машина могла
                // проспать неделю, и десять запусков подряд — не то, чего
                // человек просил.
                let missed = (after - base) / step;
                at = base + (missed + 1) * step;
            }
            Some(at)
        }
        Wake::Daily { at } => {
            let time = parse_hhmm(at)?;
            let from = Local.timestamp_millis_opt(after).single()?;
            let today = from.date_naive().and_time(time);
            let local_today = Local.from_local_datetime(&today).single()?;
            let pick = if local_today.timestamp_millis() > after {
                local_today
            } else {
                let tomorrow = (from.date_naive() + Duration::days(1)).and_time(time);
                Local.from_local_datetime(&tomorrow).single()?
            };
            Some(pick.timestamp_millis())
        }
    }
}

fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    let (h, m) = s.split_once(':')?;
    NaiveTime::from_hms_opt(h.trim().parse().ok()?, m.trim().parse().ok()?, 0)
}

/// Человеческая подпись «следующее пробуждение».
pub fn wake_label(item: &Loop) -> String {
    match &item.schedule.wake {
        Wake::Manual => "только руками".into(),
        Wake::Daily { at } => format!("каждый день в {at}"),
        Wake::Every { minutes } => match *minutes {
            0 => "только руками".into(),
            m if m % (24 * 60) == 0 => {
                let days = m / (24 * 60);
                if days == 7 { "раз в неделю".into() } else { format!("раз в {days} дн.") }
            }
            m if m % 60 == 0 => format!("каждые {} ч", m / 60),
            m => format!("каждые {m} мин"),
        },
    }
}

/// Пора ли будить: расписание наступило и цикл ещё не бежит.
pub fn due(item: &Loop, now: i64) -> bool {
    match next_wake(item, item.last_run_at.max(0)) {
        Some(at) => at <= now,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    fn daily(at: &str) -> Loop {
        let mut l = Loop::default();
        l.schedule.wake = Wake::Daily { at: at.into() };
        l
    }

    #[test]
    fn daily_wake_lands_on_the_local_clock() {
        let l = daily("02:00");
        let now = Local::now().timestamp_millis();
        let at = next_wake(&l, now).expect("ежедневное расписание всегда даёт время");
        assert!(at > now, "пробуждение всегда в будущем");
        let when = Local.timestamp_millis_opt(at).single().unwrap();
        assert_eq!((when.hour(), when.minute()), (2, 0));
        // Не дальше, чем на сутки вперёд.
        assert!(at - now <= 24 * 3600_000 + 1000);
    }

    #[test]
    fn broken_time_does_not_schedule_anything() {
        assert!(next_wake(&daily("двадцать пять"), 0).is_none());
        assert!(next_wake(&daily("25:00"), 0).is_none());
        assert!(next_wake(&daily(""), 0).is_none());
    }

    #[test]
    fn interval_counts_from_the_last_run() {
        let mut l = Loop::default();
        l.schedule.wake = Wake::Every { minutes: 60 };
        l.last_run_at = 1_000_000;
        assert_eq!(next_wake(&l, 1_000_000), Some(1_000_000 + 3_600_000));
    }

    #[test]
    fn a_long_sleep_does_not_queue_a_burst() {
        let mut l = Loop::default();
        l.schedule.wake = Wake::Every { minutes: 60 };
        l.last_run_at = 0;
        // Машина проспала десять часов: следующее пробуждение — одно, ближайшее,
        // а не десять пропущенных подряд.
        let at = next_wake(&l, 10 * 3_600_000).unwrap();
        assert_eq!(at, 11 * 3_600_000);
    }

    #[test]
    fn manual_never_wakes_itself() {
        let l = Loop::default();
        assert!(next_wake(&l, 0).is_none());
        assert!(!due(&l, i64::MAX / 2));
    }

    #[test]
    fn labels_read_like_a_human_wrote_them() {
        let mut l = Loop::default();
        assert_eq!(wake_label(&l), "только руками");
        l.schedule.wake = Wake::Daily { at: "02:00".into() };
        assert_eq!(wake_label(&l), "каждый день в 02:00");
        l.schedule.wake = Wake::Every { minutes: 60 };
        assert_eq!(wake_label(&l), "каждые 1 ч");
        l.schedule.wake = Wake::Every { minutes: 7 * 24 * 60 };
        assert_eq!(wake_label(&l), "раз в неделю");
        l.schedule.wake = Wake::Every { minutes: 45 };
        assert_eq!(wake_label(&l), "каждые 45 мин");
    }
}
