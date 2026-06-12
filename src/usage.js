/**
 * Учёт usage — слой A: транскрипты ~/.claude/projects/**∕*.jsonl.
 * Бесплатно, любой план, покрывает и API-проекты (транскрипт пишется всегда).
 *
 * usage-блок есть в каждом ходе ассистента; чанки стрима дублируют запись с
 * одним message.id и идентичным usage — дедуп по message.id (проверено).
 * Деньги: прайс per-model; для подписки это «сколько стоило бы по API» —
 * различаем биллинг per-проект: .claude/settings.json с API-ключом → 'api'.
 * Поле source заложено под будущие слои (otel / admin_api).
 */

const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { execFile } = require('node:child_process');

const PROJECTS = path.join(os.homedir(), '.claude', 'projects');
const STATE_FILE = path.join(os.homedir(), '.jarvis', 'usage.json');
const WINDOW_MS = 5 * 60 * 60 * 1000; // 5ч-окно подписочных лимитов

// $/1M токенов; кэш: запись ×1.25 input, чтение ×0.1 input (подход ccusage)
const PRICES = {
  Opus: { in: 15, out: 75 },
  Fable: { in: 15, out: 75 }, // публичного прайса нет — считаем как Opus
  Sonnet: { in: 3, out: 15 },
  Haiku: { in: 1, out: 5 },
};

let state = {
  offsets: {},   // file → прочитано байт
  hours: {},     // "YYYY-MM-DD HH|model|project|billing" → {in,out,cw,cr,cost,n}
  sessions: {},  // sessionId → {project,model,billing,in,out,cw,cr,cost,first,last}
  window: { start: 0, tokens: 0, cost: 0 },
  backfilled: false,
};
let msgSeen = new Set(); // дедуп message.id (ринг)
let persistTimer = null;
let scanning = false;
const billingCache = new Map(); // cwd → 'api' | 'plan'

/* ---------- утилиты ---------- */

function friendlyModel(id) {
  const v = String(id || '').toLowerCase();
  if (v.includes('opus')) return 'Opus';
  if (v.includes('sonnet')) return 'Sonnet';
  if (v.includes('haiku')) return 'Haiku';
  if (v.includes('fable')) return 'Fable';
  if (v.includes('mythos')) return 'Mythos';
  return 'другая';
}

function cost(u, model) {
  const p = PRICES[model] || PRICES.Sonnet;
  return (u.in * p.in + u.out * p.out + u.cw * p.in * 1.25 + u.cr * p.in * 0.1) / 1e6;
}

// 'plan' либо 'api:<host>' — конфиги бывают разные (прокси, шлюзы),
// различаем их по hostname из ANTHROPIC_BASE_URL
function detectBilling(cwd) {
  if (!cwd) return 'plan';
  if (billingCache.has(cwd)) return billingCache.get(cwd);
  let mode = 'plan';
  for (const f of [path.join(cwd, '.claude', 'settings.json'), path.join(cwd, '.claude', 'settings.local.json')]) {
    try {
      const s = JSON.parse(fs.readFileSync(f, 'utf8'));
      const env = (s && s.env) || {};
      const hasKey = env.ANTHROPIC_API_KEY || env.ANTHROPIC_AUTH_TOKEN || (s && s.apiKeyHelper);
      const base = env.ANTHROPIC_BASE_URL;
      if (hasKey || base) {
        let host = 'api.anthropic.com';
        if (base) { try { host = new URL(base).hostname; } catch {} }
        mode = `api:${host}`;
        break;
      }
    } catch {}
  }
  billingCache.set(cwd, mode);
  return mode;
}

const isApi = (b) => b !== 'plan';

const STATE_V = 2; // v2: биллинг = 'api:<host>' вместо плоского 'api'

function loadState() {
  try {
    const parsed = JSON.parse(fs.readFileSync(STATE_FILE, 'utf8'));
    if (parsed && typeof parsed === 'object') state = { ...state, ...parsed };
  } catch {}
  if (state.v !== STATE_V) {
    // схема агрегатов изменилась — пересобираем с нуля (backfill ~1.5с)
    state = {
      offsets: {}, hours: {}, sessions: {},
      window: { start: 0, tokens: 0, cost: 0 },
      backfilled: false, v: STATE_V,
    };
  }
  msgSeen = new Set(Array.isArray(state.msgIds) ? state.msgIds : []);
}

function persist() {
  if (persistTimer) return;
  persistTimer = setTimeout(() => {
    persistTimer = null;
    try {
      state.msgIds = [...msgSeen].slice(-3000); // ринг последних id
      fs.writeFileSync(STATE_FILE, JSON.stringify(state));
    } catch {}
  }, 2000);
}

/* ---------- разбор транскриптов ---------- */

