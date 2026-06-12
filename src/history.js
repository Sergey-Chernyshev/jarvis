/**
 * История чатов по проектам: список прошлых сессий из транскриптов
 * ~/.claude/projects/**∕*.jsonl с заголовком, временем, моделью.
 *
 * 1549 файлов — полный парс дорог, поэтому лёгкое чтение (голова+хвост) с
 * кэшем по mtime: меняется только то, что изменилось на диске.
 * Служебные `-p` вызовы Jarvis (haiku-саммари, переводы) теперь идут с
 * --no-session-persistence и файлов не создают; старые — отсекаем по
 * сигнатуре первого промпта.
 */

const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');

const PROJECTS = path.join(os.homedir(), '.claude', 'projects');
const CACHE_FILE = path.join(os.homedir(), '.jarvis', 'history.json');

// первый промпт начинается с этого → наш служебный вызов, в историю не берём
const SERVICE_PREFIXES = [
  'Ответ агента:', 'Хвост диалога', 'Диалог рабочей сессии:',
  'Переведи строки', 'Суммаризируй', 'сожми этот ответ', 'Задача: выдай',
];

let cache = {}; // path → { mtime, sessionId, cwd, project, title, model, firstAt, lastAt, service }
let tokenLookup = () => null;
let persistTimer = null;
let scanning = false;

function friendlyModel(id) {
  const v = String(id || '').toLowerCase();
  if (v.includes('opus')) return 'Opus';
  if (v.includes('sonnet')) return 'Sonnet';
  if (v.includes('haiku')) return 'Haiku';
  if (v.includes('fable')) return 'Fable';
  if (v.includes('mythos')) return 'Mythos';
  return '';
}

function oneLine(s) { return String(s || '').replace(/\s+/g, ' ').trim(); }

function firstUserText(msg) {
  if (typeof msg.content === 'string') return msg.content;
  if (Array.isArray(msg.content)) {
    for (const b of msg.content) if (b && b.type === 'text') return b.text;
  }
  return '';
}

function parseMeta(file, mtime) {
  let size, fd;
  try { size = fs.statSync(file).size; fd = fs.openSync(file, 'r'); } catch { return null; }
  let head = '', tail = '';
  try {
    const hl = Math.min(size, 32 * 1024);
    const hb = Buffer.alloc(hl); fs.readSync(fd, hb, 0, hl, 0); head = hb.toString('utf8');
    const tl = Math.min(size, 32 * 1024);
    const tb = Buffer.alloc(tl); fs.readSync(fd, tb, 0, tl, size - tl); tail = tb.toString('utf8');
  } catch { try { fs.closeSync(fd); } catch {} return null; }
  try { fs.closeSync(fd); } catch {}

  const meta = {
    mtime, sessionId: path.basename(file, '.jsonl'),
    cwd: null, project: null, title: '', model: '',
    firstAt: 0, lastAt: mtime, service: false,
  };

  let firstPrompt = '';
  for (const line of head.split('\n')) {
    if (!line.trim()) continue;
    let d; try { d = JSON.parse(line); } catch { continue; }
    if (!meta.cwd && d.cwd) meta.cwd = d.cwd;
    if (!meta.firstAt && d.timestamp) meta.firstAt = Date.parse(d.timestamp) || 0;
    if (!firstPrompt && d.type === 'user' && !d.isMeta) {
      const t = oneLine(firstUserText(d.message || {}));
      if (t && !t.startsWith('<')) firstPrompt = t;
    }
    if (meta.cwd && firstPrompt) break;
  }

  // хвост: ai-title (приоритетный заголовок), последняя модель, последнее время
  let aiTitle = '';
  for (const line of tail.split('\n')) {
    if (!line.trim()) continue;
    let d; try { d = JSON.parse(line); } catch { continue; }
    if (d.timestamp) { const ts = Date.parse(d.timestamp); if (ts > meta.lastAt) meta.lastAt = ts; }
    if (d.type === 'ai-title' && d.aiTitle) aiTitle = oneLine(d.aiTitle);
    else if (d.type === 'summary' && d.summary) aiTitle = aiTitle || oneLine(d.summary);
    if (d.type === 'assistant' && d.message && d.message.model) meta.model = friendlyModel(d.message.model);
  }

  meta.service = SERVICE_PREFIXES.some((p) => firstPrompt.startsWith(p))
    || (/^\/\w+$/.test(firstPrompt)); // одиночная слэш-команда
  meta.project = meta.cwd ? path.basename(meta.cwd) : 'другое';
  meta.title = (aiTitle || firstPrompt || '').slice(0, 100);
  if (!meta.firstAt) meta.firstAt = mtime;
  return meta;
}

function loadCache() {
  try {
    const c = JSON.parse(fs.readFileSync(CACHE_FILE, 'utf8'));
    if (c && typeof c === 'object') cache = c;
  } catch {}
}

function persist() {
  if (persistTimer) return;
  persistTimer = setTimeout(() => {
    persistTimer = null;
    try { fs.writeFileSync(CACHE_FILE, JSON.stringify(cache)); } catch {}
  }, 3000);
}

function listFiles() {
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
    const seen = new Set();
    for (const file of listFiles()) {
      seen.add(file);
      let st;
      try { st = fs.statSync(file); } catch { continue; }
      if (st.size < 200) continue; // пустые/обрывки
      const hit = cache[file];
      if (hit && hit.mtime === Math.floor(st.mtimeMs)) continue; // не менялся
      const meta = parseMeta(file, Math.floor(st.mtimeMs));
      if (meta) cache[file] = meta;
    }
    for (const p of Object.keys(cache)) if (!seen.has(p)) delete cache[p]; // удалённые
    persist();
  } finally {
    scanning = false;
  }
}

/** [{project, cwd, count, lastAt, sessions:[{id,title,model,tokens,cost,lastAt}]}] */
function projects() {
  const byProject = new Map();
  for (const meta of Object.values(cache)) {
    if (meta.service || !meta.title) continue;
    const key = meta.cwd || meta.project;
    let g = byProject.get(key);
    if (!g) { g = { project: meta.project, cwd: meta.cwd, lastAt: 0, sessions: [] }; byProject.set(key, g); }
    const usage = tokenLookup(meta.sessionId) || {};
    g.sessions.push({
      id: meta.sessionId,
      title: meta.title,
      model: meta.model || usage.model || '',
      tokens: usage.tok || 0,
      cost: usage.cost || 0,
      billing: usage.billing || 'plan',
      lastAt: meta.lastAt,
    });
    if (meta.lastAt > g.lastAt) g.lastAt = meta.lastAt;
  }
  const out = [...byProject.values()];
  for (const g of out) {
    g.sessions.sort((a, b) => b.lastAt - a.lastAt);
    g.count = g.sessions.length;
    g.sessions = g.sessions.slice(0, 40); // на проект — последние 40
  }
  out.sort((a, b) => b.lastAt - a.lastAt);
  return out;
}

function start(opts) {
  if (opts && typeof opts.tokenLookup === 'function') tokenLookup = opts.tokenLookup;
  loadCache();
  setTimeout(scan, 1200);
  setInterval(scan, 60000);
}

module.exports = { start, scan, projects };
