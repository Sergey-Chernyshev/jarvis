//! Бюджет лимитов: живые проценты подписок, резерв последнего дня, темп и
//! лестница ступеней.
//!
//! Источник процентов — официальные эндпоинты подписок (claude
//! `/api/oauth/usage`, kimi `/coding/v1/usages`). ТОЛЬКО GET и только чтение:
//! обновление/ротация токена разлогинила бы CLI человека, поэтому протухший
//! токен — это «нет свежих чисел», а не повод его обновить. Прежний путь
//! `claude -p /usage` мёртв: headless больше не печатает проценты вовсе.
//!
//! Опросчик ОДИН на приложение: чат, агент и панель читают общий кэш. Частота —
//! по надобности (далеко от порогов редко, у порога чаще, ничего не происходит
//! — не ходим вовсе), отказы уважаются экспоненциальным откатом, число запросов
//! за час считается и лежит в диагностике: ответ на «не заспамил ли» обязан
//! быть числом, а не уверением.

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::daemon::Daemon;
use crate::util::now_ms;

pub const CLAUDE: &str = "claude";
pub const KIMI: &str = "kimi";

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;
/// МСК круглый год UTC+3 (переводов нет с 2014-го) — сутки режем по нему.
const MSK_MS: i64 = 3 * HOUR_MS;

/// Окна темпа: короткое ловит начавшийся рывок, длинное не даёт паниковать от
/// всплеска. Берём худший из двух.
const RATE_SHORT_H: f64 = 6.0;
const RATE_LONG_H: f64 = 24.0;
/// Короче — это шум опросов, а не темп.
const RATE_MIN_SPAN_MS: i64 = 15 * 60_000;

/* ================= настройки ================= */

/// Настройки бюджета. Агенту на запись НЕ отдаются: тот, кто вправе поднять
/// себе потолок, потолка не имеет. Это единственный ограничитель подъёма
/// сессий — на их ЧИСЛО потолка нет, только на расход.
#[derive(Debug, Clone)]
pub struct Cfg {
    /// Буфер сверху резерва claude, % недельного лимита.
    pub claude_buffer: f64,
    /// Плоский минимум последнего дня kimi, % недельного лимита.
    pub kimi_floor: f64,
    /// Хвост короче — не день, приклеивается к предыдущему.
    pub glue_h: f64,
    pub poll_idle_min: i64,
    pub poll_active_min: i64,
    pub poll_near_min: i64,
    /// Границы ночи в местном времени: у человека они свои.
    pub night_from: String,
    pub night_to: String,
    /// Жёсткий ночной потолок, % недельного лимита за одну ночь.
    pub night_cap: f64,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            claude_buffer: 5.0,
            kimi_floor: 5.0,
            glue_h: 6.0,
            poll_idle_min: 15,
            poll_active_min: 3,
            poll_near_min: 2,
            night_from: "23:00".into(),
            night_to: "08:00".into(),
            night_cap: 15.0,
        }
    }
}

impl Cfg {
    /// Поле за полем с фолбэком на дефолт: `settings.json` мержится только по
    /// верхнему уровню, и частичный блок `budget` иначе снёс бы остальные.
    pub fn from_settings(s: &Value) -> Self {
        let d = Cfg::default();
        let num = |k: &str, def: f64| {
            s.pointer(&format!("/budget/{k}")).and_then(Value::as_f64).unwrap_or(def)
        };
        let text = |k: &str, def: &str| {
            s.pointer(&format!("/budget/{k}"))
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .unwrap_or(def)
                .to_string()
        };
        Self {
            claude_buffer: num("claudeBufferPct", d.claude_buffer).clamp(0.0, 50.0),
            kimi_floor: num("kimiFloorPct", d.kimi_floor).clamp(0.0, 50.0),
            glue_h: num("tailGlueHours", d.glue_h).clamp(0.0, 24.0),
            poll_idle_min: num("pollIdleMin", d.poll_idle_min as f64).clamp(1.0, 240.0) as i64,
            poll_active_min: num("pollActiveMin", d.poll_active_min as f64).clamp(1.0, 240.0) as i64,
            poll_near_min: num("pollNearMin", d.poll_near_min as f64).clamp(1.0, 240.0) as i64,
            night_from: text("nightFrom", &d.night_from),
            night_to: text("nightTo", &d.night_to),
            night_cap: num("nightCapPct", d.night_cap).clamp(0.0, 100.0),
        }
    }
}

pub fn cfg(d: &Arc<Daemon>) -> Cfg {
    Cfg::from_settings(&d.settings.load())
}

/* ================= горизонт и резерв ================= */

/// Горизонт до сброса, порезанный по календарным суткам МСК: остаток текущих
/// суток, целые сутки, неполный хвост — в часах.
///
/// Хвост короче `glue_h` не считается днём и приклеивается к предыдущему.
/// Без этого «последним днём» становится часовой огрызок после полуночи, и вся
/// дневная норма последнего дня выпадает из резерва.
pub fn segments(now: i64, reset: i64, glue_h: f64) -> Vec<f64> {
    let mut out = Vec::new();
    if reset <= now {
        return out;
    }
    let mut edge = ((now + MSK_MS).div_euclid(DAY_MS) + 1) * DAY_MS - MSK_MS; // ближайшая полночь МСК
    let mut cur = now;
    while edge < reset {
        out.push((edge - cur) as f64 / HOUR_MS as f64);
        cur = edge;
        edge += DAY_MS;
    }
    out.push((reset - cur) as f64 / HOUR_MS as f64);
    if out.len() > 1 && out.last().is_some_and(|t| *t < glue_h) {
        let tail = out.pop().unwrap_or(0.0);
        if let Some(last) = out.last_mut() {
            *last += tail;
        }
    }
    out
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// Сколько процентов недели можно тратить в сутки.
    pub norm_per_day: f64,
    /// Доля последнего дня — пропорционально часам (claude) или плоский минимум (kimi).
    pub last_day: f64,
    /// Резерв целиком: доля последнего дня + буфер.
    pub reserve: f64,
    /// Рабочий бюджет: остаток минус резерв.
    pub usable: f64,
    /// Сколько суток до сброса (дробно).
    pub days_left: f64,
    pub last_day_hours: f64,
}