function addRecord(ts, model, project, billing, sessionId, u) {
  const c = cost(u, model);
  const d = new Date(ts);
  const hourKey = `${d.toISOString().slice(0, 13)}|${model}|${project}|${billing}`;
  const h = state.hours[hourKey] ||= { in: 0, out: 0, cw: 0, cr: 0, cost: 0, n: 0 };
  h.in += u.in; h.out += u.out; h.cw += u.cw; h.cr += u.cr; h.cost += c; h.n += 1;

  const s = state.sessions[sessionId] ||= {
    project, billing, model, in: 0, out: 0, cw: 0, cr: 0, cost: 0, first: ts, last: ts,
  };
  s.in += u.in; s.out += u.out; s.cw += u.cw; s.cr += u.cr; s.cost += c;
  s.model = model; s.billing = billing; s.project = project;
  if (ts > s.last) s.last = ts;
  if (ts < s.first) s.first = ts;

  // 5ч-окно: новое окно открывает первый запрос после истечения прошлого
  if (!state.window.start || ts >= state.window.start + WINDOW_MS) {
    if (ts > (state.window.start || 0)) state.window = { start: ts, tokens: 0, cost: 0 };
  }
  if (ts >= state.window.start && ts < state.window.start + WINDOW_MS) {
    state.window.tokens += u.in + u.out + u.cw + u.cr;
    state.window.cost += c;
  }
}

function parseFilePart(file, fromOffset) {
  let fd, size;
  try {
    size = fs.statSync(file).size;
    if (size <= fromOffset) return fromOffset;
    fd = fs.openSync(file, 'r');
  } catch { return fromOffset; }
  try {
    const buf = Buffer.alloc(size - fromOffset);
    fs.readSync(fd, buf, 0, buf.length, fromOffset);
    let text = buf.toString('utf8');
    let consumed = buf.length;
    const lastNl = text.lastIndexOf('\n');
    if (lastNl === -1) return fromOffset; // одна недописанная строка
    consumed = Buffer.byteLength(text.slice(0, lastNl + 1), 'utf8');
    text = text.slice(0, lastNl);

    let cwd = null;
    for (const line of text.split('\n')) {
      if (!line.includes('"type":"assistant"') || !line.includes('"usage"')) {
        if (!cwd && line.includes('"cwd"')) {
          try { cwd = JSON.parse(line).cwd || null; } catch {}
        }
        continue;
      }
      let e;
      try { e = JSON.parse(line); } catch { continue; }
      const m = e && e.message;
      const u0 = m && m.usage;
      if (!u0 || !m.id || msgSeen.has(m.id)) continue;
      msgSeen.add(m.id);
      if (!cwd && e.cwd) cwd = e.cwd;
      const ts = Date.parse(e.timestamp) || Date.now();
      addRecord(
        ts,
        friendlyModel(m.model),
        cwd ? path.basename(cwd) : 'другое',
        detectBilling(e.cwd || cwd),
        e.sessionId || 'unknown',
        {
          in: u0.input_tokens || 0,
          out: u0.output_tokens || 0,
          cw: u0.cache_creation_input_tokens || 0,
          cr: u0.cache_read_input_tokens || 0,
        },
      );
    }
    return fromOffset + consumed;
  } finally {
    try { fs.closeSync(fd); } catch {}
  }
}

function listTranscripts() {
  const out = [];
  let dirs;
  try { dirs = fs.readdirSync(PROJECTS, { withFileTypes: true }); } catch { return out; }
  for (const d of dirs) {
    if (!d.isDirectory()) continue;
    const dir = path.join(PROJECTS, d.name);
    let files;
    try { files = fs.readdirSync(dir); } catch { continue; }
    for (const f of files) if (f.endsWith('.jsonl')) out.push(path.join(dir, f));
  }
  return out;
}

function scan() {
  if (scanning) return;
  scanning = true;
  try {
    for (const file of listTranscripts()) {
      const prev = state.offsets[file] || 0;
      const next = parseFilePart(file, prev);
      if (next !== prev) state.offsets[file] = next;
    }
    if (msgSeen.size > 6000) msgSeen = new Set([...msgSeen].slice(-3000));
    state.backfilled = true;
    persist();
  } finally {
    scanning = false;
  }
}

/* ---------- агрегаты для UI ---------- */

const tokOf = (a) => a.in + a.out + a.cw + a.cr;

function rangeHours(sinceMs) {
  const out = [];
  for (const [key, a] of Object.entries(state.hours)) {
    const [hour, model, project, billing] = key.split('|');
    const ts = Date.parse(hour + ':00:00Z');
    if (Number.isNaN(ts) || ts < sinceMs) continue;
    out.push({ ts, hour, model, project, billing, ...a });
  }
  return out;
}

