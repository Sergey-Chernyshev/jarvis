//! Учёт usage — слой A: транскрипты ~/.claude/projects/**∕*.jsonl.
//! Бесплатно, любой план, покрывает и API-проекты (транскрипт пишется всегда).
//!
//! usage-блок есть в каждом ходе ассистента; чанки стрима дублируют запись с
//! одним message.id и идентичным usage — дедуп по message.id (проверено).
//! Деньги: прайс per-model; для подписки это «сколько стоило бы по API» —
//! различаем биллинг per-проект: .claude/settings.json с API-ключом → 'api:<host>'.
//!
//! ~/.jarvis/usage.json — кэш v5: профили Codex и исключение скопированной fork-истории.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::daemon::Daemon;
use crate::util::*;

const WINDOW_MS: i64 = 5 * 60 * 60 * 1000; // 5ч-окно подписочных лимитов
const STATE_V: i64 = 5; // Rebuild old fork-inflated totals, even for unchanged files.
const DAY_MS: i64 = 86_400_000;
const CODEX_SCAN_CHUNK: u64 = 8 * 1024 * 1024;

/// Горизонт хранения агрегатов. `hours`/`sessions` не чистились никогда и росли
/// линейно временем — за 94 дня 1378 часовых записей. Читают их максимум на
/// неделю назад («сегодня»/«неделя»), 400 дней — с запасом на «а год назад» и
/// потолок файла примерно на нынешнем размере.
///
/// `offsets` НЕ чистим намеренно: смещение — единственная защита от двойного
/// счёта у Codex и Kimi (дедупа по id сообщения там нет, в отличие от Claude).
/// Забыть смещение живого файла значит посчитать его расход заново; пара
/// десятков килобайт этого не стоят.
const RETAIN_DAYS: i64 = 400;

/// Выбросить агрегаты старше горизонта. `true` — что-то удалили (значит файл
/// пора переписать).
fn prune(state: &mut State, now: i64) -> bool {
    let cutoff = now - RETAIN_DAYS * DAY_MS;
    // Час из ключа "YYYY-MM-DDTHH|модель|проект|биллинг" — тем же разбором, что
    // и в `range_hours`: ключ, который там не читается, тут не хранится.
    let hour_ts = |key: &str| {
        let hour = key.split('|').next().unwrap_or("");
        chrono::DateTime::parse_from_rfc3339(&format!("{hour}:00:00Z"))
            .ok()
            .map(|d| d.timestamp_millis())
    };
    let before = state.hours.len() + state.sessions.len();
    state.hours.retain(|key, _| hour_ts(key).is_some_and(|ts| ts >= cutoff));
    state.sessions.retain(|_, s| s.last >= cutoff);
    before != state.hours.len() + state.sessions.len()
}

/// $/1M токенов; кэш: запись ×1.25 input, чтение ×0.1 input (подход ccusage).
fn price(model: &str) -> (f64, f64) {
    match model {
        "Opus" | "Fable" => (15.0, 75.0), // у Fable публичного прайса нет — как Opus
        "Haiku" => (1.0, 5.0),
        "GPT-5" | "Codex" => (1.25, 10.0), // ОЦЕНКА OpenAI gpt-5-класс ($/1M)
        model if model.starts_with("gpt-5") || model.contains("codex") => (1.25, 10.0), // preserve the same family estimate for exact model slugs
        // ОЦЕНКА Moonshot (то же, что backend::kimi::price) — иначе Kimi считался
        // бы по дефолту Sonnet и врал в пять раз.
        "K3" | "K3-256k" | "K2.7 Coding" | "K2.7 Coding Highspeed" => (0.6, 2.5),
        _ => (3.0, 15.0), // Sonnet и дефолт
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default)]
struct Tok {
    #[serde(rename = "in")]
    input: f64,
    out: f64,
    cw: f64,
    cr: f64,
    /// A subset of `out`, for display only; never added to total or cost.
    reasoning: f64,
}