/// Резерв и норма из живых чисел. `flat` — модель kimi: последний день короткий
/// (14,5 ч), полноценная доля ему не нужна, нужен плоский минимум, и буфера
/// сверху нет. Иначе модель claude: последний день живёт полноценно (доля
/// пропорционально часам) И сверху лежит буфер на непредвиденное.
///
/// Фиксированная часть резерва снимается с остатка ДО деления на горизонт:
/// тратить её нельзя, значит и в норму она входить не должна.
pub fn plan(remaining: f64, now: i64, reset: i64, cfg: &Cfg, flat: bool) -> Plan {
    let segs = segments(now, reset, cfg.glue_h);
    let total_h: f64 = segs.iter().sum();
    let last_h = segs.last().copied().unwrap_or(0.0);
    let horizon = (total_h / 24.0).max(1e-6);
    let last_days = last_h / 24.0;
    let (fixed, plan_days) = if flat {
        // последний день покрыт плоским минимумом — из планового горизонта он уходит
        (cfg.kimi_floor.min(remaining.max(0.0)), (horizon - last_days).max(1.0 / 24.0))
    } else {
        (cfg.claude_buffer, horizon)
    };
    let norm = ((remaining - fixed).max(0.0) / plan_days).max(0.0);
    let last_day = if flat { fixed } else { norm * last_days };
    let reserve = if flat { fixed } else { last_day + cfg.claude_buffer };
    Plan {
        norm_per_day: norm,
        last_day,
        reserve,
        usable: (remaining - reserve).max(0.0),
        days_left: horizon,
        last_day_hours: last_h,
    }
}

/// Перерасход дня не съедает завтрашний молча: остаток делится на оставшиеся
/// часы заново, норма падает — и это надо сказать словами, а не процентами.
pub fn norm_note(prev: Option<f64>, now: f64) -> Option<String> {
    let prev = prev?;
    if prev <= 0.0 || now >= prev * 0.95 {
        return None;
    }
    Some(format!(
        "норма упала {prev:.1} → {now:.1}%/сут: перерасход раздан по оставшимся часам, завтрашний день не тронут"
    ))
}

/* ================= темп и лестница ================= */

/// Темп %/сут по скользящему окну — не по календарным суткам: человек работает
/// рывками, и «за сегодня» после полуночи обнулится, хотя рывок продолжается.
pub fn rate_pct_day(samples: &[(i64, f64)], now: i64, used_now: f64) -> Option<f64> {
    let one = |hours: f64| -> Option<f64> {
        let from = now - (hours * HOUR_MS as f64) as i64;
        let (t0, u0) = samples.iter().find(|(t, _)| *t >= from).copied()?;
        if now - t0 < RATE_MIN_SPAN_MS {
            return None;
        }
        Some(((used_now - u0) / ((now - t0) as f64 / DAY_MS as f64)).max(0.0))
    };
    // худший из двух окон
    match (one(RATE_SHORT_H), one(RATE_LONG_H)) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// Мёртвая зона: пока после сброса не прошло 10% окна или не потрачено 5% —
/// прогноза не выдаём. Сразу после сброса любой темп даёт «кончится завтра».
pub fn in_dead_zone(elapsed_frac: f64, used: f64) -> bool {
    elapsed_frac < 0.10 || used < 5.0
}

/// Ступень лестницы — по ЗАПАСУ ХОДА, а не по проценту остатка.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rung {
    /// Чисел нет или прогноз молчит — всегда с текстовой причиной: молчание
    /// добытчика неотличимо от «всё хорошо».
    Unknown,
    Ok,
    /// Предупредить: прогноз перестал дотягивать до сброса.
    Warn,
    /// Рутину на kimi: не дотягивает больше чем на сутки.
    Routine,
    /// Фон в очередь: прогноз показывает, что резерв будет тронут до сброса.
    Queue,
    /// Стоп: резерв НАЧАЛ расходоваться (факт, не прогноз).
    Stop,
}

/// Ступень + причина словами. `rate` None — прогноза нет (мёртвая зона или
/// мало точек), но «стоп» по факту всё равно работает: он не про прогноз.
pub fn rung(remaining: f64, p: &Plan, rate: Option<f64>, ttr_days: f64, why_no_rate: &str) -> (Rung, String) {
    if remaining <= p.reserve {
        return (
            Rung::Stop,
            format!("резерв начал расходоваться: осталось {remaining:.1}% при резерве {:.1}%", p.reserve),
        );
    }
    let Some(rate) = rate.filter(|r| *r > 0.0) else {
        return (Rung::Unknown, why_no_rate.to_string());
    };
    let runway_total = remaining / rate;
    let runway_usable = p.usable / rate;
    let hours = |d: f64| format!("{:.0} ч", d * 24.0);
    if runway_total < ttr_days {
        (
            Rung::Queue,
            format!(
                "при темпе {rate:.1}%/сут запаса хода {} против {} до сброса — резерв будет тронут, фон в очередь",
                hours(runway_total),
                hours(ttr_days)
            ),
        )
    } else if runway_usable < ttr_days - 1.0 {
        (
            Rung::Routine,
            format!(
                "рабочего бюджета не хватает больше чем на сутки ({} против {}) — рутину на kimi",
                hours(runway_usable),
                hours(ttr_days)
            ),
        )
    } else if runway_usable < ttr_days {
        (
            Rung::Warn,
            format!(
                "при темпе {rate:.1}%/сут прогноз перестал дотягивать до сброса ({} против {})",
                hours(runway_usable),
                hours(ttr_days)
            ),
        )
    } else {
        (Rung::Ok, format!("темп {rate:.1}%/сут при норме {:.1}%/сут", p.norm_per_day))
    }
}

/* ================= ночь ================= */

/// «HH:MM» → минуты от полуночи. Мусор — None.
fn hhmm(s: &str) -> Option<i64> {
    let (h, m) = s.split_once(':')?;
    let (h, m): (i64, i64) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    (0..24).contains(&h).then_some(h * 60 + m)
}

/// Сейчас ночь? Границы — настройка, а не часы в коде: у человека они свои.
/// Время местное (ночь — про человека, а не про пояс лимитов).
pub fn is_night_at(cfg: &Cfg, now: i64) -> bool {
    let (Some(from), Some(to)) = (hhmm(&cfg.night_from), hhmm(&cfg.night_to)) else {
        return false;
    };
    if from == to {
        return false;
    }
    let local: chrono::DateTime<chrono::Local> =
        chrono::DateTime::from_timestamp_millis(now).unwrap_or_default().into();
    let mins = chrono::Timelike::hour(&local) as i64 * 60 + chrono::Timelike::minute(&local) as i64;
    if from < to {
        (from..to).contains(&mins)
    } else {
        mins >= from || mins < to // окно через полночь
    }
}