function sumBy(rows, keyFn) {
  const map = new Map();
  for (const r of rows) {
    const k = keyFn(r);
    const a = map.get(k) || { tok: 0, cost: 0, api: 0, plan: 0, n: 0 };
    const t = tokOf(r);
    a.tok += t; a.cost += r.cost; a.n += r.n;
    if (isApi(r.billing)) a.api += r.cost; else a.plan += r.cost;
    map.set(k, a);
  }
  return map;
}

// сутки обнуляются в 03:00 МСК — это ровно 00:00 UTC (МСК = UTC+3, без DST),
// поэтому граница дня совпадает с UTC-сутками наших часовых агрегатов
const DAY_MS = 86400e3;
const dayStartMs = (now) => Math.floor(now / DAY_MS) * DAY_MS;

function stats(period) {
  const now = Date.now();
  const dayStart = dayStartMs(now);
  const since = period === 'week' ? dayStart - 6 * DAY_MS : dayStart;
  const rows = rangeHours(since);

  const total = { tok: 0, api: 0, plan: 0, n: 0 };
  for (const r of rows) {
    total.tok += tokOf(r);
    total.n += r.n;
    if (isApi(r.billing)) total.api += r.cost; else total.plan += r.cost;
  }

  // серия: сегодня — по часам от границы суток; неделя — по дням
  const series = [];
  if (period === 'week') {
    for (let i = 6; i >= 0; i--) {
      const start = dayStart - i * DAY_MS;
      const key = new Date(start).toISOString().slice(0, 10);
      let tok = 0;
      for (const r of rows) if (r.hour.startsWith(key)) tok += tokOf(r);
      const d = new Date(start + 3 * 3600e3); // подпись — по МСК-границе
      series.push({ label: `${String(d.getUTCDate()).padStart(2, '0')}.${String(d.getUTCMonth() + 1).padStart(2, '0')}`, tok });
    }
  } else {
    const hoursPassed = Math.min(24, Math.ceil((now - dayStart) / 3600e3));
    for (let i = 0; i < hoursPassed; i++) {
      const start = dayStart + i * 3600e3;
      const key = new Date(start).toISOString().slice(0, 13);
      let tok = 0;
      for (const r of rows) if (r.hour === key) tok += tokOf(r);
      series.push({ label: `${new Date(start).getHours()}:00`, tok });
    }
  }

  const byModel = [...sumBy(rows, (r) => r.model)].map(([k, v]) => ({ key: k, ...v }))
    .filter((m) => m.tok > 0)
    .sort((a, b) => b.tok - a.tok);
  const byProject = [...sumBy(rows, (r) => `${r.project}|${r.billing}`)]
    .map(([k, v]) => { const [project, billing] = k.split('|'); return { key: project, billing, ...v }; })
    .sort((a, b) => b.tok - a.tok).slice(0, 12);
  // разрез по биллингу: подписка и каждый API-endpoint отдельно
  const billingProjects = new Map();
  for (const r of rows) {
    const set = billingProjects.get(r.billing) || new Set();
    set.add(r.project);
    billingProjects.set(r.billing, set);
  }
  const byBilling = [...sumBy(rows, (r) => r.billing)]
    .map(([k, v]) => ({
      key: k,
      host: isApi(k) ? k.slice(4) : null,
      projects: [...(billingProjects.get(k) || [])].slice(0, 10),
      ...v,
    }))
    .sort((a, b) => b.tok - a.tok);

  const sessions = Object.entries(state.sessions)
    .filter(([, s]) => s.last >= since)
    .map(([id, s]) => ({ id, project: s.project, model: s.model, billing: s.billing, tok: tokOf(s), cost: s.cost }))
    .sort((a, b) => b.tok - a.tok).slice(0, 12);

  const w = state.window;
  const winActive = w.start && now < w.start + WINDOW_MS;

  // токены текущего ОФИЦИАЛЬНОГО окна (его старт = сброс − 5ч)
  let officialOut = null;
  if (official && official.session) {
    const winStart = official.session.resetAt ? official.session.resetAt - WINDOW_MS : 0;
    let winTok = 0;
    if (winStart) for (const r of rangeHours(winStart)) winTok += tokOf(r);
    officialOut = { ...official, account: readAccount(), windowTokens: winTok };
  }

  return {
    period: period === 'week' ? 'week' : 'today',
    total,
    series,
    byModel,
    byProject,
    byBilling,
    sessions,
    official: officialOut,
    window: winActive
      ? { tokens: w.tokens, cost: w.cost, resetInMs: w.start + WINDOW_MS - now }
      : { tokens: 0, cost: 0, resetInMs: 0 },
  };
}

function forSession(id) {
  const s = state.sessions[id];
  if (!s) return null;
  return { tok: tokOf(s), cost: s.cost, billing: s.billing, model: s.model };
}

