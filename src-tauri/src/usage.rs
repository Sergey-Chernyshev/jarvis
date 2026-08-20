//! Учёт usage — слой A: транскрипты ~/.claude/projects/**∕*.jsonl.
//! Бесплатно, любой план, покрывает и API-проекты (транскрипт пишется всегда).
//!
//! usage-блок есть в каждом ходе ассистента; чанки стрима дублируют запись с
//! одним message.id и идентичным usage — дедуп по message.id (проверено).
//! Деньги: прайс per-model; для подписки это «сколько стоило бы по API» —
//! различаем биллинг per-проект: .claude/settings.json с API-ключом → 'api:<host>'.
//!
//! Формат ~/.jarvis/usage.json не менялся (v2) — накопленное переживает порт.

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
const STATE_V: i64 = 2; // v2: биллинг = 'api:<host>' вместо плоского 'api'
const DAY_MS: i64 = 86_400_000;

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
        // ОЦЕНКА Moonshot (то же, что backend::kimi::price) — иначе Kimi считался
        // бы по дефолту Sonnet и врал в пять раз.
        "K3" | "K3-256k" | "K2.7 Coding" | "K2.7 Coding Highspeed" => (0.6, 2.5),
        _ => (3.0, 15.0), // Sonnet и дефолт
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct Tok {
    #[serde(rename = "in")]
    input: f64,
    out: f64,
    cw: f64,
    cr: f64,
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
    first: i64,
    last: i64,
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
    window: WindowAgg,
    backfilled: bool,
    #[serde(rename = "msgIds")]
    msg_ids: Vec<String>,
    v: i64,
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
    pub at: i64,
    pub account: Account,
}

#[derive(Debug, Clone)]
struct Official {
    session: Option<PctReset>,
    week: Option<PctReset>,
    week_model: Option<ModelWeek>,
    at: i64,
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
    /// Почему лимитов нет — по всем источникам разом. Молчание добытчика
    /// неотличимо от «всё хорошо», и его пришлось запретить.
    official_err: Mutex<Option<String>>,
    scanning: AtomicBool,
    official_busy: AtomicBool,
    persist_pending: AtomicBool,
}

fn state_file() -> PathBuf {
    jarvis_dir().join("usage.json")
}

fn projects_dir() -> PathBuf {
    claude_dir().join("projects")
}

fn codex_sessions_dir() -> PathBuf {
    crate::util::codex_dir().join("sessions")
}

fn kimi_sessions_dir() -> PathBuf {
    crate::backend::kimi::kimi_home().join("sessions")
}