/// Предикат для остальной системы: стоп-условия цепочки, откладывание
/// необратимого и утренняя сводка живут не здесь (`agent/chain.rs`), но
/// «сейчас ночь?» они обязаны спрашивать в одном месте — тут.
#[allow(dead_code)] // потребитель — ночной режим цепочки
pub fn is_night(d: &Arc<Daemon>) -> bool {
    is_night_at(&cfg(d), now_ms())
}

/// Ночью буфер недоступен ВСЕГДА: он тратится только по явному разрешению
/// человека, а ночью человека нет. Это явное состояние, а не следствие.
pub const BUFFER_REASON: &str = "буфер доступен только с твоего разрешения";

/* ================= состояние провайдера ================= */

#[derive(Default)]
struct Prov {
    live: Option<Live>,
    /// Почему чисел нет или почему они несвежие.
    err: Option<String>,
    /// (когда, % недели) — точки для скользящего темпа.
    samples: VecDeque<(i64, f64)>,
    /// Подряд неудач — отсюда экспоненциальный откат.
    fails: u32,
    /// Раньше этого момента не ходить (откат или выбранная частота).
    next_at: i64,
    /// (начало ночи, % недели на тот момент) — база жёсткого ночного потолка.
    night_base: Option<(i64, f64)>,
    /// Прежняя норма — чтобы падение сказать словами.
    prev_norm: Option<f64>,
}

/// Живые числа провайдера ровно как приехали.
#[derive(Debug, Clone, Copy)]
pub struct Live {
    /// % недельного окна ИСПОЛЬЗОВАНО (шкала 100, не доля).
    pub week_used: f64,
    pub week_reset: i64,
    /// % короткого окна (5 ч у claude, 300 мин у kimi).
    pub win_used: f64,
    pub win_reset: i64,
    /// Длина недельного окна, мс (для «сколько прошло после сброса»).
    pub week_len: i64,
    /// Момент получения — несвежесть показываем, а не выдаём старое за текущее.
    pub at: i64,
}

pub struct Budget {
    provs: Mutex<HashMap<&'static str, Prov>>,
    /// Моменты запросов — счётчик за час в диагностику.
    reqs: Mutex<VecDeque<i64>>,
    busy: AtomicBool,
}

impl Budget {
    fn new() -> Self {
        Self {
            provs: Mutex::new(HashMap::new()),
            reqs: Mutex::new(VecDeque::new()),
            busy: AtomicBool::new(false),
        }
    }

    fn note_request(&self, now: i64) {
        let mut r = self.reqs.lock().unwrap_or_else(|e| e.into_inner());
        r.push_back(now);
        while r.front().is_some_and(|t| now - *t > HOUR_MS) {
            r.pop_front();
        }
    }

    pub fn requests_last_hour(&self) -> usize {
        let now = now_ms();
        self.reqs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|t| now - **t <= HOUR_MS)
            .count()
    }
}

/// Один опросчик на всё приложение — и одно состояние на процесс. На `Daemon`
/// он не живёт намеренно: бюджет читают и там, где демона под рукой нет.
pub fn budget() -> &'static Budget {
    static B: OnceLock<Budget> = OnceLock::new();
    B.get_or_init(Budget::new)
}

/// Экспоненциальный откат: отказ уважаем, а не повторяем по кругу.
/// 1, 2, 4… минуты, потолок час.
pub fn backoff_ms(fails: u32) -> i64 {
    let step = 60_000i64.saturating_mul(1i64 << fails.min(6));
    step.min(60 * 60_000)
}

/* ================= добытчики (ТОЛЬКО GET) ================= */

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("http: {e}"))
}

async fn get_json(url: &str, token: &str) -> Result<Value, String> {
    let resp = http()?
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| format!("сеть: {e}"))?;
    let code = resp.status().as_u16();
    if code != 200 {
        // 429 тут — это отказ эндпоинта (он же прилетает вообще без заголовка),
        // а НЕ «лимит исчерпан»: вешать на него логику стены нельзя.
        let head = crate::util::ellipsize(
            &crate::util::one_line(&resp.text().await.unwrap_or_default()),
            120,
        );
        return Err(format!("HTTP {code}: {head}"));
    }
    resp.json::<Value>().await.map_err(|e| format!("формат ответа: {e}"))
}