/* ---------- официальные лимиты подписки ---------- */
/* Источник правды — headless `claude -p "/usage"`: проценты и времена сброса
 * сессии/недели. Тариф и аккаунт — из ~/.claude.json (oauthAccount). */

let official = null; // { session:{pct,resetAt}, week:{pct,resetAt}, weekSonnet:{pct}, at }
let officialBusy = false;
let resolveBin = () => null;

function readAccount() {
  try {
    const d = JSON.parse(fs.readFileSync(path.join(os.homedir(), '.claude.json'), 'utf8'));
    const oa = (d && d.oauthAccount) || {};
    const tier = String(oa.organizationRateLimitTier || '');
    let plan = null;
    const mx = tier.match(/max_(\d+)x/);
    if (mx) plan = `Max (${mx[1]}x)`;
    else if (oa.organizationType === 'claude_max') plan = 'Max';
    else if (oa.organizationType === 'claude_pro' || /pro/.test(tier)) plan = 'Pro';
    return { plan, email: oa.emailAddress || '', name: oa.displayName || '' };
  } catch {
    return { plan: null, email: '', name: '' };
  }
}

const MONTHS = { Jan: 0, Feb: 1, Mar: 2, Apr: 3, May: 4, Jun: 5, Jul: 6, Aug: 7, Sep: 8, Oct: 9, Nov: 10, Dec: 11 };

/** "Jun 11 at 9:30pm (Europe/Moscow)" → ms (МСК = UTC+3 круглый год) */
function parseResetDate(str) {
  const m = String(str).match(/([A-Z][a-z]{2})\s+(\d{1,2})\s+at\s+(\d{1,2})(?::(\d{2}))?\s*(am|pm)/i);
  if (!m || !(m[1] in MONTHS)) return 0;
  let hh = Number(m[3]) % 12;
  if (m[5].toLowerCase() === 'pm') hh += 12;
  const now = Date.now();
  let ts = Date.UTC(new Date(now).getUTCFullYear(), MONTHS[m[1]], Number(m[2]), hh - 3, Number(m[4] || 0));
  if (ts < now - 12 * 3600e3) ts = Date.UTC(new Date(now).getUTCFullYear() + 1, MONTHS[m[1]], Number(m[2]), hh - 3, Number(m[4] || 0));
  return ts;
}

function fetchOfficial() {
  if (officialBusy) return;
  const bin = resolveBin();
  if (!bin) return;
  officialBusy = true;
  execFile(bin, ['-p', '--no-session-persistence', '/usage'], {
    cwd: os.tmpdir(),
    timeout: 90 * 1000,
    maxBuffer: 1024 * 1024,
    env: { ...process.env, JARVIS_IGNORE: '1' },
  }, (err, out) => {
    officialBusy = false;
    if (err) return; // нет сети/квоты — живём на локальной оценке
    const text = String(out);
    const grab = (re) => {
      const m = text.match(re);
      return m ? { pct: Number(m[1]), resetAt: parseResetDate(m[2] || '') } : null;
    };
    const session = grab(/Current session:\s*(\d+)%\s*used\s*·\s*resets\s+([^\n(]+)/i);
    const week = grab(/Current week \(all models\):\s*(\d+)%\s*used\s*·\s*resets\s+([^\n(]+)/i);
    const ws = text.match(/Current week \(Sonnet only\):\s*(\d+)%/i);
    if (!session && !week) return; // формат уехал — не перетираем
    const prevPct = official && official.session ? official.session.pct : 0;
    official = {
      session,
      week,
      weekSonnet: ws ? { pct: Number(ws[1]) } : null,
      at: Date.now(),
    };
    // предупреждение ДО стены: пересекли 90% окна
    if (session && prevPct < 90 && session.pct >= 90 && onLimitWarning) {
      onLimitWarning({ pct: session.pct, resetAt: session.resetAt, account: readAccount() });
    }
  });
}

let onLimitWarning = null;

/* ---------- запуск ---------- */

function start(opts) {
  if (opts && typeof opts.resolveBin === 'function') resolveBin = opts.resolveBin;
  if (opts && typeof opts.onLimitWarning === 'function') onLimitWarning = opts.onLimitWarning;
  loadState();
  // backfill + инкрементальные сканы — одним и тем же путём (offsets решают)
  setTimeout(scan, state.backfilled ? 3000 : 500);
  setInterval(scan, 30000);
  setTimeout(fetchOfficial, 5000);
  setInterval(fetchOfficial, 5 * 60 * 1000); // официальные лимиты — раз в 5 мин
}

/** свежие официальные лимиты (+аккаунт) для лимит-баннера */
function officialInfo() {
  return official ? { ...official, account: readAccount() } : null;
}

module.exports = { start, scan, stats, forSession, officialInfo, refreshOfficial: fetchOfficial };