impl Tok {
    fn total(&self) -> f64 {
        self.input + self.out + self.cw + self.cr
    }
    fn cost(&self, model: &str) -> f64 {
        let (pin, pout) = price(model);
        (self.input * pin + self.out * pout + self.cw * pin * 1.25 + self.cr * pin * 0.1) / 1e6
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct HourAgg {
    #[serde(flatten)]
    tok: Tok,
    cost: f64,
    n: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct SessionAgg {
    project: String,
    billing: String,
    model: String,
    #[serde(flatten)]
    tok: Tok,
    cost: f64,
    n: f64,
    first: i64,
    last: i64,
    instance_id: Option<String>,
    instance_label: Option<String>,
    provider_home: Option<String>,
    provider_session_id: Option<String>,
    machine: Option<String>,
}

impl SessionAgg {
    fn json(&self) -> Value {
        let input_total = self.tok.input + self.tok.cw + self.tok.cr;
        serde_json::json!({
            "tok": self.tok.total(), "cost": self.cost, "billing": self.billing, "model": self.model,
            "inputTokens": self.tok.input, "outputTokens": self.tok.out,
            "cacheReadTokens": self.tok.cr, "cacheWriteTokens": self.tok.cw,
            "reasoningTokens": self.tok.reasoning,
            "cacheHitPct": if input_total > 0.0 { self.tok.cr / input_total * 100.0 } else { 0.0 },
            "requests": self.n, "firstAt": self.first, "lastAt": self.last,
            // These are API-equivalent estimates, including for subscription use.
            // A family-level price table cannot represent an actual invoice.
            "costEstimated": true, "costBasis": "model-family-estimate",
            "source": "local-transcripts",
            "instanceId":self.instance_id,"instanceLabel":self.instance_label,
            "providerHome":self.provider_home,"providerSessionId":self.provider_session_id,"machine":self.machine,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct WindowAgg {
    start: i64,
    tokens: f64,
    cost: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    offsets: HashMap<String, u64>,
    /// "YYYY-MM-DD HH|model|project|billing" → агрегат часа.
    hours: HashMap<String, HourAgg>,
    sessions: HashMap<String, SessionAgg>,
    /// Keep cumulative counters and model alongside byte offsets across restarts.
    codex_cursors: HashMap<String, CodexCursor>,
    window: WindowAgg,
    backfilled: bool,
    #[serde(rename = "msgIds")]
    msg_ids: Vec<String>,
    v: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct CodexCounters {
    input: f64,
    cached: f64,
    output: f64,
    reasoning: f64,
}

impl CodexCounters {
    fn parse(value: Option<&Value>) -> Option<Self> {
        let value = value?.as_object()?;
        if !["input_tokens", "output_tokens"].iter().any(|key| value.contains_key(*key)) {
            return None;
        }
        let number = |key: &str| value.get(key).and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n >= 0.0).unwrap_or(0.0);
        Some(Self {
            input: number("input_tokens"), cached: number("cached_input_tokens"),
            output: number("output_tokens"), reasoning: number("reasoning_output_tokens"),
        })
    }

    fn delta(self, previous: Self) -> Self {
        Self {
            input: (self.input - previous.input).max(0.0),
            cached: (self.cached - previous.cached).max(0.0),
            output: (self.output - previous.output).max(0.0),
            reasoning: (self.reasoning - previous.reasoning).max(0.0),
        }
    }

    fn tokens(self) -> Tok {
        let cached = self.cached.min(self.input);
        Tok {
            input: self.input - cached, cr: cached, out: self.output,
            reasoning: self.reasoning.min(self.output), cw: 0.0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct CodexCursor {
    model: String,
    total: Option<CodexCounters>,
    last_event: String,
    skip_partial_line: bool,
    scope: crate::rollout_scope::RolloutScope,
}

impl CodexCursor {
    /// Codex repeats token_count when publishing rate limits. The cumulative
    /// counter is the authority; `last` alone would count that request twice.
    /// See ccusage/ccusage#824 and openai/codex's TUI token_usage.rs.
    fn take_usage(&mut self, info: &Value, timestamp: &str) -> Option<Tok> {
        let total = CodexCounters::parse(info.get("total_token_usage"));
        let last = CodexCounters::parse(info.get("last_token_usage"));
        let raw = if let Some(total) = total {
            let previous = self.total.replace(total);
            match previous {
                Some(previous) if previous == total => return None,
                // A reset/compaction can lower counters. Account only for the
                // explicitly reported request, never an invented negative delta.
                Some(previous) if total.input < previous.input || total.output < previous.output => last?,
                Some(previous) => total.delta(previous),
                // A forked/resumed log can start with inherited cumulative use.
                None if self.scope.forked && last.is_none() => return None,
                None => last.unwrap_or(total),
            }
        } else {
            let last = last?;
            let signature = format!("{timestamp}:{}", serde_json::to_string(&last).unwrap_or_default());
            if self.last_event == signature {
                return None;
            }
            self.last_event = signature;
            // If cumulative reporting resumes, already consumed fallback records
            // must be part of its baseline as well.
            if let Some(total) = self.total.as_mut() {
                total.input += last.input;
                total.cached += last.cached;
                total.output += last.output;
                total.reasoning += last.reasoning;
            }
            last
        };
        let tokens = raw.tokens();
        (tokens.total() > 0.0).then_some(tokens)
    }
}

/* -------- официальные лимиты подписки -------- */

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PctReset {
    pub pct: i64,
    pub reset_at: i64,
}

/// Недельное окно конкретной модели: «Current week (Fable): 54% …».
///
/// Имя — какое пришло: раньше здесь было зашито «Sonnet only», и с приходом
/// Fable строка молча исчезла из панели. Зашитая модель протухает с каждым
/// релизом моделей — имя обязано быть данными.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelWeek {
    pub model: String,
    pub pct: i64,
    pub reset_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub plan: Option<String>,
    pub email: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficialInfo {
    pub session: Option<PctReset>,
    pub week: Option<PctReset>,
    pub week_model: Option<ModelWeek>,
    /// Чей это `/usage`: "local" или имя узла — человек должен видеть, про чью
    /// авторизацию эти проценты.
    pub source: String,
    pub instance_id: Option<String>,
    pub provider_home: Option<String>,
    pub provider: String,
    pub at: i64,
    pub account: Account,
}

#[derive(Debug, Clone)]
struct Official {
    source:String,
    session: Option<PctReset>,
    week: Option<PctReset>,
    week_model: Option<ModelWeek>,
    at: i64,
    instance_id: Option<String>,
    provider_home: Option<String>,
}

/// Keep the refresh reservation through publication and release it on cancellation.
struct OfficialFetchGuard<'a>(&'a AtomicBool);

impl<'a> OfficialFetchGuard<'a> {
    fn acquire(busy: &'a AtomicBool) -> Option<Self> {
        if busy.swap(true, Ordering::SeqCst) { None } else { Some(Self(busy)) }
    }
}

impl Drop for OfficialFetchGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// A successful process exit is not proof that this CLI supports headless /usage.
/// Validate each source before selecting it, so an unsupported local CLI cannot
/// mask usable limits from a remote node. Never publish a cost summary as a quota.
fn official_candidate(
    source: &str,
    result: Result<String, String>,
    errors: &mut Vec<String>,
) -> Option<(Official, String)> {
    match result {
        Ok(text) => match parse_official(&text) {
            Some((session, week, week_model)) => {
                return Some((Official { source:source.into(),session, week, week_model, at: now_ms(),instance_id:None,provider_home:None }, source.into()));
            }
            None => errors.push(format!(
                "{source}: CLI не вернул лимиты подписки; проверь /usage в интерактивном Claude Code"
            )),
        },
        Err(error) => errors.push(format!("{source}: {error}")),
    }
    None
}

/// Ринг последних message.id: дедуп с сохранением порядка вставки (как JS Set) —
/// при обрезке выбрасываются именно самые старые id, а не произвольные.
#[derive(Default)]
struct OrderedRing {
    set: HashSet<String>,
    order: std::collections::VecDeque<String>,
}

impl OrderedRing {
    fn from_iter(ids: impl IntoIterator<Item = String>) -> Self {
        let mut ring = Self::default();
        for id in ids {
            ring.insert(id);
        }
        ring
    }

    /// false — id уже встречался.
    fn insert(&mut self, id: String) -> bool {
        if !self.set.insert(id.clone()) {
            return false;
        }
        self.order.push_back(id);
        true
    }

    fn len(&self) -> usize {
        self.order.len()
    }

    /// Оставить последние n (старые уходят первыми).
    fn trim_to(&mut self, n: usize) {
        while self.order.len() > n {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
    }

    fn last_n(&self, n: usize) -> Vec<String> {
        self.order
            .iter()
            .skip(self.order.len().saturating_sub(n))
            .cloned()
            .collect()
    }
}

pub struct Usage {
    state: Mutex<State>,
    msg_seen: Mutex<OrderedRing>,
    billing_cache: Mutex<HashMap<String, String>>,
    official: Mutex<Option<Official>>,
    /// Откуда приехали лимиты: "local" или имя узла. Пусто — ниоткуда.
    official_source: Mutex<String>,
    official_busy: AtomicBool,
    /// Почему лимитов нет — по всем источникам разом. Молчание добытчика
    /// неотличимо от «всё хорошо», и его пришлось запретить.
    official_err: Mutex<Option<String>>,
    scanning: AtomicBool,
    persist_pending: AtomicBool,
}

fn state_file() -> PathBuf {
    jarvis_dir().join("usage.json")
}

fn projects_dir() -> PathBuf {
    claude_dir().join("projects")
}

#[derive(Clone)]
struct CodexSourceFile {
    path: String,
    key: String,
    sid: String,
    instance_id: String,
    label: String,
    home: String,
}

fn kimi_sessions_dir() -> PathBuf {
    crate::backend::kimi::kimi_home().join("sessions")
}

/// Все *.jsonl под каталогом, на любой глубине.
fn walk_jsonl(dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        if p.is_dir() {
            walk_jsonl(&p, out);
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            out.push(p.to_string_lossy().into_owned());
        }
    }
}

/// cwd + session_id из ПЕРВОЙ строки rollout (session_meta). Нужно при
/// инкрементальном скане: session_meta уже ниже from_offset, иначе токены
/// уходят в "unknown"/"другое".
fn codex_meta_head(file: &str) -> (Option<String>, String) {
    use std::io::BufRead;
    let Ok(f) = fs::File::open(file) else { return (None, "unknown".into()) };
    let mut first = String::new();
    if std::io::BufReader::new(f.take(128 * 1024)).read_line(&mut first).is_err() {
        return (None, "unknown".into());
    }
    if let Ok(v) = serde_json::from_str::<Value>(first.trim()) {
        if let Some(p) = v.get("payload") {
            let cwd = p.get("cwd").and_then(Value::as_str).map(String::from);
            let sid = p.get("id").and_then(Value::as_str).unwrap_or("unknown").to_string();
            return (cwd, sid);
        }
    }
    (None, "unknown".into())
}

/// cwd + session_id для `<...>/sessions/<wd_*>/<sid>/agents/<агент>/wire.jsonl`.
/// sid — имя каталога сессии, cwd — из `state.json` рядом (на два уровня выше
/// wire). state.json живёт в двух версиях: v2 с `cwd`, legacy v1 с `workDir`;
/// в живых сессиях встречаются обе, поэтому читаем обе.
fn kimi_meta_for(file: &Path) -> (Option<String>, String) {
    let Some(dir) = file.parent().and_then(Path::parent).and_then(Path::parent) else {
        return (None, "unknown".into());
    };
    let sid = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into());
    let cwd = fs::read_to_string(dir.join("state.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| {
            v.get("cwd")
                .or_else(|| v.get("workDir"))
                .and_then(Value::as_str)
                .map(String::from)
        });
    (cwd, sid)
}

impl Usage {
    pub fn load() -> Self {
        let raw = fs::read_to_string(state_file()).ok();
        let mut state: State = raw.as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();
        if state.v != STATE_V {
            // Keep the previous cache recoverable: some source transcripts may
            // already have been removed. Never overwrite an earlier backup.
            if state.v > 0 {
                use std::io::Write;
                let backup = state_file().with_extension(format!("v{}.json", state.v));
                if let (Some(raw), Ok(mut file)) = (raw, fs::OpenOptions::new().write(true).create_new(true).open(backup)) {
                    let _ = file.write_all(raw.as_bytes());
                }
            }
            // схема агрегатов изменилась — пересобираем с нуля (backfill ~1.5с)
            state = State { v: STATE_V, ..Default::default() };
        }
        let msg_seen = OrderedRing::from_iter(state.msg_ids.iter().cloned());
        Self {
            state: Mutex::new(state),
            msg_seen: Mutex::new(msg_seen),
            billing_cache: Mutex::new(HashMap::new()),
            official: Mutex::new(None),
            official_source: Mutex::new(String::new()), official_busy: AtomicBool::new(false),
            official_err: Mutex::new(None),
            scanning: AtomicBool::new(false),
            persist_pending: AtomicBool::new(false),
        }
    }

    fn persist(self: &Arc<Self>) {
        if self.persist_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let u = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            u.persist_pending.store(false, Ordering::SeqCst);
            // Never snapshot counters while their matching file offsets are
            // still being advanced by a long backfill.
            if u.scanning.swap(true, Ordering::SeqCst) {
                u.persist();
                return;
            }
            let json = {
                let mut state = u.state.lock().unwrap();
                state.msg_ids = u.msg_seen.lock().unwrap().last_n(3000); // ринг последних id
                serde_json::to_string(&*state).ok()
            };
            u.scanning.store(false, Ordering::SeqCst);
            if let Some(json) = json {
                let _ = fs::create_dir_all(jarvis_dir());
                let _ = fs::write(state_file(), json);
            }
        });
    }

    /* ---------- разбор транскриптов ---------- */

    /// 'plan' либо 'api:<host>' — конфиги бывают разные (прокси, шлюзы),
    /// различаем по hostname из ANTHROPIC_BASE_URL.
    fn detect_billing(&self, cwd: Option<&str>) -> String {
        let Some(cwd) = cwd else { return "plan".into() };
        if let Some(hit) = self.billing_cache.lock().unwrap().get(cwd) {
            return hit.clone();
        }
        let mut mode = "plan".to_string();
        for f in [
            Path::new(cwd).join(".claude/settings.json"),
            Path::new(cwd).join(".claude/settings.local.json"),
        ] {
            let Some(s) = fs::read_to_string(&f)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            else {
                continue;
            };
            let env = s.get("env").cloned().unwrap_or(Value::Null);
            // truthy как в JS: пустая строка/null/false/0 — не ключ
            let has_key = js_truthy(env.get("ANTHROPIC_API_KEY"))
                || js_truthy(env.get("ANTHROPIC_AUTH_TOKEN"))
                || js_truthy(s.get("apiKeyHelper"));
            let base = env
                .get("ANTHROPIC_BASE_URL")
                .and_then(Value::as_str)
                .filter(|b| !b.is_empty());
            if has_key || base.is_some() {
                let host = base
                    .and_then(url_host)
                    .unwrap_or_else(|| "api.anthropic.com".into());
                mode = format!("api:{host}");
                break;
            }
        }
        self.billing_cache.lock().unwrap().insert(cwd.to_string(), mode.clone());
        mode
    }

    fn add_record(state: &mut State, ts: i64, model: &str, project: &str, billing: &str, sid: &str, u: Tok) {
        let c = u.cost(model);
        let hour = chrono::DateTime::from_timestamp_millis(ts)
            .unwrap_or_default()
            .format("%Y-%m-%dT%H")
            .to_string();
        let key = format!("{hour}|{model}|{project}|{billing}");
        let h = state.hours.entry(key).or_default();
        h.tok.input += u.input;
        h.tok.out += u.out;
        h.tok.cw += u.cw;
        h.tok.cr += u.cr;
        h.tok.reasoning += u.reasoning;
        h.cost += c;
        h.n += 1.0;

        let s = state.sessions.entry(sid.to_string()).or_insert_with(|| SessionAgg {
            project: project.into(),
            billing: billing.into(),
            model: model.into(),
            first: ts,
            last: ts,
            ..Default::default()
        });
        s.tok.input += u.input;
        s.tok.out += u.out;
        s.tok.cw += u.cw;
        s.tok.cr += u.cr;
        s.tok.reasoning += u.reasoning;
        s.cost += c;
        s.n += 1.0;
        s.model = model.into();
        s.billing = billing.into();
        s.project = project.into();
        s.last = s.last.max(ts);
        s.first = s.first.min(ts);

        // 5ч-окно: новое окно открывает первый запрос после истечения прошлого
        if state.window.start == 0 || ts >= state.window.start + WINDOW_MS {
            if ts > state.window.start {
                state.window = WindowAgg { start: ts, tokens: 0.0, cost: 0.0 };
            }
        }
        if ts >= state.window.start && ts < state.window.start + WINDOW_MS {
            state.window.tokens += u.total();
            state.window.cost += c;
        }
    }

    fn parse_file_part(&self, file: &str, from_offset: u64) -> u64 {
        let Ok(meta) = fs::metadata(file) else { return from_offset };
        let size = meta.len();
        if size <= from_offset {
            return from_offset;
        }
        let Ok(mut f) = fs::File::open(file) else { return from_offset };
        if f.seek(SeekFrom::Start(from_offset)).is_err() {
            return from_offset;
        }
        let mut buf = Vec::with_capacity((size - from_offset) as usize);
        if f.read_to_end(&mut buf).is_err() {
            return from_offset;
        }
        let Some(last_nl) = buf.iter().rposition(|byte| *byte == b'\n') else { return from_offset };
        let consumed = (last_nl + 1) as u64;
        let text = String::from_utf8_lossy(&buf[..last_nl]);

        let mut cwd: Option<String> = None;
        for line in text.split('\n') {
            if !line.contains("\"assistant\"") || !line.contains("\"usage\"") {
                if cwd.is_none() && line.contains("\"cwd\"") {
                    if let Ok(v) = serde_json::from_str::<Value>(line) {
                        cwd = v.get("cwd").and_then(Value::as_str).map(String::from);
                    }
                }
                continue;
            }
            let Ok(e) = serde_json::from_str::<Value>(line) else { continue };
            if e.get("type").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let Some(m) = e.get("message") else { continue };
            let Some(u0) = m.get("usage") else { continue };
            let Some(mid) = m.get("id").and_then(Value::as_str) else { continue };
            if !self.msg_seen.lock().unwrap().insert(mid.to_string()) {
                continue;
            }
            if cwd.is_none() {
                cwd = e.get("cwd").and_then(Value::as_str).map(String::from);
            }
            let ts = e
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(crate::transcript::parse_ts)
                .unwrap_or_else(now_ms);
            let num = |k: &str| u0.get(k).and_then(Value::as_f64)
                .filter(|n| n.is_finite() && *n >= 0.0).unwrap_or(0.0);
            let entry_cwd = e.get("cwd").and_then(Value::as_str).map(String::from).or(cwd.clone());
            let billing = self.detect_billing(entry_cwd.as_deref());
            let model = friendly_model_or_other(m.get("model").and_then(Value::as_str).unwrap_or(""));
            let project = entry_cwd.as_deref().map(basename).unwrap_or_else(|| "другое".into());
            let sid = e.get("sessionId").and_then(Value::as_str).unwrap_or("unknown");
            Self::add_record(
                &mut self.state.lock().unwrap(),
                ts,
                &model,
                &project,
                &billing,
                sid,
                Tok {
                    input: num("input_tokens"),
                    out: num("output_tokens"),
                    cw: num("cache_creation_input_tokens"),
                    cr: num("cache_read_input_tokens"),
                    reasoning: 0.0,
                },
            );
        }
        from_offset + consumed
    }

    /// Все транскрипты Claude Code — обходом В ГЛУБИНУ, а не на два уровня.
    ///
    /// Хранилище стало вложенным: субагенты пишут в
    /// `projects/<проект>/<uuid сессии>/subagents/agent-*.jsonl`. Плоский обход
    /// видел 78 файлов из 1467 — то есть четверть запросов и половину токенов,
    /// и метрики недосчитывали ровно на сабагентах, которые жгут больше всех.
    /// Двойного счёта не будет: inline-формат `"isSidechain":true` в родительском
    /// транскрипте больше не пишется, а дедуп по `message.id` остаётся.
    fn list_transcripts() -> Vec<String> {
        let mut out = Vec::new();
        walk_jsonl(&projects_dir(), &mut out);
        out
    }

    /// backfill + инкрементальные сканы — одним и тем же путём (offsets решают).
    ///
    /// Пишем файл ТОЛЬКО когда что-то изменилось. Скан идёт раз в 30 с круглые
    /// сутки, а usage.json — это сотни килобайт: безусловная запись давала
    /// ~900 МБ на SSD в день у приложения, которое просто висит в менюбаре.
    /// Признак изменения тут же под рукой — сдвинулось смещение файла.
    pub fn scan(self: &Arc<Self>) {
        if self.scanning.swap(true, Ordering::SeqCst) {
            return;
        }
        for file in Self::list_transcripts() {
            let prev = self.state.lock().unwrap().offsets.get(&file).copied().unwrap_or(0);
            let next = self.parse_file_part(&file, prev);
            if next != prev {
                self.state.lock().unwrap().offsets.insert(file, next);
            }
        }
        // Codex rollouts: only new usage from cumulative token_count counters.
        let registry = crate::session_identity::registry();
        let mut codex_complete = registry.is_ok();
        let mut remaining = 256 * 1024 * 1024u64;
        for file in registry.as_ref().map(Self::list_codex_rollouts).unwrap_or_default() {
            let offset_key = format!("codex-session:{}", file.key);
            let mut at = self.state.lock().unwrap().offsets.get(&offset_key).copied().unwrap_or(0);
            let size = fs::metadata(&file.path).map(|meta| meta.len()).unwrap_or(at);
            while at < size && remaining >= CODEX_SCAN_CHUNK {
                let next = self.parse_codex_file_part_scoped(&file.path, at, Some(&file));
                if next <= at { break; }
                remaining = remaining.saturating_sub(next - at);
                self.state.lock().unwrap().offsets.insert(offset_key.clone(), next);
                at = next;
            }
            if at < size { codex_complete = false; }
        }
        self.scan_files(Self::list_kimi_wires(), Self::parse_kimi_file_part);
        {
            let mut seen = self.msg_seen.lock().unwrap();
            if seen.len() > 6000 {
                seen.trim_to(3000);
            }
        }
        self.state.lock().unwrap().backfilled = codex_complete;
        prune(&mut self.state.lock().unwrap(), now_ms());
        self.persist();
        self.scanning.store(false, Ordering::SeqCst);
    }

    /// Все rollout-файлы Codex: ~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl.
    fn list_codex_rollouts(registry: &crate::agent_instances::Registry) -> Vec<CodexSourceFile> {
        fn walk(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
            if depth > 5 || out.len() >= 20_000 { return; }
            let Ok(rd) = fs::read_dir(dir) else { return };
            for e in rd.filter_map(|e| e.ok()) {
                if out.len() >= 20_000 { break; }
                let p = e.path();
                let Ok(kind) = e.file_type() else { continue };
                if kind.is_dir() {
                    walk(&p, depth + 1, out);
                } else if kind.is_file() && p.extension().is_some_and(|x| x == "jsonl") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        for root in registry.roots(true) { walk(&root.path, 0, &mut files); }
        let mut seen = HashSet::new();
        let mut logical = HashMap::new();
        for path in files {
            let Ok(path) = path.canonicalize() else { continue };
            if !seen.insert(path.clone()) { continue; }
            let Some(instance) = registry.instance_for_transcript(&path) else { continue };
            let path_text = path.to_string_lossy().into_owned();
            let (_, sid) = codex_meta_head(&path_text);
            if sid == "unknown" || sid.is_empty() { continue; }
            let home = instance.canonical_home.to_string_lossy().into_owned();
            let key = crate::session_identity::key(&instance.id, &home, &sid, None);
            let modified = fs::metadata(&path).and_then(|meta| meta.modified()).unwrap_or(std::time::UNIX_EPOCH);
            let file = CodexSourceFile { path: path_text, key: key.clone(), sid, instance_id: instance.id.clone(), label: instance.label.clone(), home };
            let best = logical.entry(key).or_insert_with(|| (modified, file.clone()));
            if modified > best.0 { *best = (modified, file); }
        }
        let mut out: Vec<_> = logical.into_values().map(|(_, file)| file).collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }

    /// Разобрать список файлов своим парсером, подвинув смещения.
    /// `true` — хоть одно смещение сдвинулось, то есть состояние изменилось и
    /// файл придётся переписать.
    fn scan_files(&self, files: Vec<String>, parse: fn(&Self, &str, u64) -> u64) -> bool {
        let mut changed = false;
        for file in files {
            let prev = self.state.lock().unwrap().offsets.get(&file).copied().unwrap_or(0);
            let next = parse(self, &file, prev);
            if next != prev {
                self.state.lock().unwrap().offsets.insert(file, next);
                changed = true;
            }
        }
        changed
    }

    /// Разбор rollout Codex: дельта `event_msg.token_count.total_token_usage`,
    /// с fallback на `last_token_usage`. Модель и counters переживают сканы;
    /// cwd/sid — из `session_meta`. billing="codex". Маппинг в общий `Tok`:
    /// input_tokens включает cached (конвенция OpenAI) → input = input−cached,
    /// cr = cached, out = output (reasoning уже внутри), cw = 0.
    fn parse_codex_file_part(&self, file: &str, from_offset: u64) -> u64 {
        self.parse_codex_file_part_scoped(file, from_offset, None)
    }

    fn parse_codex_file_part_scoped(&self, file: &str, from_offset: u64, source: Option<&CodexSourceFile>) -> u64 {
        let Ok(meta) = fs::metadata(file) else { return from_offset };
        let size = meta.len();
        if size <= from_offset {
            return from_offset;
        }
        let Ok(mut f) = fs::File::open(file) else { return from_offset };
        if f.seek(SeekFrom::Start(from_offset)).is_err() {
            return from_offset;
        }
        let mut buf = Vec::with_capacity((size - from_offset).min(CODEX_SCAN_CHUNK) as usize);
        if f.take(CODEX_SCAN_CHUNK).read_to_end(&mut buf).is_err() {
            return from_offset;
        }
        let cursor_key = source.map(|source| source.key.as_str()).unwrap_or(file);
        let mut cursor = self.state.lock().unwrap().codex_cursors.get(cursor_key).cloned().unwrap_or_default();
        let Some(last_nl) = buf.iter().rposition(|byte| *byte == b'\n') else {
            if buf.len() as u64 == CODEX_SCAN_CHUNK {
                cursor.skip_partial_line = true;
                self.state.lock().unwrap().codex_cursors.insert(cursor_key.to_string(), cursor);
                return from_offset + buf.len() as u64;
            }
            return from_offset;
        };
        let consumed = (last_nl + 1) as u64;
        let start = if cursor.skip_partial_line {
            cursor.skip_partial_line = false;
            cursor.scope.skip_record();
            buf.iter().position(|byte| *byte == b'\n').unwrap_or(0) + 1
        } else { 0 };
        let text = String::from_utf8_lossy(&buf[start..last_nl + 1]);

        // cwd/sid из первой строки (session_meta) — переживают инкрементальный скан
        let (cwd, sid) = codex_meta_head(file);
        for line in text.lines() {
            if !["\"session_meta\"", "\"turn_context\"", "\"token_count\""].iter().any(|kind| line.contains(kind)) {
                cursor.scope.skip_record();
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { cursor.scope.skip_record();continue };
            if !cursor.scope.accept(&v) {continue;}
            if v.get("type").and_then(Value::as_str) == Some("session_meta") {
                continue;
            }
            if v.get("type").and_then(Value::as_str) == Some("turn_context") {
                if let Some(m) = v.pointer("/payload/model").and_then(Value::as_str) {
                    cursor.model = m.to_string();
                }
                continue;
            }
            if v.get("type").and_then(Value::as_str) != Some("event_msg")
                || v.pointer("/payload/type").and_then(Value::as_str) != Some("token_count") {
                continue;
            }
            let Some(info) = v.pointer("/payload/info").filter(|info| info.is_object()) else { continue };
            let timestamp = v.get("timestamp").and_then(Value::as_str).unwrap_or("");
            let Some(tok) = cursor.take_usage(info, timestamp) else { continue };
            let ts = crate::transcript::parse_ts(timestamp).unwrap_or_else(now_ms);
            let friendly = if cursor.model.is_empty() { "Codex".into() } else {
                crate::backend::backend(crate::backend::Agent::Codex).friendly_model(&cursor.model)
            };
            let project = cwd.as_deref().map(basename).unwrap_or_else(|| "другое".into());
            let session_key = source.map(|source| source.key.as_str()).unwrap_or(&sid);
            let mut state = self.state.lock().unwrap();
            Self::add_record(&mut state, ts, &friendly, &project, "codex", session_key, tok);
            if let Some(source) = source {
                if let Some(session) = state.sessions.get_mut(session_key) {
                    session.instance_id = Some(source.instance_id.clone()); session.instance_label = Some(source.label.clone());
                    session.provider_home = Some(source.home.clone()); session.provider_session_id = Some(source.sid.clone()); session.machine = Some("local".into());
                }
            }
        }
        self.state.lock().unwrap().codex_cursors.insert(cursor_key.to_string(), cursor);
        from_offset + consumed
    }

    /// Все wire.jsonl Kimi: `<дом>/sessions/<wd_*>/<sid>/agents/<агент>/wire.jsonl`.
    /// Сабагенты (`agent-N`) жгут те же токены, что и `main`, — берём всех, иначе
    /// расход сессии с делегированием занижен в разы.
    fn list_kimi_wires() -> Vec<String> {
        let mut out = Vec::new();
        let Ok(wds) = fs::read_dir(kimi_sessions_dir()) else { return out };
        for wd in wds.filter_map(|e| e.ok()) {
            let Ok(sessions) = fs::read_dir(wd.path()) else { continue };
            for s in sessions.filter_map(|e| e.ok()) {
                let Ok(agents) = fs::read_dir(s.path().join("agents")) else { continue };
                for a in agents.filter_map(|e| e.ok()) {
                    let p = a.path().join("wire.jsonl");
                    if p.is_file() {
                        out.push(p.to_string_lossy().into_owned());
                    }
                }
            }
        }
        out
    }

    /// Разбор wire.jsonl Kimi: считаем ТОЛЬКО `usage.record` (оба scope: `turn` и
    /// `session` — второй пишется при компакции). Рядом в том же файле лежит
    /// `context.append_loop_event`/`step.end` с побайтово таким же `usage`
    /// (сверено на 203 файлах, расхождений 0) — сложить оба значит ровно удвоить
    /// расход, поэтому step.end не трогаем. billing="kimi", model — полный алиас
    /// (`kimi-code/k3`) через friendly_model. `usage` всегда четыре поля, без
    /// total/reasoning: input = inputOther (кэш отдельно, как в общем `Tok`).
    fn parse_kimi_file_part(&self, file: &str, from_offset: u64) -> u64 {
        let Ok(meta) = fs::metadata(file) else { return from_offset };
        let size = meta.len();
        if size <= from_offset {
            return from_offset;
        }
        let Ok(mut f) = fs::File::open(file) else { return from_offset };
        if f.seek(SeekFrom::Start(from_offset)).is_err() {
            return from_offset;
        }
        let mut buf = Vec::with_capacity((size - from_offset) as usize);
        if f.read_to_end(&mut buf).is_err() {
            return from_offset;
        }
        let text = String::from_utf8_lossy(&buf);
        let Some(last_nl) = text.rfind('\n') else { return from_offset };
        let consumed = text[..=last_nl].len() as u64;
        let text = &text[..last_nl];

        // sid — имя каталога сессии, cwd — из state.json: оба переживают
        // инкрементальный скан, в самих записях расхода их нет
        let (cwd, sid) = kimi_meta_for(Path::new(file));
        let project = cwd.as_deref().map(basename).unwrap_or_else(|| "другое".into());
        for line in text.split('\n') {
            if !line.contains("usage.record") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if v.get("type").and_then(Value::as_str) != Some("usage.record") {
                continue;
            }
            let Some(u0) = v.get("usage") else { continue };
            let num = |k: &str| u0.get(k).and_then(Value::as_f64).unwrap_or(0.0);
            let ts = v.get("time").and_then(Value::as_i64).unwrap_or_else(now_ms);
            let tok = Tok {
                input: num("inputOther"),
                out: num("output"),
                cw: num("inputCacheCreation"),
                cr: num("inputCacheRead"),
                reasoning: 0.0,
            };
            let model = v.get("model").and_then(Value::as_str).unwrap_or("");
            let friendly = crate::backend::backend(crate::backend::Agent::Kimi).friendly_model(model);
            Self::add_record(&mut self.state.lock().unwrap(), ts, &friendly, &project, "kimi", &sid, tok);
        }
        from_offset + consumed
    }

    pub fn backfilled(&self) -> bool {
        self.state.lock().unwrap().backfilled
    }

    /* ---------- агрегаты для UI ---------- */

    fn range_hours(&self, since_ms: i64) -> Vec<HourRow> {
        let state = self.state.lock().unwrap();
        let mut out = Vec::new();
        for (key, a) in &state.hours {
            let mut parts = key.split('|');
            let (Some(hour), Some(model), Some(project), Some(billing)) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let Some(ts) = chrono::DateTime::parse_from_rfc3339(&format!("{hour}:00:00Z"))
                .ok()
                .map(|d| d.timestamp_millis())
            else {
                continue;
            };
            if ts < since_ms {
                continue;
            }
            out.push(HourRow {
                ts,
                hour: hour.to_string(),
                model: model.to_string(),
                project: project.to_string(),
                billing: billing.to_string(),
                tok: a.tok,
                cost: a.cost,
                n: a.n,
            });
        }
        // HashMap итерируется в произвольном порядке — сортируем хронологически,
        // чтобы порядок строк в разрезах был стабильным (JS-объект хранил
        // insertion order)
        out.sort_by_key(|r| r.ts);
        out
    }

    /// Полная сводка периода для вкладки «Статистика» (форма — как у Electron).
    pub fn stats(&self, period: &str) -> Value {
        let now = now_ms();
        // сутки обнуляются в 03:00 МСК — это ровно 00:00 UTC (МСК = UTC+3, без
        // DST), поэтому граница дня совпадает с UTC-сутками часовых агрегатов
        let day_start = now / DAY_MS * DAY_MS;
        let week = period == "week";
        let since = if week { day_start - 6 * DAY_MS } else { day_start };
        let rows = self.range_hours(since);

        let mut total_tok = 0.0;
        let mut total_api = 0.0;
        let mut total_plan = 0.0;
        let mut total_n = 0.0;
        for r in &rows {
            total_tok += r.tok.total();
            total_n += r.n;
            if is_api(&r.billing) {
                total_api += r.cost;
            } else {
                total_plan += r.cost;
            }
        }

        // серия: сегодня — по часам от границы суток; неделя — по дням
        let mut series = Vec::new();
        if week {
            for i in (0..=6).rev() {
                let start = day_start - i * DAY_MS;
                let key = chrono::DateTime::from_timestamp_millis(start)
                    .unwrap_or_default()
                    .format("%Y-%m-%d")
                    .to_string();
                let tok: f64 = rows.iter().filter(|r| r.hour.starts_with(&key)).map(|r| r.tok.total()).sum();
                // подпись — по МСК-границе
                let d = chrono::DateTime::from_timestamp_millis(start + 3 * 3_600_000).unwrap_or_default();
                series.push(serde_json::json!({ "label": d.format("%d.%m").to_string(), "tok": tok }));
            }
        } else {
            let hours_passed = (((now - day_start) as f64) / 3_600_000.0).ceil().min(24.0) as i64;
            for i in 0..hours_passed {
                let start = day_start + i * 3_600_000;
                let key = chrono::DateTime::from_timestamp_millis(start)
                    .unwrap_or_default()
                    .format("%Y-%m-%dT%H")
                    .to_string();
                let tok: f64 = rows.iter().filter(|r| r.hour == key).map(|r| r.tok.total()).sum();
                let local: chrono::DateTime<chrono::Local> =
                    chrono::DateTime::from_timestamp_millis(start).unwrap_or_default().into();
                series.push(serde_json::json!({
                    "label": format!("{}:00", chrono::Timelike::hour(&local)),
                    "tok": tok,
                }));
            }
        }

        let by_model = sum_by(&rows, |r| r.model.clone());
        let mut by_model: Vec<Value> = by_model
            .into_iter()
            .filter(|(_, a)| a.tok > 0.0)
            .map(|(k, a)| serde_json::json!({"key": k, "tok": a.tok, "cost": a.cost, "api": a.api, "plan": a.plan, "n": a.n}))
            .collect();
        by_model.sort_by(|a, b| cmp_f64_desc(a["tok"].as_f64(), b["tok"].as_f64()));

        let by_project_map = sum_by(&rows, |r| format!("{}|{}", r.project, r.billing));
        let mut by_project: Vec<Value> = by_project_map
            .into_iter()
            .map(|(k, a)| {
                let (project, billing) = k.split_once('|').unwrap_or((k.as_str(), "plan"));
                serde_json::json!({"key": project, "billing": billing, "tok": a.tok, "cost": a.cost, "api": a.api, "plan": a.plan, "n": a.n})
            })
            .collect();
        by_project.sort_by(|a, b| cmp_f64_desc(a["tok"].as_f64(), b["tok"].as_f64()));
        by_project.truncate(12);

        // разрез по биллингу: подписка и каждый API-endpoint отдельно
        let mut billing_projects: HashMap<String, Vec<String>> = HashMap::new();
        for r in &rows {
            let set = billing_projects.entry(r.billing.clone()).or_default();
            if !set.contains(&r.project) {
                set.push(r.project.clone());
            }
        }
        let mut by_billing: Vec<Value> = sum_by(&rows, |r| r.billing.clone())
            .into_iter()
            .map(|(k, a)| {
                let host = is_api(&k).then(|| k[4..].to_string());
                let projects: Vec<String> = billing_projects.get(&k).cloned().unwrap_or_default()
                    .into_iter().take(10).collect();
                serde_json::json!({"key": k, "host": host, "projects": projects, "tok": a.tok, "cost": a.cost, "api": a.api, "plan": a.plan, "n": a.n})
            })
            .collect();
        by_billing.sort_by(|a, b| cmp_f64_desc(a["tok"].as_f64(), b["tok"].as_f64()));

        let mut sessions: Vec<Value> = {
            let state = self.state.lock().unwrap();
            state
                .sessions
                .iter()
                .filter(|(_, s)| s.last >= since)
                .map(|(id, s)| {
                    let mut row = s.json();
                    row["id"] = serde_json::json!(id);
                    row["project"] = serde_json::json!(s.project);
                    row["scope"] = serde_json::json!("session-lifetime");
                    row
                })
                .collect()
        };
        sessions.sort_by(|a, b| cmp_f64_desc(a["tok"].as_f64(), b["tok"].as_f64()));
        sessions.truncate(12);

        let (win_start, win_tokens, win_cost) = {
            let st = self.state.lock().unwrap();
            (st.window.start, st.window.tokens, st.window.cost)
        };
        let win_active = win_start > 0 && now < win_start + WINDOW_MS;

        // токены текущего ОФИЦИАЛЬНОГО окна (его старт = сброс − 5ч);
        // A weekly-only response is still official data, even without a session window.
        let official_out = self.official_info().map(|o| {
            let win_start = o.session.as_ref().map(|s| s.reset_at - WINDOW_MS).unwrap_or(0);
            let win_tok:Option<f64> = if win_start > 0 && official_is_default_local(&o) {
                Some(self.range_hours(win_start).iter().filter(|r|r.billing=="plan").map(|r|r.tok.total()).sum())
            } else { None };
            let mut v = serde_json::to_value(&o).unwrap_or(Value::Null);
            if let Some(obj) = v.as_object_mut() {
                obj.insert("windowTokens".into(), serde_json::json!(win_tok));
            }
            v
        });

        let official_err = self.official_err.lock().unwrap_or_else(|e| e.into_inner()).clone();
        serde_json::json!({
            "period": if week { "week" } else { "today" },
            "officialError": official_err,
            "costEstimated": true, "costBasis": "model-family-estimate",
            "source": "local-transcripts",
            "total": { "tok": total_tok, "api": total_api, "plan": total_plan, "n": total_n },
            "series": series,
            "byModel": by_model,
            "byProject": by_project,
            "byBilling": by_billing,
            "sessions": sessions,
            "official": official_out,
            "window": if win_active {
                serde_json::json!({ "tokens": win_tokens, "cost": win_cost, "resetInMs": win_start + WINDOW_MS - now })
            } else {
                serde_json::json!({ "tokens": 0, "cost": 0, "resetInMs": 0 })
            },
        })
    }

    /// Расход одной сессии — для строки чата и истории.
    pub fn for_session(&self, id: &str) -> Option<Value> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let s = state.sessions.get(id)?;
        Some(s.json())
    }

    pub async fn for_remote_session(d:&Daemon,session:&crate::model::Session)->Value {
        let trace=crate::analytics::remote_session_trace(d,session).await;
        remote_session_usage(trace)
    }

    /* ---------- официальные лимиты подписки ---------- */
    /* Принимаем только распознанные проценты и времена сброса из /usage.
     * Некоторые CLI возвращают вместо них cost summary: это не лимиты.
     * Тариф и аккаунт — из ~/.claude.json (oauthAccount). */

    pub fn official_info(&self) -> Option<OfficialInfo> {
        let o = self.official.lock().unwrap().clone()?;
        let source = o.source.clone();
        // A remote CLI may belong to a completely different account.
        let default_home=crate::agent_instances::canonical_home(&claude_dir()).ok();
        let default_profile=source=="local" && o.provider_home.as_deref().is_some_and(|home|Some(Path::new(home))==default_home.as_deref());
        let account = if default_profile { read_account() } else {
            Account { plan: None, email: String::new(), name: String::new() }
        };
        Some(OfficialInfo {
            session: o.session,
            week: o.week,
            week_model: o.week_model,
            source,
            instance_id:o.instance_id,
            provider_home:o.provider_home,
            provider:"claude".into(),
            at: o.at,
            account,
        })
    }

    pub fn official_info_for_session(&self,session:&crate::model::Session)->Option<OfficialInfo> {
        let info=self.official_info()?;
        if session.agent.as_deref().unwrap_or("claude")!="claude"
            || info.source!=session.remote.as_deref().unwrap_or("local") {return None;}
        let identity=if let Some(id)=session.instance_id.as_deref() {info.instance_id.as_deref()==Some(id)}
            else if let Some(home)=session.provider_home.as_deref() {info.provider_home.as_deref()==Some(home)}
            else {session.remote.is_none() && official_is_default_local(&info)};
        identity.then_some(info)
    }

    /// Свежий /usage как можно скорее (после подтверждённого лимита).
    pub fn refresh_official_soon(self: &Arc<Self>, d: &Arc<Daemon>) {
        let d = d.clone();
        tauri::async_runtime::spawn(async move {
            crate::budget::ensure_fresh(&d, 0, "подтверждённый лимит").await;
        });
    }

    pub fn set_official_err(&self, why: &str) {
        *self.official.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *self.official_err.lock().unwrap_or_else(|p| p.into_inner()) = Some(why.to_string());
    }

    pub fn set_official(&self, d: &Arc<Daemon>, session: Option<PctReset>, week: Option<PctReset>, week_model: Option<ModelWeek>, source: &str) {
        let home = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(claude_dir);
        let home = crate::agent_instances::canonical_home(&home).ok();
        let instance_id = if source == "local" { home.as_ref().and_then(|h| crate::agent_instances::provider_instance_id("claude", "local", h).ok()) } else { None };
        let provider_home = if source == "local" { home.map(|h| h.to_string_lossy().into_owned()) } else { None };
        let official = Official { session, week, week_model, source: source.into(), at: now_ms(), instance_id, provider_home };
        self.publish_official(d, official, source.into());
    }

    /// Достать текст `/usage`: сначала локально, затем с узлов по порядку.
    ///
    /// Человек, работающий на узле, может быть не авторизован локально вовсе —
    /// тогда правда о лимитах живёт только там. Ошибки всех источников
    /// собираются в одну строку: чинить будут по ней.
    async fn obtain_official(self: &Arc<Self>, d: &Arc<Daemon>) -> Result<(Official, String), String> {
        let mut errs: Vec<String> = Vec::new();
        if crate::claude_bin::resolve_claude_bin().is_none() {
            errs.push("локально: claude не найден".into());
        } else {
            let result = crate::claude_bin::run_claude(
                &["-p", "--no-session-persistence", "/usage"],
                Duration::from_secs(90),
            )
            .await
            .ok_or_else(|| "/usage не ответил — проверь авторизацию и сеть".into());
            if let Some((mut official,source)) = official_candidate("local", result, &mut errs) {
                let home=std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(claude_dir);
                let home=crate::agent_instances::canonical_home(&home)?;
                official.instance_id=Some(crate::agent_instances::provider_instance_id("claude","local",&home)?);
                official.provider_home=Some(home.to_string_lossy().into_owned());
                return Ok((official,source));
            }
        }
        for node in d.remotes.all() {
            let name = node.cfg.name.clone();
            if !node.status().connected {continue;}
            let client=match node.client(){Ok(c)=>c,Err(e)=>{errs.push(format!("{name}: {e}"));continue;}};
            let sources=match client.sources().await {Ok(s)=>s,Err(e)=>{errs.push(format!("{name}: {e}"));continue;}};
            let profiles:Vec<_>=sources.as_array().into_iter().flatten().filter(|s|s["agent"]=="claude" && s["available"]==true).collect();
            if profiles.len()!=1 {
                errs.push(format!("{name}: требуется однозначный профиль Claude для квоты (найдено {})",profiles.len()));
                continue;
            }
            let profile=profiles[0];
            let Some(id)=profile["instanceId"].as_str() else {continue;};
            let result=client.usage_text_for(false,Some(id)).await;
            if let Some((mut official,source)) = official_candidate(&name, result, &mut errs) {
                official.instance_id=Some(id.into());
                official.provider_home=profile["providerHome"].as_str().map(String::from);
                return Ok((official,source));
            }
        }
        Err(if errs.is_empty() { "источников лимитов нет".into() } else { errs.join(" · ") })
    }

    pub async fn fetch_official(self: &Arc<Self>, d: &Arc<Daemon>) {
        let Some(_reservation) = OfficialFetchGuard::acquire(&self.official_busy) else { return };
        let got = self.obtain_official(d).await;
        let (official, source) = match got {
            Ok(x) => x,
            Err(why) => {
                // The UI and automatic limit recovery must not act on an old
                // successful poll after every current source has failed.
                *self.official.lock().unwrap_or_else(|p| p.into_inner()) = None;
                // Провал добытчика обязан быть виден: раньше он молчал, и
                // человек смотрел на пустую полоску, гадая, где сломано.
                let changed = {
                    let mut e = self.official_err.lock().unwrap_or_else(|p| p.into_inner());
                    let same = e.as_deref() == Some(why.as_str());
                    *e = Some(why.clone());
                    !same
                };
                if changed {
                    crate::log::line(&format!("[usage] лимиты недоступны: {why}"));
                }
                return;
            }
        };
        self.publish_official(d, official, source);
    }

    fn publish_official(&self, d: &Arc<Daemon>, official: Official, source: String) {
        *self.official_err.lock().unwrap_or_else(|p| p.into_inner()) = None;
        let source_changed={
            let mut src = self.official_source.lock().unwrap_or_else(|p| p.into_inner());
            let changed=*src!=source;
            if *src != source {
                crate::log::line(&format!("[usage] лимиты приехали: источник {source}"));
                *src = source.clone();
            }
            changed
        };
        let prev_pct = self
            .official
            .lock()
            .unwrap()
            .as_ref()
            .filter(|o|!source_changed && o.instance_id==official.instance_id)
            .and_then(|o| o.session.as_ref().map(|s| s.pct))
            .unwrap_or(0);
        let warn = official.session.as_ref().filter(|s| prev_pct < 90 && s.pct >= 90).cloned();
        *self.official.lock().unwrap() = Some(official);
        // предупреждение ДО стены: пересекли 90% окна
        if let Some(w) = warn {
            let plan = self.official_info().filter(official_is_default_local).and_then(|o|o.account.plan).unwrap_or_default();
            d.notify(
                &format!(
                    "Claude{} — окно почти исчерпано",
                    if plan.is_empty() { String::new() } else { format!(" {plan}") }
                ),
                &format!("{} · {}% использовано · сброс через {}", source,w.pct, fmt_reset_in(w.reset_at)),
                None,
                "limit",
            );
        }
    }
}

#[derive(Clone)]
struct HourRow {
    #[allow(dead_code)]
    ts: i64,
    hour: String,
    model: String,
    project: String,
    billing: String,
    tok: Tok,
    cost: f64,
    n: f64,
}

#[derive(Default)]
struct SumAgg {
    tok: f64,
    cost: f64,
    api: f64,
    plan: f64,
    n: f64,
}

fn sum_by(rows: &[HourRow], key_fn: impl Fn(&HourRow) -> String) -> Vec<(String, SumAgg)> {
    let mut map: HashMap<String, SumAgg> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for r in rows {
        let k = key_fn(r);
        if !map.contains_key(&k) {
            order.push(k.clone());
        }
        let a = map.entry(k).or_default();
        a.tok += r.tok.total();
        a.cost += r.cost;
        a.n += r.n;
        if is_api(&r.billing) {
            a.api += r.cost;
        } else {
            a.plan += r.cost;
        }
    }
    order.into_iter().filter_map(|k| map.remove_entry(&k)).collect()
}

fn is_api(billing: &str) -> bool {
    billing != "plan"
}

fn cmp_f64_desc(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
    b.unwrap_or(0.0).partial_cmp(&a.unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal)
}

fn friendly_model_or_other(id: &str) -> String {
    let m = friendly_model(id);
    let known = ["Opus", "Sonnet", "Haiku", "Fable", "Mythos"];
    if known.contains(&m.as_str()) {
        m
    } else {
        "другая".into()
    }
}

pub(crate) fn remote_session_usage(mut result:Value)->Value {
    let trace=result.as_object_mut().and_then(|o|o.remove("trace")).unwrap_or(Value::Null);
    let models=trace["models"].as_array();
    let mut tok=Tok::default(); let mut cost=0.0; let mut known=false; let mut requests=0.0;
    for model in models.into_iter().flatten() {
        let read=|key:&str|model[key].as_f64().filter(|n|n.is_finite() && *n>=0.0).unwrap_or(0.0);
        let observed=model["inputTokens"].is_number() || model["outputTokens"].is_number();
        if !observed {continue;}
        known=true;
        let tokens=Tok {input:read("inputTokens"),out:read("outputTokens"),cr:read("cacheReadTokens"),
            cw:read("cacheWriteTokens"),reasoning:read("reasoningTokens").min(read("outputTokens"))};
        let model_id=model["model"].as_str().unwrap_or("");
        let friendly=if trace["sourceFormat"]=="codex" {crate::backend::backend(crate::backend::Agent::Codex).friendly_model(model_id)}else{friendly_model_or_other(model_id)};
        cost+=tokens.cost(&friendly);requests+=read("requests");
        tok.input+=tokens.input;tok.out+=tokens.out;tok.cr+=tokens.cr;tok.cw+=tokens.cw;tok.reasoning+=tokens.reasoning;
    }
    for (key,value) in [("tok",tok.total()),("cost",cost),("inputTokens",tok.input),("outputTokens",tok.out),
        ("cacheReadTokens",tok.cr),("cacheWriteTokens",tok.cw),("reasoningTokens",tok.reasoning),("requests",requests)] {
        result[key]=if known {serde_json::json!(value)}else{Value::Null};
    }
    let input=tok.input+tok.cr+tok.cw;
    result["cacheHitPct"]=if known && input>0.0 {serde_json::json!(100.0*tok.cr/input)}else{Value::Null};
    result["costEstimated"]=serde_json::json!(true);
    result["costBasis"]=serde_json::json!("model-family-estimate");
    result["coverage"]=trace["coverage"].clone();
    result["firstAt"]=trace["firstAt"].clone();result["lastAt"]=trace["lastAt"].clone();
    result["partial"]=serde_json::json!(trace["coverage"]["truncated"]==true || !known);
    result["available"]=serde_json::json!(known);
    if !trace.is_null() {
        for key in ["machine","instanceId","providerHome","providerSessionId"] {result[key]=trace[key].clone();}
        result["instanceLabel"]=trace["sourceLabel"].clone();
    }
    result
}

fn official_is_default_local(info:&OfficialInfo)->bool {
    info.source=="local" && info.provider=="claude" && info.provider_home.as_deref().is_some_and(|home|
        crate::agent_instances::canonical_home(&claude_dir()).ok().as_deref()==Some(Path::new(home)))
}

/// JS-truthiness для значений конфига: '', null, false, 0 → false.
fn js_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(_) => true,
    }
}

/// hostname как у new URL(): без userinfo и порта, в нижнем регистре.
fn url_host(u: &str) -> Option<String> {
    let rest = u.split("://").nth(1)?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?; // отрезать user:pass@
    let host = host.split(':').next()?; // отрезать порт
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// Тариф и аккаунт из ~/.claude.json (oauthAccount).
fn read_account() -> Account {
    let parse = || -> Option<Account> {
        let raw = fs::read_to_string(home_dir().join(".claude.json")).ok()?;
        let d: Value = serde_json::from_str(&raw).ok()?;
        let oa = d.get("oauthAccount").cloned().unwrap_or(Value::Null);
        let tier = oa
            .get("organizationRateLimitTier")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let org = oa.get("organizationType").and_then(Value::as_str).unwrap_or("");
        let plan = if let Some(c) = regex::Regex::new(r"max_(\d+)x").unwrap().captures(&tier) {
            Some(format!("Max ({}x)", &c[1]))
        } else if org == "claude_max" {
            Some("Max".into())
        } else if org == "claude_pro" || tier.contains("pro") {
            Some("Pro".into())
        } else {
            None
        };
        Some(Account {
            plan,
            email: oa.get("emailAddress").and_then(Value::as_str).unwrap_or("").into(),
            name: oa.get("displayName").and_then(Value::as_str).unwrap_or("").into(),
        })
    };
    parse().unwrap_or(Account { plan: None, email: String::new(), name: String::new() })
}

fn parse_official(text: &str) -> Option<(Option<PctReset>, Option<PctReset>, Option<ModelWeek>)> {
    let grab = |p: &str| -> Option<PctReset> {
        let re = regex::RegexBuilder::new(p).case_insensitive(true).build().unwrap();
        let c = re.captures(text)?;
        Some(PctReset {
            pct: c[1].parse().unwrap_or(0),
            reset_at: parse_reset_date(c.get(2).map(|m| m.as_str()).unwrap_or("")),
        })
    };
    // Хвост строки берём целиком, со скобками: в них теперь живёт «(UTC)», и
    // без него время сброса трактовалось бы в неведомо чьём поясе.
    let session = grab(r"Current session:\s*(\d+)%\s*used\s*·\s*resets\s+([^\n]+)");
    let week = grab(r"Current week \(all models\):\s*(\d+)%\s*used\s*·\s*resets\s+([^\n]+)");
    // Модель в скобках — любая: Fable, Opus, Sonnet only… Жёсткое имя молча
    // протухает при каждой смене модельного ряда.
    let model_re = regex::RegexBuilder::new(
        r"Current week \(([^)]+)\):\s*(\d+)%\s*used(?:\s*·\s*resets\s+([^\n]+))?",
    )
    .case_insensitive(true)
    .build()
    .unwrap();
    let week_model = model_re
        .captures_iter(text)
        .find(|c| !c[1].eq_ignore_ascii_case("all models"))
        .map(|c| ModelWeek {
            model: c[1].trim().to_string(),
            pct: c[2].parse().unwrap_or(0),
            reset_at: parse_reset_date(c.get(3).map(|m| m.as_str()).unwrap_or("")),
        });
    if session.is_none() && week.is_none() {
        return None;
    }
    Some((session, week, week_model))
}

/// «Aug 10, 6:59pm (UTC)» → миллисекунды эпохи.
///
/// Терпимо к дрейфу: запятая или «at» после числа, минуты необязательны,
/// месяц полным словом или тремя буквами. Пояс — по хвосту строки: «(UTC)»
/// значит UTC, иначе местное время машины. Прежний разбор требовал «at»
/// (формат уже ушёл на запятую — и «до …» исчезло из панели), а пояс был
/// зашит числом +3 — то есть время врало всем, кто не в Москве.
fn parse_reset_date(s: &str) -> i64 {
    let re = regex::RegexBuilder::new(
        r"([A-Z][a-z]{2})[a-z]*\s+(\d{1,2})(?:,|\s+at)?\s+(\d{1,2})(?::(\d{2}))?\s*(am|pm)?",
    )
    .case_insensitive(true)
    .build()
    .unwrap();
    let Some(c) = re.captures(s) else { return 0 };
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let Some(month) = MONTHS.iter().position(|m| m.eq_ignore_ascii_case(&c[1])) else { return 0 };
    let day: u32 = c[2].parse().unwrap_or(1);
    let mut hh: u32 = c[3].parse::<u32>().unwrap_or(0);
    let ampm = c.get(5).map(|m| m.as_str().to_ascii_lowercase());
    match ampm.as_deref() {
        Some("pm") if hh < 12 => hh += 12,
        Some("am") if hh == 12 => hh = 0,
        _ => {} // без am/pm — 24-часовой формат как есть
    }
    let min: u32 = c.get(4).map(|m| m.as_str().parse().unwrap_or(0)).unwrap_or(0);
    // Пояс — из хвоста строки. Живьём видели два: «(UTC)» у нынешнего формата
    // и «(Europe/Moscow)» у прежнего (Claude пишет время в поясе аккаунта;
    // Москва без переводов с 2014-го, смещение константно). Незнакомое имя —
    // это почти наверняка пояс самой машины: разбираем как местное время, что
    // строго честнее прежнего зашитого +3 для всех подряд.
    let tail_up = s.to_ascii_uppercase();
    let fixed_offset_ms: Option<i64> = if tail_up.contains("UTC") {
        Some(0)
    } else if tail_up.contains("EUROPE/MOSCOW") {
        Some(3 * 3_600_000)
    } else {
        None
    };
    let now = now_ms();
    let year = chrono::DateTime::from_timestamp_millis(now)
        .map(|d| chrono::Datelike::year(&d))
        .unwrap_or(2026);
    let make = |y: i32| -> i64 {
        let Some(naive) = chrono::NaiveDate::from_ymd_opt(y, month as u32 + 1, day)
            .and_then(|d| d.and_hms_opt(hh, min, 0))
        else {
            return 0;
        };
        match fixed_offset_ms {
            Some(off) => naive.and_utc().timestamp_millis() - off,
            None => {
                use chrono::TimeZone;
                chrono::Local
                    .from_local_datetime(&naive)
                    .single()
                    .map(|dt| dt.timestamp_millis())
                    .unwrap_or(0)
            }
        }
    };
    let mut ts = make(year);
    // Сброс всегда в будущем; «Jan 1» в конце декабря — это уже следующий год.
    if ts != 0 && ts < now - 12 * 3_600_000 {
        ts = make(year + 1);
    }
    ts
}

#[cfg(test)]
mod tests {
    use super::*;
    const REAL_USAGE: &str = "You are currently using your subscription to power your Claude Code usage\n\n\
Current session: 62% used · resets Aug 10, 6:59pm (UTC)\n\
Current week (all models): 94% used · resets Aug 10, 10:59pm (UTC)\n\
Current week (Fable): 54% used · resets Aug 10, 11pm (UTC)\n";

    #[test]
    fn billing_host_extraction() {
        assert_eq!(url_host("https://proxy.corp.dev/v1"), Some("proxy.corp.dev".into()));
        assert_eq!(url_host("http://localhost:8080"), Some("localhost".into()));
        assert_eq!(url_host("мусор"), None);
    }

    /// Обход транскриптов Claude Code — В ГЛУБИНУ: субагенты живут в
    /// `<проект>/<uuid сессии>/subagents/agent-*.jsonl`, и плоский обход на два
    /// уровня видел четверть запросов. Дедуп по `message.id` при этом остаётся:
    /// один и тот же ход, попавший в два файла, считается один раз.
    #[test]
    fn deep_walk_finds_subagents_and_does_not_double_count() {
        let root = std::env::temp_dir().join("jarvis-usage-deep");
        let _ = fs::remove_dir_all(&root);
        let proj = root.join("-Users-me-proj");
        let subs = proj.join("2bd7188f-7b81-455c-a711-1dc664dc2462/subagents");
        fs::create_dir_all(&subs).unwrap();
        let turn = |id: &str, tok: i64| {
            format!(
                "{{\"type\":\"assistant\",\"cwd\":\"/Users/me/proj\",\"sessionId\":\"S1\",\
                  \"timestamp\":\"2026-08-20T10:00:00.000Z\",\"message\":{{\"id\":\"{id}\",\
                  \"model\":\"claude-sonnet-4-6\",\"usage\":{{\"input_tokens\":{tok},\"output_tokens\":0,\
                  \"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0}}}}}}\n"
            )
        };
        fs::write(proj.join("main.jsonl"), turn("msg_main", 100)).unwrap();
        // сабагент на глубине 3 + повтор родительского хода (страховка дедупа)
        fs::write(
            subs.join("agent-ace037da304d4799.jsonl"),
            turn("msg_sub", 20) + &turn("msg_main", 100),
        )
        .unwrap();

        let mut files = Vec::new();
        walk_jsonl(&root, &mut files);
        files.sort();
        assert_eq!(files.len(), 2, "глубокий обход видит и сабагентов: {files:?}");
        assert!(files.iter().any(|f| f.contains("subagents")));

        let u = fresh_usage();
        for f in &files {
            u.parse_file_part(f, 0);
        }
        let st = u.state.lock().unwrap();
        assert_eq!(
            st.sessions["S1"].tok.total(),
            120.0,
            "родитель + сабагент, повтор по message.id не удвоился"
        );
        drop(st);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fork_usage_skips_parent_prefix_preserves_child_identity_and_restores_ordinal() {
        let child=serde_json::json!({"type":"session_meta","payload":{"id":"child","cwd":"/qa/child","forked_from_id":"parent","subagent_history_start_ordinal":4}});
        let parent=serde_json::json!({"type":"session_meta","payload":{"id":"parent","cwd":"/qa/parent"}});
        let log=UsageLog::new(&format!("{child}\n{parent}\n{}",codex_event("2026-09-05T01:00:00Z",raw_usage(1000,0,100,0),raw_usage(1000,0,100,0))));
        let usage=usage_with_state(State::default());
        let offset=usage.parse_codex_file_part(log.path(),0);
        assert!(usage.for_session("parent").is_none());assert!(usage.for_session("child").is_none());
        let restored:State=serde_json::from_str(&serde_json::to_string(&*usage.state.lock().unwrap()).unwrap()).unwrap();
        let usage=usage_with_state(restored);
        log.append(&format!("{}\n{}\n{}{}",
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[]}}),
            serde_json::json!({"type":"turn_context","payload":{"model":"gpt-5.4"}}),
            codex_event("2026-09-05T01:00:01Z",raw_usage(1200,0,120,0),raw_usage(200,0,20,0)),
            codex_event("2026-09-05T01:00:02Z",raw_usage(1300,0,130,0),raw_usage(100,0,10,0))));
        usage.parse_codex_file_part(log.path(),offset);
        assert_eq!(usage.for_session("child").unwrap()["tok"],330.0);
        assert!(usage.for_session("parent").is_none());
        assert_eq!(usage.state.lock().unwrap().sessions["child"].project,"child");
    }

    #[test]
    fn fork_first_total_without_last_is_only_a_baseline() {
        let mut cursor=CodexCursor::default();cursor.scope.forked=true;
        assert!(cursor.take_usage(&serde_json::json!({"total_token_usage":raw_usage(1200,0,120,0)}),"first").is_none());
        let next=cursor.take_usage(&serde_json::json!({"total_token_usage":raw_usage(1300,0,130,0)}),"next").unwrap();
        assert_eq!(next.total(),110.0);
    }
    #[test]
    fn remote_session_usage_preserves_unknown_offline_and_excludes_reasoning_from_total() {
        let missing=remote_session_usage(serde_json::json!({"trace":null,"source":"remote-transcripts","machine":"vm-a","stale":true,"error":"offline"}));
        assert!(missing["tok"].is_null());assert!(missing["cost"].is_null());assert_eq!(missing["available"],false);
        let observed=remote_session_usage(serde_json::json!({"source":"remote-transcripts","stale":true,"cachedAt":100,"trace":{
            "sourceFormat":"codex","machine":"vm-a","instanceId":"personal","sourceLabel":"Personal","coverage":{"truncated":true},
            "models":[{"model":"gpt-5.4","inputTokens":80,"cacheReadTokens":20,"outputTokens":30,"reasoningTokens":12,"requests":1}]}}));
        assert_eq!(observed["tok"],130.0);assert_eq!(observed["reasoningTokens"],12.0);
        assert_eq!(observed["instanceId"],"personal");assert_eq!(observed["partial"],true);
        assert_eq!(observed["stale"],true);assert_eq!(observed["cachedAt"],100);
        assert!(!observed.to_string().contains("\"trace\""));
    }

    #[test]
    fn official_quota_matches_provider_machine_and_profile_only() {
        let usage=usage_with_state(State::default());
        *usage.official.lock().unwrap()=Some(Official {source:"vm-a".into(),session:Some(PctReset{pct:95,reset_at:now_ms()+100000}),week:None,week_model:None,at:now_ms(),
            instance_id:Some("claude-v1-work".into()),provider_home:Some("/guest/work".into())});
        *usage.official_source.lock().unwrap()="vm-a".into();
        let mut session=crate::model::Session::new("vm-a:sid".into(),0);
        session.remote=Some("vm-a".into());session.agent=Some("claude".into());session.instance_id=Some("claude-v1-work".into());
        let info=usage.official_info_for_session(&session).unwrap();
        assert_eq!(info.provider_home.as_deref(),Some("/guest/work"));assert!(info.account.plan.is_none());
        assert!(usage.stats("today")["official"]["windowTokens"].is_null());
        session.agent=Some("codex".into());assert!(usage.official_info_for_session(&session).is_none());
        session.agent=Some("claude".into());session.remote=Some("vm-b".into());assert!(usage.official_info_for_session(&session).is_none());
        session.remote=Some("vm-a".into());session.instance_id=Some("claude-v1-personal".into());assert!(usage.official_info_for_session(&session).is_none());
        session.instance_id=None;assert!(usage.official_info_for_session(&session).is_none());
    }

    #[test]
    fn codex_accounts_with_equal_provider_ids_have_independent_totals_and_cursors() {
        let log = UsageLog::new(&format!("{}\n{}", serde_json::json!({"type":"session_meta","payload":{"id":"same-id","cwd":"/qa/project"}}),
            codex_event("2026-09-05T01:00:00Z", raw_usage(100, 20, 10, 0), raw_usage(100, 20, 10, 0))));
        let usage = usage_with_state(State::default());
        let first = CodexSourceFile { path: log.path().into(), key: "codex:work:same-id".into(), sid:"same-id".into(), instance_id:"work".into(), label:"Work".into(), home:"/qa/work".into() };
        let second = CodexSourceFile { key:"codex:personal:same-id".into(), instance_id:"personal".into(), label:"Personal".into(), home:"/qa/personal".into(), ..first.clone() };
        let at = usage.parse_codex_file_part_scoped(log.path(), 0, Some(&first));
        usage.parse_codex_file_part_scoped(log.path(), 0, Some(&second));
        assert_eq!(usage.for_session(&first.key).unwrap()["tok"],110.0);
        assert_eq!(usage.for_session(&second.key).unwrap()["tok"],110.0);
        assert_eq!(usage.for_session(&second.key).unwrap()["instanceLabel"],"Personal");
        assert!(usage.for_session("same-id").is_none());
        log.append(&codex_event("2026-09-05T01:00:03Z",raw_usage(100,20,10,0),raw_usage(100,20,10,0)));
        log.append(&codex_event("2026-09-05T01:00:05Z",raw_usage(160,30,20,0),raw_usage(60,10,10,0)));
        usage.parse_codex_file_part_scoped(log.path(),at,Some(&first));
        assert_eq!(usage.for_session(&first.key).unwrap()["tok"],180.0);
        assert_eq!(usage.for_session(&second.key).unwrap()["tok"],110.0);
    }

    #[test]
    fn codex_archive_move_preserves_logical_cursor_and_new_usage_only() {
        let log = UsageLog::new(&format!("{}\n{}", serde_json::json!({"type":"session_meta","payload":{"id":"archive-id","cwd":"/qa/project"}}),
            codex_event("2026-09-05T01:00:00Z",raw_usage(100,20,10,0),raw_usage(100,20,10,0))));
        let usage = usage_with_state(State::default());
        let mut source = CodexSourceFile { path: log.path().into(), key:"codex:personal:archive-id".into(), sid:"archive-id".into(), instance_id:"personal".into(), label:"Personal".into(), home:"/qa/personal".into() };
        let at = usage.parse_codex_file_part_scoped(log.path(),0,Some(&source));
        let archived = log.0.with_extension("archived.jsonl");
        fs::rename(&log.0,&archived).unwrap();
        source.path=archived.to_string_lossy().into_owned();
        use std::io::Write;
        let mut file=fs::OpenOptions::new().append(true).open(&archived).unwrap();
        file.write_all(codex_event("2026-09-05T01:00:03Z",raw_usage(100,20,10,0),raw_usage(100,20,10,0)).as_bytes()).unwrap();
        file.write_all(codex_event("2026-09-05T01:00:05Z",raw_usage(150,30,20,0),raw_usage(50,10,10,0)).as_bytes()).unwrap();
        usage.parse_codex_file_part_scoped(&source.path,at,Some(&source));
        assert_eq!(usage.for_session(&source.key).unwrap()["tok"],170.0);
        assert_eq!(usage.for_session(&source.key).unwrap()["requests"],2.0);
        let _=fs::remove_file(archived);
    }

    #[test]
    fn cost_cache_multiplier_compatibility() {
        let t = Tok { input: 1_000_000.0, out: 0.0, cw: 1_000_000.0, cr: 1_000_000.0, ..Default::default() };
        // Sonnet: 3 + 3*1.25 + 3*0.1 = 7.05
        assert!((t.cost("Sonnet") - 7.05).abs() < 1e-9);
    }

    /* -------- сканер Kimi -------- */

    #[test]
    fn unsupported_local_usage_does_not_mask_a_valid_remote_source() {
        // Observed from installed Claude 2.1.258: exit success, but no quota data.
        let cost = "Total cost: $0.0000\nTotal duration (API): 0s\n";
        let mut errors = Vec::new();
        assert!(official_candidate("local", Ok(cost.into()), &mut errors).is_none());
        assert!(official_candidate("offline", Err("connection failed".into()), &mut errors).is_none());
        let (official, source) = official_candidate("working-node", Ok(REAL_USAGE.into()), &mut errors)
            .expect("continue to a source with actual limits");
        assert_eq!(source, "working-node");
        assert_eq!(official.session.unwrap().pct, 62);
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("CLI не вернул лимиты подписки"));
        assert!(!errors[0].contains("$0.0000"));
    }

    #[test]
    fn cancelled_quota_refresh_releases_reservation_without_releasing_another_owner() {
        let busy = AtomicBool::new(false);
        let first = OfficialFetchGuard::acquire(&busy).unwrap();
        assert!(OfficialFetchGuard::acquire(&busy).is_none());
        assert!(busy.load(Ordering::SeqCst), "a rejected caller must not release the owner");
        drop(first); // Also happens when the async refresh future is cancelled.
        assert!(!busy.load(Ordering::SeqCst));
        assert!(OfficialFetchGuard::acquire(&busy).is_some());
    }

    #[test]
    fn official_parses_the_real_output() {
        let (session, week, model) = parse_official(REAL_USAGE).expect("живой формат обязан разбираться");
        let s = session.expect("сессия");
        assert_eq!(s.pct, 62);
        assert!(s.reset_at > 0, "время сброса сессии не разобралось");
        let w = week.expect("неделя");
        assert_eq!(w.pct, 94);
        assert!(w.reset_at > 0, "время сброса недели не разобралось");
        let m = model.expect("модельная неделя — та самая строка про Fable");
        assert_eq!(m.model, "Fable");
        assert_eq!(m.pct, 54);
        assert!(m.reset_at > 0, "«11pm» без минут обязан разбираться");
    }

    const KIMI_STATE_V2: &str =
        r#"{"id":"session_TEST","cwd":"/Users/me/Goool","createdAt":1787091343079}"#;
    const KIMI_STATE_V1: &str =
        r#"{"workDir":"/Users/me/Goool","createdAt":"2026-08-03T09:31:21.133Z"}"#;

    /// Запись расхода и её step.end-двойник — в живых логах они всегда парой.
    fn kimi_pair(other: i64, out: i64, cr: i64, cw: i64, ts: i64) -> String {
        format!(
            "{{\"type\":\"usage.record\",\"model\":\"kimi-code/k3\",\"usage\":{{\"inputOther\":{other},\"output\":{out},\"inputCacheRead\":{cr},\"inputCacheCreation\":{cw}}},\"usageScope\":\"turn\",\"time\":{ts}}}\n\
{{\"type\":\"context.append_loop_event\",\"event\":{{\"type\":\"step.end\",\"step\":1,\"usage\":{{\"inputOther\":{other},\"output\":{out},\"inputCacheRead\":{cr},\"inputCacheCreation\":{cw}}},\"finishReason\":\"tool_use\"}},\"time\":{ts}}}\n"
        )
    }

    /// `<root>/wd_*/session_TEST/{state.json,agents/<агент>/wire.jsonl}`.
    fn kimi_tree(name: &str, state_json: &str, agents: &[(&str, String)]) -> (PathBuf, Vec<String>) {
        let root = std::env::temp_dir().join(format!("jarvis-kimi-usage-{name}"));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("wd_proj_0123456789ab").join("session_TEST");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("state.json"), state_json).unwrap();
        let mut files = Vec::new();
        for (agent, body) in agents {
            let ad = dir.join("agents").join(agent);
            fs::create_dir_all(&ad).unwrap();
            let p = ad.join("wire.jsonl");
            fs::write(&p, body).unwrap();
            files.push(p.to_string_lossy().into_owned());
        }
        (root, files)
    }

    /// На диске лежит реальное ~/.jarvis/usage.json — считать по нему нельзя.
    fn fresh_usage() -> Usage {
        let u = Usage::load();
        *u.state.lock().unwrap() = State { v: STATE_V, ..Default::default() };
        u
    }

    fn kimi_scan(u: &Usage, files: &[String]) {
        for f in files {
            u.parse_kimi_file_part(f, 0);
        }
    }

    #[test]
    fn kimi_counts_usage_record_once_ignoring_step_end() {
        // step.end несёт побайтовый дубль usage.record — сложить оба значит удвоить
        let mut body = kimi_pair(100, 10, 1000, 5, 1787091343079);
        body.push_str("{\"type\":\"usage.record\",\"model\":\"kimi-code/k3\",\"usage\":{\"inputOther\":7,\"output\":1,\"inputCacheRead\":0,\"inputCacheCreation\":0},\"usageScope\":\"session\",\"time\":1787091344000}\n");
        let (root, files) = kimi_tree("nodouble", KIMI_STATE_V2, &[("main", body)]);
        let u = fresh_usage();
        kimi_scan(&u, &files);

        let st = u.state.lock().unwrap();
        let s = st.sessions.get("session_TEST").expect("сессия по имени каталога");
        assert_eq!(s.tok.total(), 1123.0, "одинарный расход: 1115 turn + 8 компакции");
        assert_eq!((s.tok.input, s.tok.out, s.tok.cr, s.tok.cw), (107.0, 11.0, 1000.0, 5.0));
        assert_eq!((s.model.as_str(), s.billing.as_str(), s.project.as_str()), ("K3", "kimi", "Goool"));
        drop(st);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn kimi_sums_over_all_agents() {
        // сабагенты жгут те же токены — расход сессии складывается по main + agent-N
        let (root, files) = kimi_tree(
            "agents",
            KIMI_STATE_V2,
            &[
                ("main", kimi_pair(100, 10, 0, 0, 1787091343079)),
                ("agent-0", kimi_pair(20, 3, 0, 0, 1787091343999)),
            ],
        );
        assert_eq!(files.len(), 2);
        let u = fresh_usage();
        kimi_scan(&u, &files);

        let st = u.state.lock().unwrap();
        assert_eq!(st.sessions.len(), 1, "агенты одной сессии — одна строка");
        assert_eq!(st.sessions["session_TEST"].tok.total(), 133.0);
        drop(st);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn kimi_cwd_from_both_state_versions() {
        for (name, state) in [("v2", KIMI_STATE_V2), ("v1", KIMI_STATE_V1)] {
            let (root, files) = kimi_tree(
                &format!("state-{name}"),
                state,
                &[("main", kimi_pair(1, 1, 0, 0, 1787091343079))],
            );
            let u = fresh_usage();
            kimi_scan(&u, &files);
            let st = u.state.lock().unwrap();
            assert_eq!(st.sessions["session_TEST"].project, "Goool", "state.json {name}");
            drop(st);
            let _ = fs::remove_dir_all(&root);
        }
        // без state.json — проект «другое», но токены не теряются
        let (root, files) = kimi_tree("state-none", "не json вовсе", &[("main", kimi_pair(1, 1, 0, 0, 1))]);
        let u = fresh_usage();
        kimi_scan(&u, &files);
        let st = u.state.lock().unwrap();
        assert_eq!(st.sessions["session_TEST"].project, "другое");
        drop(st);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn kimi_survives_broken_lines() {
        let mut body = String::from("{\"type\":\"usage.record\", это не json\n");
        // подстрока-приманка в чужой записи: тип обязан проверяться после разбора
        body.push_str("{\"type\":\"context.append_loop_event\",\"event\":{\"type\":\"tool.result\",\"text\":\"usage.record\"}}\n");
        body.push_str(&kimi_pair(50, 5, 0, 0, 1787091343079));
        let (root, files) = kimi_tree("broken", KIMI_STATE_V2, &[("main", body)]);
        let u = fresh_usage();
        kimi_scan(&u, &files);

        let st = u.state.lock().unwrap();
        assert_eq!(st.sessions["session_TEST"].tok.total(), 55.0, "битая строка не роняет и не искажает");
        drop(st);
        let _ = fs::remove_dir_all(&root);
    }

    /// usage.json — сотни килобайт, а скан идёт раз в 30 с круглые сутки:
    /// безусловная запись давала ~900 МБ на SSD в день у приложения, которое
    /// просто висит в менюбаре. Признак «писать» ровно один — сдвинулось
    /// смещение; на неизменившихся файлах его быть не должно.
    #[test]
    fn an_unchanged_scan_asks_for_no_write() {
        let (root, files) = kimi_tree(
            "nowrite",
            KIMI_STATE_V2,
            &[("main", kimi_pair(100, 10, 0, 0, 1787091343079))],
        );
        let u = fresh_usage();
        assert!(
            u.scan_files(files.clone(), Usage::parse_kimi_file_part),
            "первый проход разобрал новый файл"
        );
        assert!(
            !u.scan_files(files.clone(), Usage::parse_kimi_file_part),
            "файлы не менялись — писать нечего"
        );
        // дописали ход — снова есть что сохранить
        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(&files[0]).unwrap();
        f.write_all(kimi_pair(5, 1, 0, 0, 1787091350000).as_bytes()).unwrap();
        drop(f);
        assert!(u.scan_files(files, Usage::parse_kimi_file_part), "файл вырос");
        let _ = fs::remove_dir_all(root);
    }

    /// Ретеншен: карты агрегатов не росли бы вечно, но и лишнего не теряют.
    #[test]
    fn cost_uses_cache_multipliers() {
        let t = Tok { input: 1_000_000.0, out: 0.0, cw: 1_000_000.0, cr: 1_000_000.0, ..Default::default() };
        // Sonnet: 3 + 3*1.25 + 3*0.1 = 7.05
        assert!((t.cost("Sonnet") - 7.05).abs() < 1e-9);
    }

    #[test]
    fn prune_drops_only_what_is_past_the_horizon() {
        let now = 1787091343079;
        let t = Tok { input: 10.0, ..Default::default() };
        let mut st = State::default();
        Usage::add_record(&mut st, now, "Sonnet", "p", "plan", "свежая", t);
        Usage::add_record(&mut st, now - (RETAIN_DAYS + 5) * DAY_MS, "Sonnet", "p", "plan", "древняя", t);
        assert_eq!(st.hours.len(), 2);

        assert!(prune(&mut st, now), "что-то удалили — файл пора переписать");
        assert_eq!(st.hours.len(), 1, "старый час ушёл");
        assert!(st.sessions.contains_key("свежая"));
        assert!(!st.sessions.contains_key("древняя"));
        assert!(!prune(&mut st, now), "второй раз удалять нечего — и записи не будет");
    }

    #[test]
    fn window_rolls_over() {
        let mut st = State::default();
        Usage::add_record(&mut st, 0, "Sonnet", "p", "plan", "s1", Tok { input: 10.0, ..Default::default() });
        assert_eq!(st.window.tokens, 10.0);
        // через 6 часов — новое окно
        Usage::add_record(&mut st, 6 * 3_600_000, "Sonnet", "p", "plan", "s1", Tok { input: 5.0, ..Default::default() });
        assert_eq!(st.window.start, 6 * 3_600_000);
        assert_eq!(st.window.tokens, 5.0);
    }

    fn raw_usage(input: u64, cached: u64, output: u64, reasoning: u64) -> Value {
        serde_json::json!({
            "input_tokens": input, "cached_input_tokens": cached,
            "output_tokens": output, "reasoning_output_tokens": reasoning,
            "total_tokens": input + output,
        })
    }

    #[test]
    fn codex_cumulative_counters_deduplicate_and_include_reasoning_once() {
        let mut cursor = CodexCursor::default();
        let first = raw_usage(100, 40, 20, 8);
        let info = serde_json::json!({ "total_token_usage": first, "last_token_usage": first });
        let first = cursor.take_usage(&info, "first").unwrap();
        assert_eq!(first.total(), 120.0);
        assert_eq!((first.input, first.cr, first.out, first.reasoning), (60.0, 40.0, 20.0, 8.0));
        assert!(cursor.take_usage(&info, "later quota refresh").is_none());
        let next = serde_json::json!({
            "total_token_usage": raw_usage(250, 100, 40, 12),
            // Intentionally stale: cumulative growth is authoritative.
            "last_token_usage": raw_usage(100, 40, 20, 8),
        });
        let delta = cursor.take_usage(&next, "next").unwrap();
        assert_eq!(delta.total(), 170.0);
        assert_eq!((delta.input, delta.cr, delta.out, delta.reasoning), (90.0, 60.0, 20.0, 4.0));
        let without_last = serde_json::json!({ "total_token_usage": raw_usage(300, 120, 50, 16) });
        assert_eq!(cursor.take_usage(&without_last, "third").unwrap().total(), 60.0);
        assert!(cursor.take_usage(&Value::Null, "quota only").is_none());
    }

    #[test]
    fn codex_reset_and_legacy_fallback_do_not_recount_a_request() {
        let mut cursor = CodexCursor::default();
        cursor.take_usage(&serde_json::json!({ "total_token_usage": raw_usage(1000, 500, 200, 80) }), "first");
        let reset = serde_json::json!({
            "total_token_usage": raw_usage(100, 40, 20, 8),
            "last_token_usage": raw_usage(30, 10, 5, 2),
        });
        assert_eq!(cursor.take_usage(&reset, "reset").unwrap().total(), 35.0);
        assert!(cursor.take_usage(&reset, "reset again").is_none());
        let legacy = serde_json::json!({ "last_token_usage": raw_usage(30, 10, 5, 2) });
        assert_eq!(cursor.take_usage(&legacy, "same timestamp").unwrap().total(), 35.0);
        assert!(cursor.take_usage(&legacy, "same timestamp").is_none());
        // Restored totals include the fallback event already counted above.
        let restored = serde_json::json!({ "total_token_usage": raw_usage(130, 50, 25, 10) });
        assert!(cursor.take_usage(&restored, "restored").is_none());
    }

    fn usage_with_state(state: State) -> Usage {
        Usage {
            msg_seen: Mutex::new(OrderedRing::from_iter(state.msg_ids.iter().cloned())),
            state: Mutex::new(state), billing_cache: Mutex::new(HashMap::new()),
            official: Mutex::new(None), official_source: Mutex::new(String::new()), official_busy: AtomicBool::new(false),
            official_err: Mutex::new(None), scanning: AtomicBool::new(false),
            persist_pending: AtomicBool::new(false),
        }
    }

    struct UsageLog(PathBuf);

    impl UsageLog {
        fn new(text: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "jarvis-usage-{}-{}.jsonl", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::write(&path, text).unwrap();
            Self(path)
        }
        fn path(&self) -> &str { self.0.to_str().unwrap() }
        fn append(&self, text: &str) {
            use std::io::Write;
            fs::OpenOptions::new().append(true).open(&self.0).unwrap().write_all(text.as_bytes()).unwrap();
        }
    }

    impl Drop for UsageLog {
        fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
    }

    fn codex_event(timestamp: &str, total: Value, last: Value) -> String {
        format!("{}\n", serde_json::json!({
            "timestamp": timestamp, "type": "event_msg",
            "payload": { "type": "token_count", "info": {
                "total_token_usage": total, "last_token_usage": last,
            } },
        }))
    }

    #[test]
    fn codex_incremental_scan_survives_restart_and_partial_lines() {
        let first = raw_usage(100, 40, 20, 8);
        let log = UsageLog::new(&format!(
            "{}\n{}\n{}",
            serde_json::json!({ "type": "session_meta", "payload": { "id": "usage-session", "cwd": "/work/project" } }),
            serde_json::json!({ "type": "turn_context", "payload": { "model": "gpt-5.5" } }),
            codex_event("2026-09-05T01:00:00Z", first.clone(), first.clone()),
        ));
        let usage = usage_with_state(State::default());
        let offset = usage.parse_codex_file_part(log.path(), 0);
        usage.state.lock().unwrap().offsets.insert(log.path().into(), offset);
        assert_eq!(usage.for_session("usage-session").unwrap()["tok"], 120.0);

        // Same persisted state as load() uses, including the model/cumulative cursor.
        let state: State = serde_json::from_str(&serde_json::to_string(&*usage.state.lock().unwrap()).unwrap()).unwrap();
        let restarted = usage_with_state(state);
        let duplicate = codex_event("2026-09-05T01:00:02Z", first.clone(), first);
        let next = codex_event("2026-09-05T01:00:05Z", raw_usage(250, 100, 40, 12), raw_usage(150, 60, 20, 4));
        log.append(&duplicate);
        log.append(&next[..next.len() / 2]);
        let after_duplicate = restarted.parse_codex_file_part(log.path(), offset);
        assert_eq!(after_duplicate, offset + duplicate.len() as u64);
        assert_eq!(restarted.for_session("usage-session").unwrap()["tok"], 120.0);
        log.append(&next[next.len() / 2..]);
        let final_offset = restarted.parse_codex_file_part(log.path(), after_duplicate);
        assert_eq!(final_offset, fs::metadata(&log.0).unwrap().len());
        let session = restarted.for_session("usage-session").unwrap();
        assert_eq!(session["tok"], 290.0);
        assert_eq!(session["model"], "gpt-5.5");
        assert_eq!(session["requests"], 2.0);
        assert_eq!(session["reasoningTokens"], 12.0);
        assert_eq!(session["cacheHitPct"], 40.0);
        assert_eq!(session["costEstimated"], true);
        assert_eq!(session["source"], "local-transcripts");
        assert_eq!(restarted.state.lock().unwrap().sessions["usage-session"].project, "project");
    }

    #[test]
    fn claude_usage_accepts_whitespace_deduplicates_and_preserves_byte_offsets() {
        let entry = serde_json::json!({
            "type": "assistant", "sessionId": "claude-session", "cwd": "/work/claude-project",
            "timestamp": "2026-09-05T01:00:00Z", "message": {
                "id": "claude-message", "model": "claude-sonnet-4-5", "usage": {
                    "input_tokens": 100, "output_tokens": 20,
                    "cache_creation_input_tokens": 10, "cache_read_input_tokens": 70,
                },
            },
        }).to_string().replace("\":", "\": ");
        let log = UsageLog::new("");
        fs::write(&log.0, b"\xff\n").unwrap(); // Invalid UTF-8 must not shift the byte cursor.
        log.append(&format!("{entry}\n{entry}\n"));
        let usage = usage_with_state(State::default());
        let offset = usage.parse_file_part(log.path(), 0);
        assert_eq!(offset, fs::metadata(&log.0).unwrap().len());
        let session = usage.for_session("claude-session").unwrap();
        assert_eq!(session["tok"], 200.0);
        assert_eq!(session["requests"], 1.0);
        assert_eq!(session["cacheReadTokens"], 70.0);
    }
}