fn iso_ms(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

/// Токен claude: keychain (macOS) или `~/.claude/.credentials.json` (Linux).
/// Только чтение — ни обновления, ни ротации.
fn claude_token() -> Result<String, String> {
    let from_json = |raw: &str| -> Option<String> {
        serde_json::from_str::<Value>(raw)
            .ok()?
            .pointer("/claudeAiOauth/accessToken")?
            .as_str()
            .map(String::from)
    };
    if let Ok(raw) = std::fs::read_to_string(crate::util::claude_dir().join(".credentials.json")) {
        if let Some(t) = from_json(&raw) {
            return Ok(t);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("security")
            .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
            .output()
            .map_err(|e| format!("keychain: {e}"))?;
        if out.status.success() {
            if let Some(t) = from_json(String::from_utf8_lossy(&out.stdout).trim()) {
                return Ok(t);
            }
        }
    }
    Err("токена claude нет: не авторизован локально".into())
}

/// Токен kimi живёт 15 минут. Обновлять его мы НЕ будем — ротация ключа
/// обновления разлогинит CLI человека. Протух → «нет свежих чисел», и ходить
/// с ним в сеть незачем.
fn kimi_token(now: i64) -> Result<String, String> {
    let path = crate::backend::kimi::kimi_home().join("credentials/kimi-code.json");
    let raw = std::fs::read_to_string(&path).map_err(|_| "kimi не авторизован".to_string())?;
    let v: Value = serde_json::from_str(&raw).map_err(|e| format!("kimi credentials: {e}"))?;
    let exp = v.get("expires_at").and_then(Value::as_i64).unwrap_or(0) * 1000;
    if exp > 0 && exp <= now {
        return Err(format!(
            "токен kimi протух {} назад — обновлять не будем (разлогинит CLI), нет свежих чисел",
            crate::util::fmt_reset_in(exp)
        ));
    }
    v.get("access_token")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| "в kimi credentials нет access_token".into())
}

/// Ответ claude: `seven_day`/`five_hour` с `utilization` УЖЕ в процентах (55.0,
/// не доля) и ISO-временем сброса.
pub fn parse_claude(v: &Value, at: i64) -> Option<Live> {
    let part = |k: &str| -> Option<(f64, i64)> {
        let o = v.get(k)?;
        let u = o.get("utilization").and_then(Value::as_f64)?;
        let r = o.get("resets_at").and_then(Value::as_str).map(iso_ms).unwrap_or(0);
        Some((u, r))
    };
    let (week_used, week_reset) = part("seven_day")?;
    let (win_used, win_reset) = part("five_hour").unwrap_or((0.0, 0));
    Some(Live {
        week_used,
        week_reset,
        win_used,
        win_reset,
        week_len: 7 * DAY_MS,
        at,
    })
}

/// Недельное окно модели из `limits[]` — для панели (строка «Current week (X)»).
fn claude_model_week(v: &Value) -> Option<crate::usage::ModelWeek> {
    let l = v
        .get("limits")?
        .as_array()?
        .iter()
        .find(|l| l.get("kind").and_then(Value::as_str) == Some("weekly_scoped"))?;
    Some(crate::usage::ModelWeek {
        model: l
            .pointer("/scope/model/display_name")
            .and_then(Value::as_str)
            .unwrap_or("модель")
            .to_string(),
        pct: l.get("percent").and_then(Value::as_i64).unwrap_or(0),
        reset_at: l.get("resets_at").and_then(Value::as_str).map(iso_ms).unwrap_or(0),
    })
}

/// Ответ kimi: `usage.{used,limit,resetTime}` — неделя, `limits[].detail` — окно
/// 300 минут. Шкала 100, то есть те же целые проценты; считаем долей от limit,
/// чтобы пережить смену шкалы. `resetTime` встречается и числом (сек/мс), и ISO.
pub fn parse_kimi(v: &Value, at: i64) -> Option<Live> {
    let pct = |o: &Value| -> f64 {
        let used = o.get("used").and_then(Value::as_f64).unwrap_or(0.0);
        let limit = o.get("limit").and_then(Value::as_f64).unwrap_or(0.0);
        if limit > 0.0 {
            used / limit * 100.0
        } else {
            used
        }
    };
    let when = |o: &Value| -> i64 {
        match o.get("resetTime") {
            Some(Value::Number(n)) => {
                let x = n.as_f64().unwrap_or(0.0);
                if x > 1e11 { x as i64 } else { (x * 1000.0) as i64 } // сек или мс
            }
            Some(Value::String(s)) => iso_ms(s),
            _ => 0,
        }
    };
    let week = v.get("usage")?;
    let win = v
        .get("limits")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|l| l.get("detail"));
    Some(Live {
        week_used: pct(week),
        week_reset: when(week),
        win_used: win.map(pct).unwrap_or(0.0),
        win_reset: win.map(when).unwrap_or(0),
        week_len: 7 * DAY_MS,
        at,
    })
}

async fn fetch(provider: &str, now: i64) -> Result<(Live, Option<crate::usage::ModelWeek>), String> {
    match provider {
        CLAUDE => {
            let v = get_json("https://api.anthropic.com/api/oauth/usage", &claude_token()?).await?;
            let live = parse_claude(&v, now).ok_or("формат ответа claude не разобрался")?;
            Ok((live, claude_model_week(&v)))
        }
        _ => {
            let v = get_json("https://api.kimi.com/coding/v1/usages", &kimi_token(now)?).await?;
            Ok((parse_kimi(&v, now).ok_or("формат ответа kimi не разобрался")?, None))
        }
    }
}

/* ================= опрос ================= */

/// Как часто ходить: у порога — часто, в работе — средне, панель открыта без
/// работы — редко (ориентир 15 мин). `None` — не опрашивать ВОВСЕ: ходов нет,
/// приложение в фоне, идти в сеть незачем. `no_numbers` — чисел нет ни одного,
/// базовый замер нужен даже в тишине, иначе панель навсегда останется с
/// «unknown».
fn interval_ms(cfg: &Cfg, rung: Rung, active: bool, awake: bool, no_numbers: bool) -> Option<i64> {
    if no_numbers || matches!(rung, Rung::Warn | Rung::Routine | Rung::Queue | Rung::Stop) {
        return Some(cfg.poll_near_min * 60_000);
    }
    if active {
        return Some(cfg.poll_active_min * 60_000);
    }
    awake.then(|| cfg.poll_idle_min * 60_000)
}

/// Идёт ли работа: есть ход в полёте или сессию трогали недавно.
fn work_active(d: &Arc<Daemon>, now: i64) -> bool {
    d.sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .any(|s| s.status == crate::model::Status::Working || now - s.updated_at < 10 * 60_000)
}

/// Приложение на глазах у человека — панель открыта.
fn app_awake(d: &Arc<Daemon>) -> bool {
    use tauri::Manager;
    d.app
        .get_webview_window("main")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false)
}

/// Один заход к провайдеру: результат кладём в состояние, отказ — в откат.
async fn poll_one(d: &Arc<Daemon>, provider: &'static str, now: i64) {
    budget().note_request(now);
    let night = is_night_at(&cfg(d), now); // до захвата замка: настройки под своим
    let got = fetch(provider, now).await;
    let b = budget();
    let mut provs = b.provs.lock().unwrap_or_else(|e| e.into_inner());
    let p = provs.entry(provider).or_default();
    match got {
        Ok((live, model_week)) => {
            p.fails = 0;
            p.err = None;
            // сброс окна: старые точки темпа стали чужими
            if p.samples.back().is_some_and(|(_, u)| live.week_used + 2.0 < *u) {
                p.samples.clear();
            }
            p.samples.push_back((now, live.week_used));
            while p.samples.front().is_some_and(|(t, _)| now - *t > 26 * HOUR_MS) {
                p.samples.pop_front();
            }
            // база ночного потолка ставится на первом ночном замере и держится
            // до утра: ночь тратит тот же дневной бюджет, но со своим потолком
            if night {
                p.night_base.get_or_insert((now, live.week_used));
            } else {
                p.night_base = None;
            }
            p.live = Some(live);
            drop(provs);
            if provider == CLAUDE {
                // панель смотрит на официальные проценты — теперь они приезжают
                // из API, а не из мёртвого скрейпинга
                d.usage.set_official(
                    d,
                    Some(crate::usage::PctReset { pct: live.win_used.round() as i64, reset_at: live.win_reset }),
                    Some(crate::usage::PctReset { pct: live.week_used.round() as i64, reset_at: live.week_reset }),
                    model_week,
                    "api",
                );
            }
        }
        Err(why) => {
            p.fails = p.fails.saturating_add(1);
            p.next_at = now + backoff_ms(p.fails);
            let changed = p.err.as_deref() != Some(why.as_str());
            p.err = Some(why.clone());
            drop(provs);
            if changed {
                crate::log::line(&format!("[budget] {provider}: {why}"));
                if provider == CLAUDE {
                    d.usage.set_official_err(&format!("claude: {why}"));
                }
            }
        }
    }
}