/// cwd + session_id из ПЕРВОЙ строки rollout (session_meta). Нужно при
/// инкрементальном скане: session_meta уже ниже from_offset, иначе токены
/// уходят в "unknown"/"другое".
fn codex_meta_head(file: &str) -> (Option<String>, String) {
    use std::io::BufRead;
    let Ok(f) = fs::File::open(file) else { return (None, "unknown".into()) };
    let mut first = String::new();
    if std::io::BufReader::new(f).read_line(&mut first).is_err() {
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
        let mut state: State = fs::read_to_string(state_file())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        if state.v != STATE_V {
            // схема агрегатов изменилась — пересобираем с нуля (backfill ~1.5с)
            state = State { v: STATE_V, ..Default::default() };
        }
        let msg_seen = OrderedRing::from_iter(state.msg_ids.iter().cloned());
        Self {
            state: Mutex::new(state),
            msg_seen: Mutex::new(msg_seen),
            billing_cache: Mutex::new(HashMap::new()),
            official: Mutex::new(None),
            official_source: Mutex::new(String::new()),
            official_err: Mutex::new(None),
            scanning: AtomicBool::new(false),
            official_busy: AtomicBool::new(false),
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
            let json = {
                let mut state = u.state.lock().unwrap();
                state.msg_ids = u.msg_seen.lock().unwrap().last_n(3000); // ринг последних id
                serde_json::to_string(&*state).ok()
            };
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
        s.cost += c;
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
        let text = String::from_utf8_lossy(&buf);
        let Some(last_nl) = text.rfind('\n') else { return from_offset }; // одна недописанная строка
        let consumed = text[..=last_nl].len() as u64;
        let text = &text[..last_nl];

        let mut cwd: Option<String> = None;
        for line in text.split('\n') {
            if !line.contains("\"type\":\"assistant\"") || !line.contains("\"usage\"") {
                if cwd.is_none() && line.contains("\"cwd\"") {
                    if let Ok(v) = serde_json::from_str::<Value>(line) {
                        cwd = v.get("cwd").and_then(Value::as_str).map(String::from);
                    }
                }
                continue;
            }
            let Ok(e) = serde_json::from_str::<Value>(line) else { continue };
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
            let num = |k: &str| u0.get(k).and_then(Value::as_f64).unwrap_or(0.0);
            let entry_cwd = e.get("cwd").and_then(Value::as_str).map(String::from).or(cwd.clone());
            let billing = self.detect_billing(entry_cwd.as_deref());
            let model = friendly_model_or_other(m.get("model").and_then(Value::as_str).unwrap_or(""));
            let project = cwd.as_deref().map(basename).unwrap_or_else(|| "другое".into());
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
                },
            );
        }
        from_offset + consumed
    }

    fn list_transcripts() -> Vec<String> {
        let mut out = Vec::new();
        let Ok(dirs) = fs::read_dir(projects_dir()) else { return out };
        for d in dirs.filter_map(|e| e.ok()) {
            if !d.path().is_dir() {
                continue;
            }
            let Ok(files) = fs::read_dir(d.path()) else { continue };
            for f in files.filter_map(|e| e.ok()) {
                let p = f.path();
                if p.extension().is_some_and(|x| x == "jsonl") {
                    out.push(p.to_string_lossy().into_owned());
                }
            }
        }
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
        // Все три источника проходим всегда (`|=`, не `||`): скан на то и скан.
        let mut changed = self.scan_files(Self::list_transcripts(), Self::parse_file_part);
        // Codex rollouts: token_count.last_token_usage (per-turn) → та же агрегация.
        changed |= self.scan_files(Self::list_codex_rollouts(), Self::parse_codex_file_part);
        // Kimi wire.jsonl: usage.record всех агентов сессии → та же агрегация.
        changed |= self.scan_files(Self::list_kimi_wires(), Self::parse_kimi_file_part);
        {
            let mut seen = self.msg_seen.lock().unwrap();
            if seen.len() > 6000 {
                seen.trim_to(3000);
            }
        }
        {
            let mut state = self.state.lock().unwrap();
            changed |= !state.backfilled; // первый скан обязан лечь на диск
            state.backfilled = true;
            changed |= prune(&mut state, now_ms());
        }
        if changed {
            self.persist();
        }
        self.scanning.store(false, Ordering::SeqCst);
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

    /// Все rollout-файлы Codex: ~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl.
    fn list_codex_rollouts() -> Vec<String> {
        fn walk(dir: &Path, out: &mut Vec<String>) {
            let Ok(rd) = fs::read_dir(dir) else { return };
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "jsonl") {
                    out.push(p.to_string_lossy().into_owned());
                }
            }
        }
        let mut out = Vec::new();
        walk(&codex_sessions_dir(), &mut out);
        out
    }

    /// Разбор rollout Codex: `event_msg.token_count.last_token_usage` (per-turn,
    /// НЕ total — иначе двойной счёт). model — последний `turn_context.model`;
    /// cwd/sid — из `session_meta`. billing="codex". Маппинг в общий `Tok`:
    /// input_tokens включает cached (конвенция OpenAI) → input = input−cached,
    /// cr = cached, out = output+reasoning, cw = 0 (Codex не делит cache-creation).
    fn parse_codex_file_part(&self, file: &str, from_offset: u64) -> u64 {
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

        // cwd/sid из первой строки (session_meta) — переживают инкрементальный скан
        let (mut cwd, mut sid) = codex_meta_head(file);
        let mut model = String::new();
        for line in text.split('\n') {
            if line.contains("\"session_meta\"") {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    if let Some(p) = v.get("payload") {
                        cwd = p.get("cwd").and_then(Value::as_str).map(String::from);
                        if let Some(id) = p.get("id").and_then(Value::as_str) {
                            sid = id.to_string();
                        }
                    }
                }
                continue;
            }
            if line.contains("\"turn_context\"") {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    if let Some(m) = v.get("payload").and_then(|p| p.get("model")).and_then(Value::as_str) {
                        model = m.to_string();
                    }
                }
                continue;
            }
            if !line.contains("\"token_count\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let Some(lt) = v.pointer("/payload/info/last_token_usage") else { continue };
            let num = |k: &str| lt.get(k).and_then(Value::as_f64).unwrap_or(0.0);
            let ts = v
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(crate::transcript::parse_ts)
                .unwrap_or_else(now_ms);
            let cached = num("cached_input_tokens");
            let tok = Tok {
                input: (num("input_tokens") - cached).max(0.0),
                out: num("output_tokens") + num("reasoning_output_tokens"),
                cw: 0.0,
                cr: cached,
            };
            let friendly = crate::backend::backend(crate::backend::Agent::Codex).friendly_model(&model);
            let project = cwd.as_deref().map(basename).unwrap_or_else(|| "другое".into());
            Self::add_record(&mut self.state.lock().unwrap(), ts, &friendly, &project, "codex", &sid, tok);
        }
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
                    serde_json::json!({
                        "id": id, "project": s.project, "model": s.model,
                        "billing": s.billing, "tok": s.tok.total(), "cost": s.cost,
                    })
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
        // как в Electron: блок официальных лимитов — только при данных СЕССИИ
        let official_out = self.official_info().filter(|o| o.session.is_some()).map(|o| {
            let win_start = o.session.as_ref().map(|s| s.reset_at - WINDOW_MS).unwrap_or(0);
            let win_tok: f64 = if win_start > 0 {
                self.range_hours(win_start).iter().map(|r| r.tok.total()).sum()
            } else {
                0.0
            };
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
        Some(serde_json::json!({
            "tok": s.tok.total(), "cost": s.cost, "billing": s.billing, "model": s.model,
        }))
    }

    /* ---------- официальные лимиты подписки ---------- */
    /* Источник правды — headless `claude -p "/usage"`: проценты и времена сброса
     * сессии/недели. Тариф и аккаунт — из ~/.claude.json (oauthAccount). */

    pub fn official_info(&self) -> Option<OfficialInfo> {
        let o = self.official.lock().unwrap().clone()?;
        Some(OfficialInfo {
            session: o.session,
            week: o.week,
            week_model: o.week_model,
            source: self.official_source.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            at: o.at,
            account: read_account(),
        })
    }

    /// Свежий /usage как можно скорее (после подтверждённого лимита).
    pub fn refresh_official_soon(self: &Arc<Self>, d: &Arc<Daemon>) {
        let u = self.clone();
        let d = d.clone();
        tauri::async_runtime::spawn(async move {
            u.fetch_official(&d).await;
        });
    }

    /// Достать текст `/usage`: сначала локально, затем с узлов по порядку.
    ///
    /// Человек, работающий на узле, может быть не авторизован локально вовсе —
    /// тогда правда о лимитах живёт только там. Ошибки всех источников
    /// собираются в одну строку: чинить будут по ней.
    async fn obtain_official(self: &Arc<Self>, d: &Arc<Daemon>) -> Result<(String, String), String> {
        let mut errs: Vec<String> = Vec::new();
        if crate::claude_bin::resolve_claude_bin().is_none() {
            errs.push("локально: claude не найден".into());
        } else {
            match crate::claude_bin::run_claude(
                &["-p", "--no-session-persistence", "/usage"],
                Duration::from_secs(90),
            )
            .await
            {
                Some(text) => return Ok((text, "local".into())),
                None => errs.push("локально: /usage не ответил — не авторизован или нет сети".into()),
            }
        }
        for node in d.remotes.all() {
            let name = node.cfg.name.clone();
            match node.client() {
                Ok(client) => match client.usage_text(false).await {
                    Ok(text) => return Ok((text, name)),
                    Err(e) => errs.push(format!("{name}: {e}")),
                },
                Err(e) => errs.push(format!("{name}: {e}")),
            }
        }
        Err(if errs.is_empty() { "источников лимитов нет".into() } else { errs.join(" · ") })
    }

    pub async fn fetch_official(self: &Arc<Self>, d: &Arc<Daemon>) {
        if self.official_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let got = self.obtain_official(d).await;
        self.official_busy.store(false, Ordering::SeqCst);
        let (text, source) = match got {
            Ok(x) => x,
            Err(why) => {
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
        let Some((session, week, week_model)) = parse_official(&text) else {
            let head = crate::util::ellipsize(&crate::util::one_line(&text), 120);
            *self.official_err.lock().unwrap_or_else(|p| p.into_inner()) =
                Some(format!("{source}: формат /usage не разобрался — {head}"));
            crate::log::line(&format!("[usage] формат /usage ({source}) не разобрался: {head}"));
            return; // формат уехал — не перетираем
        };
        *self.official_err.lock().unwrap_or_else(|p| p.into_inner()) = None;
        {
            let mut src = self.official_source.lock().unwrap_or_else(|p| p.into_inner());
            if *src != source {
                crate::log::line(&format!("[usage] лимиты приехали: источник {source}"));
                *src = source.clone();
            }
        }
        let prev_pct = self
            .official
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|o| o.session.as_ref().map(|s| s.pct))
            .unwrap_or(0);
        let warn = session.as_ref().filter(|s| prev_pct < 90 && s.pct >= 90).cloned();
        *self.official.lock().unwrap() = Some(Official {
            session,
            week,
            week_model,
            at: now_ms(),
        });
        // предупреждение ДО стены: пересекли 90% окна
        if let Some(w) = warn {
            let plan = read_account().plan.unwrap_or_default();
            d.notify(
                &format!(
                    "Claude{} — окно почти исчерпано",
                    if plan.is_empty() { String::new() } else { format!(" {plan}") }
                ),
                &format!("{}% использовано · сброс через {}", w.pct, fmt_reset_in(w.reset_at)),
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

/// "Jun 11 at 9:30pm (Europe/Moscow)" → мс эпохи (МСК = UTC+3 круглый год).
/// Разбор официального `/usage`: сессия, неделя и недельное окно модели.
///
/// Чистая функция ради тестов на живом выводе: формат внутренний и дрейфует,
/// и каждый дрейф до сих пор замечал пользователь, а не тест.
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

    /// Живой вывод `claude /usage` от 2026-08-10 — дословно. Каждый дрейф
    /// формата до этого замечал пользователь, а не тест; теперь наоборот.
    const REAL_USAGE: &str = "You are currently using your subscription to power your Claude Code usage\n\n\
Current session: 62% used · resets Aug 10, 6:59pm (UTC)\n\
Current week (all models): 94% used · resets Aug 10, 10:59pm (UTC)\n\
Current week (Fable): 54% used · resets Aug 10, 11pm (UTC)\n";

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

    #[test]
    fn old_sonnet_format_still_parses() {
        let text = "Current session: 10% used · resets Aug 10 at 6:59pm\n\
Current week (all models): 20% used · resets Aug 12 at 7am\n\
Current week (Sonnet only): 30% used\n";
        let (_, _, model) = parse_official(text).unwrap();
        let m = model.unwrap();
        assert_eq!(m.model, "Sonnet only");
        assert_eq!(m.pct, 30);
    }

    #[test]
    fn reset_date_honors_utc_marker() {
        use chrono::{Datelike, TimeZone};
        let ts = parse_reset_date("Aug 10, 6:59pm (UTC)");
        assert!(ts > 0);
        let dt = chrono::Utc.timestamp_millis_opt(ts).single().unwrap();
        assert_eq!((dt.month(), dt.day()), (8, 10));
        assert_eq!((chrono::Timelike::hour(&dt), chrono::Timelike::minute(&dt)), (18, 59));

        // Без пометки — местное время машины, а не зашитый чей-то пояс.
        let local = parse_reset_date("Aug 10, 6:59pm");
        let ldt = chrono::Local.timestamp_millis_opt(local).single().unwrap();
        assert_eq!((chrono::Timelike::hour(&ldt), chrono::Timelike::minute(&ldt)), (18, 59));
    }

    #[test]
    fn reset_date_edge_forms() {
        assert!(parse_reset_date("Aug 10, 11pm (UTC)") > 0, "без минут");
        assert!(parse_reset_date("Aug 10 at 6:59pm") > 0, "старый формат с at");
        assert!(parse_reset_date("August 10, 6:59pm (UTC)") > 0, "полное имя месяца");
        assert_eq!(parse_reset_date("совсем не дата"), 0);
        assert_eq!(parse_reset_date(""), 0);
        // 12am — полночь, не полдень.
        use chrono::Timelike;
        let ts = parse_reset_date("Aug 10, 12am (UTC)");
        let dt = chrono::DateTime::from_timestamp_millis(ts).unwrap();
        assert_eq!(dt.hour(), 0);
    }

    #[test]
    fn billing_host_extraction() {
        assert_eq!(url_host("https://proxy.corp.dev/v1"), Some("proxy.corp.dev".into()));
        assert_eq!(url_host("http://localhost:8080"), Some("localhost".into()));
        assert_eq!(url_host("мусор"), None);
    }

    #[test]
    fn reset_date_is_msk() {
        // 9:30pm МСК = 18:30 UTC того же дня
        let ts = parse_reset_date("Jun 11 at 9:30pm (Europe/Moscow)");
        assert!(ts > 0);
        let d = chrono::DateTime::from_timestamp_millis(ts).unwrap();
        assert_eq!(d.format("%m-%d %H:%M").to_string(), "06-11 18:30");
    }

    #[test]
    fn cost_uses_cache_multipliers() {
        let t = Tok { input: 1_000_000.0, out: 0.0, cw: 1_000_000.0, cr: 1_000_000.0 };
        // Sonnet: 3 + 3*1.25 + 3*0.1 = 7.05
        assert!((t.cost("Sonnet") - 7.05).abs() < 1e-9);
    }

    /* -------- сканер Kimi -------- */

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
}