/// Обязательный свежий запрос: перед стартом дорогой работы (длинный ход,
/// запуск параллельных сессий) и при пересечении порога лестницы. Там цена
/// ошибки высокая, и кэш «пятиминутной свежести» её не оправдывает.
pub async fn ensure_fresh(d: &Arc<Daemon>, max_age_ms: i64, why: &str) {
    if budget().busy.swap(true, Ordering::SeqCst) {
        return; // опрос уже идёт — второй такой же только заспамит
    }
    let now = now_ms();
    let stale: Vec<&'static str> = [CLAUDE, KIMI]
        .into_iter()
        .filter(|p| {
            let provs = budget().provs.lock().unwrap_or_else(|e| e.into_inner());
            provs
                .get(p)
                .and_then(|s| s.live.map(|l| now - l.at > max_age_ms))
                .unwrap_or(true)
        })
        .collect();
    for p in stale {
        crate::log::line(&format!("[budget] свежий запрос {p}: {why}"));
        poll_one(d, p, now_ms()).await;
    }
    budget().busy.store(false, Ordering::SeqCst);
}

/// Единственный опросчик приложения. Такт короткий, решение о походе в сеть —
/// внутри: далеко от порогов редко (ориентир — `pollIdleMin`), у порога и в
/// работе чаще, в полной тишине не ходим вовсе.
pub fn spawn_poller(d: &Arc<Daemon>) {
    let d = d.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            let c = cfg(&d);
            let now = now_ms();
            let active = work_active(&d, now);
            let awake = app_awake(&d);
            // обязательный свежий запрос уже в полёте — второй только заспамит
            if budget().busy.swap(true, Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }
            for p in [CLAUDE, KIMI] {
                let s = snapshot(p, &c, now);
                if now < s.next_at {
                    continue; // откат после отказа: повторять по кругу нельзя
                }
                let Some(every) = interval_ms(&c, s.rung, active, awake, s.live.is_none()) else {
                    continue;
                };
                if s.age >= every {
                    poll_one(&d, p, now_ms()).await;
                }
            }
            budget().busy.store(false, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}

/* ================= контракт наружу ================= */

/// Разобранное состояние провайдера: одна и та же оценка и для опросчика
/// (какая частота нужна), и для контракта наружу.
struct Snap {
    live: Option<Live>,
    err: Option<String>,
    plan: Option<Plan>,
    rate: Option<f64>,
    rung: Rung,
    reason: String,
    ttr: f64,
    age: i64,
    next_at: i64,
    night_spent: Option<f64>,
    night_hit: bool,
    prev_norm: Option<f64>,
}

fn snapshot(provider: &'static str, c: &Cfg, now: i64) -> Snap {
    let provs = budget().provs.lock().unwrap_or_else(|e| e.into_inner());
    let st = provs.get(provider);
    let base = Snap {
        live: None,
        err: st.and_then(|s| s.err.clone()),
        plan: None,
        rate: None,
        rung: Rung::Unknown,
        reason: st
            .and_then(|s| s.err.clone())
            .unwrap_or_else(|| "опросчик ещё не ходил за числами".into()),
        ttr: 0.0,
        age: i64::MAX,
        next_at: st.map(|s| s.next_at).unwrap_or(0),
        night_spent: None,
        night_hit: false,
        prev_norm: st.and_then(|s| s.prev_norm),
    };
    let (Some(st), Some(live)) = (st, st.and_then(|s| s.live)) else { return base };

    let remaining = (100.0 - live.week_used).max(0.0);
    let p = plan(remaining, now, live.week_reset, c, provider == KIMI);
    let ttr = ((live.week_reset - now).max(0) as f64) / DAY_MS as f64;
    let elapsed = if live.week_len > 0 {
        1.0 - ((live.week_reset - now).max(0) as f64 / live.week_len as f64)
    } else {
        0.0
    };
    let dead = in_dead_zone(elapsed, live.week_used);
    let samples: Vec<(i64, f64)> = st.samples.iter().copied().collect();
    let rate = if dead { None } else { rate_pct_day(&samples, now, live.week_used) };
    let why_no_rate = if dead {
        format!(
            "мёртвая зона: после сброса прошло {:.0}% окна и потрачено {:.0}% — темп ещё ничего не значит",
            elapsed * 100.0,
            live.week_used
        )
    } else {
        "точек мало: темп пока не о чем".to_string()
    };
    let (mut rung_v, mut reason) = rung(remaining, &p, rate, ttr, &why_no_rate);

    // Ночной потолок — ВТОРОЙ, независимый ограничитель: может сработать раньше
    // дневной нормы, чтобы человек не проснулся с пустой неделей из-за одной
    // зациклившейся цепочки.
    let night_spent = st.night_base.map(|(_, base)| (live.week_used - base).max(0.0));
    let night_hit = is_night_at(c, now) && night_spent.is_some_and(|s| s >= c.night_cap);
    if night_hit {
        rung_v = Rung::Stop;
        reason = format!(
            "ночной потолок {:.0}% недели исчерпан ({:.1}%) — до утра стоп",
            c.night_cap,
            night_spent.unwrap_or(0.0)
        );
    }
    Snap {
        live: Some(live),
        plan: Some(p),
        rate,
        rung: rung_v,
        reason,
        ttr,
        age: now - live.at,
        night_spent,
        night_hit,
        ..base
    }
}

/// Сводка по одному провайдеру. Чисел нет — ступень `unknown` С ТЕКСТОВОЙ
/// ПРИЧИНОЙ, а не отсутствующий ключ: молчание добытчика неотличимо от «всё
/// хорошо», этот урок в `usage.rs` уже выучен (`official_err`).
pub fn report_one(provider: &'static str, c: &Cfg, now: i64) -> Value {
    let s = snapshot(provider, c, now);
    let flat = provider == KIMI;
    let (Some(live), Some(p)) = (s.live, s.plan) else {
        return json!({
            "rung": Rung::Unknown,
            "reason": s.reason,
            "stale": true,
            "err": s.err,
            "bufferAvailable": false,
            "bufferReason": BUFFER_REASON,
        });
    };
    // Несвежесть показываем: «данные от такого-то времени», а не молча старое
    // за текущее. Порог — два спокойных интервала.
    let stale = s.age > 2 * c.poll_idle_min * 60_000 || s.err.is_some();
    json!({
        "weekPct": live.week_used,
        "weekLeftPct": (100.0 - live.week_used).max(0.0),
        "weekResetAt": live.week_reset,
        "windowPct": live.win_used,
        "windowResetAt": live.win_reset,
        "normPerDay": p.norm_per_day,
        "normNote": norm_note(s.prev_norm, p.norm_per_day),
        "lastDayPct": p.last_day,
        "lastDayHours": p.last_day_hours,
        "reservePct": p.reserve,
        "usablePct": p.usable,
        "ratePctDay": s.rate,
        "runwayDays": s.rate.filter(|r| *r > 0.0).map(|r| p.usable / r),
        "daysToReset": s.ttr,
        "rung": s.rung,
        "reason": s.reason,
        "at": live.at,
        "ageMs": s.age,
        "stale": stale,
        "staleNote": stale.then(|| format!("данные от {} назад", crate::util::fmt_reset_in(live.at))),
        "err": s.err,
        "bufferPct": if flat { 0.0 } else { c.claude_buffer },
        // Буфер тратится только по явному разрешению человека. Ночью разрешения
        // быть не может — человека нет, — поэтому «стоп» на границе резерва
        // окончателен.
        "bufferAvailable": false,
        "bufferReason": BUFFER_REASON,
        "nightSpentPct": s.night_spent,
        "nightCapHit": s.night_hit,
    })
}

/// Полный контракт бюджета: провайдеры, ночь, диагностика опросов.
pub fn report(d: &Arc<Daemon>) -> Value {
    let c = cfg(d);
    let now = now_ms();
    let out = json!({
        "providers": {
            CLAUDE: report_one(CLAUDE, &c, now),
            KIMI: report_one(KIMI, &c, now),
        },
        "night": {
            "active": is_night_at(&c, now),
            "from": c.night_from,
            "to": c.night_to,
            "capPct": c.night_cap,
            // Отдельного ночного кошелька НЕТ: ночь тратит тот же дневной
            // бюджет и послаблений в норме не получает.
            "wallet": "общий дневной бюджет, отдельного ночного нет",
            "bufferAvailable": false,
            "bufferReason": BUFFER_REASON,
        },
        "polls": { "lastHour": budget().requests_last_hour() },
    });
    // норму запоминаем после отчёта: падение говорим словами один раз
    let mut provs = budget().provs.lock().unwrap_or_else(|e| e.into_inner());
    for p in [CLAUDE, KIMI] {
        if let Some(n) = out.pointer(&format!("/providers/{p}/normPerDay")).and_then(Value::as_f64) {
            provs.entry(p).or_default().prev_norm = Some(n);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Момент по МСК — на нём построены все проверочные числа.
    fn msk(y: i32, m: u32, d: u32, h: u32, mi: u32) -> i64 {
        chrono::NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
            - MSK_MS
    }

    fn near(a: f64, b: f64, eps: f64, what: &str) {
        assert!((a - b).abs() <= eps, "{what}: {a:.4} против {b:.4}");
    }

    /// Проверочные числа владельца: claude 21.08 14:30 МСК, 44% остатка, сброс
    /// ср 26.08 00:59 → норма 8.79%/сут, доля последнего дня ≈9.16, резерв
    /// ≈14.16. Ручной счёт округлял норму до сотых ДО умножения (8.79 × 25/24 =
    /// 9.16), код несёт полную точность (9.15) — та же величина с точностью до
    /// порядка округлений, отсюда допуск в копейку.
    #[test]
    fn claude_reserve_matches_the_reference_numbers() {
        let c = Cfg::default();
        let now = msk(2026, 8, 21, 14, 30);
        let reset = msk(2026, 8, 26, 0, 59);
        let p = plan(44.0, now, reset, &c, false);
        near(p.norm_per_day, 8.79, 0.01, "норма claude");
        near(p.last_day, 9.16, 0.02, "доля последнего дня");
        near(p.reserve, 14.16, 0.02, "резерв claude");
        assert!(p.usable < 44.0 - 14.0, "рабочий бюджет = остаток минус резерв");
    }

    /// kimi: сброс во вторник 14:33, последний день всего 14,5 ч — полноценная
    /// доля ему не нужна, только плоский минимум 5%, буфера сверху нет.
    #[test]
    fn kimi_reserve_is_a_flat_floor() {
        let c = Cfg::default();
        let now = msk(2026, 8, 21, 14, 30);
        let reset = msk(2026, 8, 25, 14, 33);
        let p = plan(45.0, now, reset, &c, true);
        near(p.norm_per_day, 11.78, 0.01, "норма kimi");
        near(p.last_day, 5.0, 1e-9, "последний день kimi — плоский минимум");
        near(p.reserve, 5.00, 1e-9, "резерв kimi");
    }

    /// Хвост короче порога не считается днём: иначе «последним днём» станет
    /// часовой огрызок после полуночи и дневная норма выпадет из резерва.
    #[test]
    fn a_short_tail_is_glued_to_the_previous_day() {
        let now = msk(2026, 8, 21, 14, 30);
        let glued = segments(now, msk(2026, 8, 26, 0, 59), 6.0);
        assert_eq!(glued.len(), 5, "огрызок не отдельный день");
        near(glued[0], 9.5, 1e-9, "остаток текущих суток");
        near(*glued.last().unwrap(), 24.0 + 59.0 / 60.0, 1e-6, "хвост приклеен");

        // хвост длиннее порога — самостоятельный день
        let kept = segments(now, msk(2026, 8, 25, 14, 33), 6.0);
        assert_eq!(kept.len(), 5);
        near(*kept.last().unwrap(), 14.55, 1e-6, "14,5 ч — уже день");

        // и без склейки резерв обваливается: ровно та ошибка, на которой споткнулись
        let c = Cfg::default();
        let with = plan(44.0, now, msk(2026, 8, 26, 0, 59), &c, false);
        let without = plan(44.0, now, msk(2026, 8, 26, 0, 59), &Cfg { glue_h: 0.0, ..c }, false);
        assert!(
            without.reserve < with.reserve - 8.0,
            "без склейки резервом становится часовой огрызок: {:.2} против {:.2}",
            without.reserve,
            with.reserve
        );
    }

    /// Ступени — по запасу хода, а не по проценту остатка: при 20% остатка и
    /// спокойном темпе ступень «ок», при 60% и злом темпе — уже нет.
    #[test]
    fn rungs_follow_runway_not_percentage() {
        let c = Cfg::default();
        let now = msk(2026, 8, 21, 14, 30);
        let reset = msk(2026, 8, 26, 0, 59);

        let calm = plan(20.0, now, reset, &c, false);
        let (r, _) = rung(20.0, &calm, Some(1.0), 4.44, "нет темпа");
        assert_eq!(r, Rung::Ok, "20% остатка при 1%/сут — запас хода есть");

        let hot = plan(60.0, now, reset, &c, false);
        let (r, why) = rung(60.0, &hot, Some(40.0), 4.44, "нет темпа");
        assert_eq!(r, Rung::Queue, "60% остатка при 40%/сут — резерв уйдёт до сброса");
        assert!(why.contains("резерв"), "причина словами: {why}");

        // «стоп» — факт, а не прогноз: резерв начал расходоваться
        let (r, why) = rung(10.0, &hot, Some(0.1), 4.44, "нет темпа");
        assert_eq!(r, Rung::Stop);
        assert!(why.contains("резерв начал расходоваться"), "{why}");
    }

    /// Мёртвая зона молчит: сразу после сброса любой темп даёт «кончится завтра».
    #[test]
    fn the_dead_zone_stays_silent() {
        assert!(in_dead_zone(0.02, 30.0), "прошло 2% окна — рано");
        assert!(in_dead_zone(0.50, 1.0), "потрачено 1% — не о чем");
        assert!(!in_dead_zone(0.50, 30.0), "полокна и треть расхода — уже разговор");

        let c = Cfg::default();
        let p = plan(90.0, msk(2026, 8, 21, 14, 30), msk(2026, 8, 26, 0, 59), &c, false);
        let (r, why) = rung(90.0, &p, None, 4.44, "мёртвая зона: рано судить");
        assert_eq!(r, Rung::Unknown);
        assert!(why.contains("мёртвая зона"), "причина обязана быть текстом: {why}");
    }

    /// Перерасход дня не съедает завтрашний молча: остаток делится на оставшиеся
    /// часы заново, норма падает — и это сказано словами.
    #[test]
    fn overspend_lowers_the_norm_and_says_so() {
        let c = Cfg::default();
        let reset = msk(2026, 8, 26, 0, 59);
        let before = plan(44.0, msk(2026, 8, 21, 14, 30), reset, &c, false);
        let after = plan(30.0, msk(2026, 8, 21, 20, 30), reset, &c, false);
        assert!(after.norm_per_day < before.norm_per_day, "норма обязана упасть");
        let note = norm_note(Some(before.norm_per_day), after.norm_per_day).expect("словами");
        assert!(note.contains("норма упала"), "{note}");
        assert!(norm_note(Some(8.8), 8.79).is_none(), "дрожь в сотых — не новость");
    }

    /// Отказ (429 и любой другой) уважаем экспоненциальным откатом, а не
    /// повтором по кругу. 429 тут — отказ эндпоинта, НЕ «лимит исчерпан».
    #[test]
    fn refusals_back_off_exponentially() {
        assert_eq!(backoff_ms(1), 2 * 60_000);
        assert_eq!(backoff_ms(2), 4 * 60_000);
        assert!(backoff_ms(3) > backoff_ms(2));
        assert_eq!(backoff_ms(30), 60 * 60_000, "потолок — час");
        assert!(backoff_ms(1) >= 60_000, "первый отказ уже отодвигает поход");
    }

    /// Поставить провайдеру состояние руками: опросчик в тестах не бегает, а
    /// имя берём своё — чтобы не толкаться с живыми claude/kimi.
    fn install(name: &'static str, p: Prov) {
        budget().provs.lock().unwrap_or_else(|e| e.into_inner()).insert(name, p);
    }

    fn live_at(at: i64, used: f64, reset: i64) -> Live {
        Live { week_used: used, week_reset: reset, win_used: 0.0, win_reset: 0, week_len: 7 * DAY_MS, at }
    }

    /// Несвежесть показываем словами, а не выдаём старое за текущее; чисел нет —
    /// ступень `unknown` С ПРИЧИНОЙ, а не отсутствующий ключ.
    #[test]
    fn stale_numbers_are_marked() {
        let c = Cfg::default();
        let now = now_ms();
        install(
            "проба-несвежесть",
            Prov { live: Some(live_at(now - 90 * 60_000, 56.0, now + 3 * DAY_MS)), ..Prov::default() },
        );
        let v = report_one("проба-несвежесть", &c, now);
        assert_eq!(v["stale"], json!(true), "полтора часа без опроса — несвежо");
        assert!(v["staleNote"].as_str().is_some_and(|s| s.contains("данные от")), "{v}");
        assert!(v["at"].as_i64().is_some(), "момент получения чисел виден");

        install(
            "проба-пусто",
            Prov { err: Some("токен kimi протух — нет свежих чисел".into()), ..Prov::default() },
        );
        let v = report_one("проба-пусто", &c, now);
        assert_eq!(v["rung"], json!("unknown"));
        assert!(v["reason"].as_str().unwrap_or_default().contains("протух"), "{v}");

        // протухший токен kimi — это «нет свежих чисел», а не последнее известное
        assert!(!kimi_token(now + 10 * 365 * DAY_MS).unwrap_err().is_empty());
    }

    /// Ночной потолок — второй, независимый ограничитель: срабатывает раньше
    /// дневной нормы, и буфер ночью недоступен явным состоянием.
    #[test]
    fn the_night_cap_fires_before_the_daily_norm() {
        let c = Cfg {
            night_from: "00:00".into(),
            night_to: "23:59".into(),
            night_cap: 15.0,
            ..Cfg::default()
        };
        let now = now_ms();
        install(
            "проба-ночь",
            Prov {
                live: Some(live_at(now, 30.0, now + 4 * DAY_MS)),
                night_base: Some((now - 4 * HOUR_MS, 10.0)),
                ..Prov::default()
            },
        );
        let v = report_one("проба-ночь", &c, now);
        assert_eq!(v["nightSpentPct"], json!(20.0), "за ночь потрачено 20% недели");
        assert_eq!(v["nightCapHit"], json!(true));
        assert_eq!(v["rung"], json!("stop"), "70% остатка, а всё равно стоп");
        assert!(v["reason"].as_str().unwrap_or_default().contains("ночной потолок"), "{v}");
        assert_eq!(v["bufferAvailable"], json!(false));
        assert_eq!(v["bufferReason"], json!(BUFFER_REASON));
    }

    /// Счётчик запросов за час: ответ на «не заспамил ли» — число.
    #[test]
    fn requests_are_counted_per_hour() {
        let b = Budget::new();
        let now = now_ms();
        b.note_request(now - 2 * HOUR_MS); // старьё выпадает
        b.note_request(now - 10 * 60_000);
        b.note_request(now);
        assert_eq!(b.requests_last_hour(), 2);
    }

    /// Границы ночи — настройка, а не часы в коде; окно через полночь.
    #[test]
    fn night_window_wraps_over_midnight() {
        let c = Cfg { night_from: "23:00".into(), night_to: "08:00".into(), ..Cfg::default() };
        let at = |h: u32, mi: u32| {
            let today: chrono::DateTime<chrono::Local> =
                chrono::DateTime::from_timestamp_millis(now_ms()).unwrap().into();
            use chrono::TimeZone;
            chrono::Local
                .with_ymd_and_hms(
                    chrono::Datelike::year(&today),
                    chrono::Datelike::month(&today),
                    chrono::Datelike::day(&today),
                    h,
                    mi,
                    0,
                )
                .unwrap()
                .timestamp_millis()
        };
        assert!(is_night_at(&c, at(23, 30)));
        assert!(is_night_at(&c, at(3, 0)));
        assert!(!is_night_at(&c, at(12, 0)));
        // свои границы человека
        let own = Cfg { night_from: "01:00".into(), night_to: "06:00".into(), ..c.clone() };
        assert!(!is_night_at(&own, at(23, 30)));
        assert!(is_night_at(&own, at(3, 0)));
        // мусор в настройке не превращает день в ночь
        let junk = Cfg { night_from: "хх".into(), ..c };
        assert!(!is_night_at(&junk, at(3, 0)));
    }

    /// Ночь послаблений не получает: та же норма, буфер недоступен явно.
    #[test]
    fn night_has_no_wallet_of_its_own() {
        let c = Cfg::default();
        let now = msk(2026, 8, 21, 2, 30);
        let reset = msk(2026, 8, 26, 0, 59);
        let day = plan(44.0, msk(2026, 8, 21, 2, 30), reset, &c, false);
        assert!(day.norm_per_day > 0.0);
        // резерв ночью тот же: ночного кошелька нет
        assert_eq!(plan(44.0, now, reset, &c, false).reserve, day.reserve);
        assert_eq!(BUFFER_REASON, "буфер доступен только с твоего разрешения");
    }

    /// Частоты опроса: далеко от порогов — редко, ходов нет и приложение в
    /// фоне — не ходим вовсе.
    #[test]
    fn polling_frequency_follows_need() {
        let c = Cfg::default();
        assert_eq!(interval_ms(&c, Rung::Ok, false, false, false), None, "тишина — в сеть незачем");
        assert_eq!(interval_ms(&c, Rung::Ok, false, true, false), Some(15 * 60_000), "панель открыта");
        assert_eq!(interval_ms(&c, Rung::Ok, true, false, false), Some(3 * 60_000), "идёт работа");
        assert_eq!(interval_ms(&c, Rung::Warn, false, false, false), Some(2 * 60_000), "у порога");
        assert_eq!(interval_ms(&c, Rung::Stop, false, false, false), Some(2 * 60_000), "у стены");
        assert_eq!(interval_ms(&c, Rung::Ok, false, false, true), Some(2 * 60_000), "чисел ещё нет");
    }

    /// Темп — по скользящему окну; берём худший из двух.
    #[test]
    fn rate_takes_the_worst_window() {
        let now = now_ms();
        let s = vec![
            (now - 24 * HOUR_MS, 10.0), // за сутки +20 → 20%/сут
            (now - 3 * HOUR_MS, 22.0),  // за 3 часа +8 → 64%/сут
        ];
        let r = rate_pct_day(&s, now, 30.0).expect("темп есть");
        near(r, 64.0, 0.5, "рывок последних часов виден");
        // одна точка минуту назад — это шум, а не темп
        assert_eq!(rate_pct_day(&[(now - 60_000, 29.0)], now, 30.0), None);
    }

    /// Формат живых ответов: проценты уже в процентах, не в долях.
    #[test]
    fn live_response_shapes_parse() {
        let v = json!({
            "five_hour": { "utilization": 14.0, "resets_at": "2026-08-21T14:10:00.297049+00:00" },
            "seven_day": { "utilization": 56.0, "resets_at": "2026-08-25T22:00:00.297071+00:00" },
            "limits": [{ "kind": "weekly_scoped", "percent": 3, "resets_at": "2026-08-25T22:00:00+00:00",
                          "scope": { "model": { "display_name": "Fable" } } }],
        });
        let live = parse_claude(&v, 1).expect("claude");
        near(live.week_used, 56.0, 1e-9, "utilization — проценты");
        assert!(live.week_reset > 0 && live.win_reset > 0);
        assert_eq!(claude_model_week(&v).unwrap().model, "Fable");

        let k = json!({
            "usage": { "used": 55, "limit": 100, "resetTime": 1787312046 },
            "limits": [{ "window": { "duration": 300, "timeUnit": "minute" },
                          "detail": { "used": 12, "limit": 100, "resetTime": 1787312046 } }],
            "parallel": { "limit": 3 },
        });
        let live = parse_kimi(&k, 1).expect("kimi");
        near(live.week_used, 55.0, 1e-9, "шкала 100");
        near(live.win_used, 12.0, 1e-9, "окно 300 минут");
        assert!(live.week_reset > 1_700_000_000_000, "секунды разложены в мс");
    }
}

