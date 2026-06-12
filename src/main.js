/**
 * Jarvis MVP — демон + меню-бар + панель.
 *
 * Main-процесс Electron и есть демон: слушает unix-сокет ~/.jarvis/run.sock,
 * на который jarvis-hook кидает события из хуков Claude Code.
 *
 * Состояния сессии: idle → working → (waiting ⇄ working) → done → working → ...
 *   session-start → idle        (сессия открыта, ничего не делает)
 *   prompt        → working     (юзер отправил промпт)
 *   notification  → waiting     (нужен пермишен / ждёт ввода) → уведомление
 *   stop          → done        (закончил ответ)              → уведомление
 *   session-end   → сессия удаляется
 */

const {
  app, BrowserWindow, Tray, Menu, Notification,
  nativeImage, screen, ipcMain, globalShortcut, nativeTheme,
} = require('electron');
const http = require('node:http');
const { execFile } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const settings = require('./settings');
const commands = require('./commands');
const usage = require('./usage');
const history = require('./history');
const pluginHost = require('./plugins');

const JARVIS_DIR = path.join(os.homedir(), '.jarvis');
const SOCK = process.env.JARVIS_SOCK || path.join(JARVIS_DIR, 'run.sock');

const PANEL_W = 820;
const PANEL_H = 620;

/** @type {Map<string, object>} session_id → session */
const sessions = new Map();
let panel = null;
let tray = null;
let server = null;
let panelFocusMode = false; // показана ли панель «фокусно» (хоткей)

/* ================= state ================= */

const ORDER = { waiting: 0, limit: 1, working: 2, done: 3, idle: 4 };

function snapshot() {
  return [...sessions.values()].sort(
    (a, b) => (ORDER[a.status] - ORDER[b.status]) || (b.updatedAt - a.updatedAt),
  );
}

function shortHome(p) {
  return typeof p === 'string' ? p.replace(os.homedir(), '~') : p;
}

function oneLine(s) {
  return String(s || '').replace(/\s+/g, ' ').trim();
}

function reduce(evt) {
  if (!evt || typeof evt !== 'object') return;
  const p = evt.payload && typeof evt.payload === 'object' ? evt.payload : {};
  const id = p.session_id || 'unknown';
  const now = Date.now();

  let s = sessions.get(id);
  if (!s) {
    s = { id, status: 'idle', detail: '', createdAt: now };
    sessions.set(id, s);
  }
  if (p.cwd) {
    s.cwd = p.cwd;
    s.project = path.basename(p.cwd);
  }
  s.project ||= '?';
  if (evt.agent) s.agent = evt.agent;
  if (evt.tmux_pane && evt.tmux_pane !== s.tmuxPane) {
    s.tmuxPane = evt.tmux_pane;
    // человекочитаемое имя tmux-сессии — для бейджа в панели
    tmuxJ(['display-message', '-p', '-t', s.tmuxPane, '#{session_name}'])
      .then((out) => { s.tmuxName = oneLine(out); push(); })
      .catch(() => {});
  }
  if (evt.host) s.host = evt.host; // TERM_PROGRAM / TERMINAL_EMULATOR терминала
  if (evt.tty) s.tty = evt.tty;
  if (p.transcript_path) s.transcript = p.transcript_path;
  s.updatedAt = now;

  // pid процесса claude (= $PPID хука) → один раз резолвим GUI-приложение
  if (evt.pid && evt.pid !== s.pid) {
    s.pid = evt.pid;
    if (!s.app) {
      guiAncestorApp(s.pid).then((app) => {
        if (app && sessions.get(id) === s) { s.app = app.name; push(); }
      }).catch(() => {});
    }
  }

  switch (evt.event) {
    case 'session-start':
      s.status = 'idle';
      s.detail = '';
      refreshMeta(s);
      break;

    case 'prompt': {
      s.status = 'working';
      const txt = oneLine(p.prompt).slice(0, 140);
      // системные инъекции (<task-notification>, <command-…>) — не промпт юзера
      if (txt && !txt.startsWith('<')) {
        s.detail = txt;
        s.lastPrompt = txt; // «последняя задача» от юзера — живёт дольше detail
      }
      s.limitWait = false; // юзер сам продолжил — авто-резюме этой сессии не нужно
      refreshMeta(s);
      refreshTasks(s);
      break;
    }

    // живая лента: что агент делает прямо сейчас
    case 'pre-tool': {
      if (p.tool_name === 'AskUserQuestion') {
        // это опрос, не пермишен: показываем вопрос карточкой и ждём выбора
        const qs = Array.isArray(p.tool_input?.questions) ? p.tool_input.questions : [];
        if (qs.length) {
          s.status = 'waiting';
          s.question = {
            at: now,
            questions: qs.slice(0, 4).map((q) => ({
              question: oneLine(q?.question).slice(0, 300),
              header: oneLine(q?.header || '').slice(0, 40),
              multiSelect: !!q?.multiSelect,
              options: (Array.isArray(q?.options) ? q.options : []).slice(0, 9).map((o) => ({
                label: oneLine(o?.label).slice(0, 80),
                description: oneLine(o?.description || '').slice(0, 140),
              })),
            })),
          };
          s.detail = oneLine(qs[0]?.question).slice(0, 140) || 'Опрос';
          notify(`${s.project} — спрашивает`, s.detail, id, 'waiting');
        }
        break;
      }
      // таск-тулы: обновляем «текущую задачу», в живую ленту их не пишем
      if (/^(TaskCreate|TaskUpdate|TaskGet|TaskList|TodoWrite)$/.test(p.tool_name || '')) {
        s.status = 'working';
        if (p.tool_name === 'TodoWrite') parseTodos(s, p.tool_input);
        refreshTasks(s);
        break;
      }
      s.status = 'working';
      trackActivity(s, p.tool_name, p.tool_input);
      if (!s.branch && !s.title) refreshMeta(s); // сессия ожила после рестарта демона
      break;
    }

    case 'post-tool':
      if (p.tool_name === 'AskUserQuestion' && s.question) {
        s.question = null; // ответили (в терминале или из панели) — карточка закрывается
        s.detail = 'ответ получен';
      }
      s.status = 'working'; // обновляем updatedAt — сессия дышит
      break;

    case 'notification': {
      // AskUserQuestion дублируется PermissionRequest-уведомлением — вопрос
      // уже показан карточкой, «Claude needs your permission» его не перетирает
      if (s.question) break;
      const msg = ruNotification(oneLine(p.message)) || 'Claude ждёт ввода';
      // Claude Code повторяет idle-уведомления — не спамим одним и тем же
      const isNew = !(s.status === 'waiting' && s.detail === msg);
      s.status = 'waiting';
      s.detail = msg;
      if (isNew && settings.load().notifyWaiting) notify(`${s.project} — нужен ты`, msg, id, 'waiting');
      break;
    }

    case 'stop':
      s.status = 'done';
      s.doneAt = now; // момент последнего завершённого ответа — по нему сортируется список
      s.detail = 'Ответ готов';
      // тост сразу с черновым текстом, затем ИИ-выжимка полного ответа (haiku)
      if (settings.load().notifyDone) {
        const reply = lastAssistantReply(s);
        const tid = notify(`${s.project} — закончил`, reply || s.task || s.summary || s.title || 'Ответ готов', id);
        aiToastSummary(s, tid);
      }
      refreshMeta(s);
      refreshTasks(s);
      genSummary(s);
      break;

    case 'stop-failure':
      onStopFailure(s, p);
      break;

    case 'session-end':
      sessions.delete(id);
      push();
      return;

    default:
      break;
  }
  push();
}

// Троттлим: tool-события сыплются часто, рендерить чаще ~8 раз/с незачем
let pushTimer = null;
function push() {
  if (pushTimer) return;
  pushTimer = setTimeout(() => {
    pushTimer = null;
    const list = snapshot();
    pluginHost.onSessions(list); // плагины первыми — бейджи к updateTray уже свежие
    if (panel && !panel.isDestroyed()) {
      panel.webContents.send('state', list);
      panel.webContents.send('plugins', pluginHost.statuses());
    }
    updateTray(list);
    persistState();
  }, 120);
}

/* ================= персистентность реестра ================= */
/* Реестр в памяти — перезапуск демона не должен «ронять» сессии. */

const STATE_FILE = path.join(JARVIS_DIR, 'state.json');
let persistTimer = null;

function writeStateNow() {
  try {
    fs.mkdirSync(JARVIS_DIR, { recursive: true });
    const arr = [...sessions.values()].map(({ resumeBusy, metaBusy, tasksBusy, summaryBusy, ...rest }) => rest);
    fs.writeFileSync(STATE_FILE, JSON.stringify(arr) + '\n');
  } catch {}
}

function persistState() {
  if (persistTimer) return;
  persistTimer = setTimeout(() => { persistTimer = null; writeStateNow(); }, 500);
}

function restoreState() {
  try {
    const arr = JSON.parse(fs.readFileSync(STATE_FILE, 'utf8'));
    if (!Array.isArray(arr)) return;
    const cutoff = Date.now() - 24 * 3600 * 1000; // суточный мусор не тащим
    for (const s of arr) {
      if (s && typeof s === 'object' && s.id && (s.updatedAt || 0) > cutoff) {
        if (s.title) s.title = ru(s.title); // англ. заголовки доезжают переводом
        if (s.task) s.task = ru(s.task);
        sessions.set(s.id, s);
      }
    }
  } catch {}
}

/* ================= чистка умерших сессий ================= */
/* Жёстко убитый терминал не шлёт SessionEnd. Раз в 30с сверяем tmux-паны;
 * working-сессии без событий 15 минут считаем потерянными. */

function sweepSessions() {
  execFile('tmux', ['-L', 'jarvis', 'list-panes', '-a', '-F', '#{pane_id}'], { timeout: 4000 }, (err, out) => {
    // ENOENT — tmux не установлен, паны не проверяем; иначе ошибка = сервер пуст
    const alive = err
      ? (err.code === 'ENOENT' ? null : new Set())
      : new Set(String(out).trim().split('\n').filter(Boolean));
    let changed = false;
    const now = Date.now();
    for (const [id, s] of sessions) {
      if (s.tmuxPane && alive && !alive.has(s.tmuxPane)) {
        sessions.delete(id); // пана мертва — claude умер вместе с ней
        changed = true;
      } else if (s.status === 'working' && !s.resumeBusy && now - s.updatedAt > 15 * 60 * 1000) {
        s.status = 'idle';
        s.detail = 'связь потеряна — событий нет 15 минут';
        changed = true;
      }
    }
    if (changed) push();
  });
}

/* ================= чтение транскриптов ================= */
/* Claude Code пишет JSONL-лог сессии (transcript_path из хуков). Формат
 * внутренний и дрейфует — парсим defensive: неизвестное поле → дефолт,
 * битая строка → скип, никогда не падаем. */

let currentTail = null; // панель смотрит один чат за раз

function shortToolLabel(name, input) {
  let detail = '';
  if (input && typeof input === 'object') {
    if (typeof input.command === 'string') detail = input.command;
    else if (typeof input.file_path === 'string') detail = path.basename(input.file_path);
    else if (typeof input.pattern === 'string') detail = input.pattern;
    else if (typeof input.url === 'string') detail = input.url;
    else if (typeof input.description === 'string') detail = input.description;
  }
  detail = oneLine(detail).slice(0, 64);
  // mcp__plugin_playwright_playwright__browser_click → browser_click
  const short = String(name || 'tool').replace(/^mcp__.+__/, '');
  return detail ? `${short} · ${detail}` : short;
}

/** одна строка JSONL → 0..n элементов чата (юзер-текст, ассистент-текст, тул-чипы) */
function toChatItems(entry) {
  if (!entry || typeof entry !== 'object' || entry.isSidechain || entry.isMeta) return [];
  const msg = entry.message;
  if (!msg || typeof msg !== 'object') return [];
  const ts = Date.parse(entry.timestamp) || Date.now();
  const items = [];
  const pushText = (role, text) => {
    const t = String(text || '').trim();
    // служебные вставки (<system-reminder>, <command-name>…) в чат не показываем
    if (t && !t.startsWith('<')) items.push({ role, kind: 'text', text: t.slice(0, 4000), ts });
  };
  if (entry.type === 'user') {
    if (typeof msg.content === 'string') pushText('user', msg.content);
    else if (Array.isArray(msg.content)) {
      for (const b of msg.content) if (b && b.type === 'text') pushText('user', b.text);
    }
  } else if (entry.type === 'assistant' && Array.isArray(msg.content)) {
    for (const b of msg.content) {
      if (!b || typeof b !== 'object') continue;
      if (b.type === 'text') pushText('assistant', b.text);
      else if (b.type === 'tool_use') items.push({ role: 'assistant', kind: 'tool', text: shortToolLabel(b.name, b.input), ts });
    }
  }
  return items;
}

/** хвост файла → массив распарсенных строк (старое не тянем, файлы бывают на мегабайты) */
function readRecentEntries(file, maxBytes = 512 * 1024) {
  try {
    const size = fs.statSync(file).size;
    const start = Math.max(0, size - maxBytes);
    const fd = fs.openSync(file, 'r');
    const buf = Buffer.alloc(size - start);
    fs.readSync(fd, buf, 0, buf.length, start);
    fs.closeSync(fd);
    let text = buf.toString('utf8');
    if (start > 0) text = text.slice(text.indexOf('\n') + 1); // первая строка могла обрезаться
    const entries = [];
    for (const line of text.split('\n')) {
      if (!line.trim()) continue;
      try { entries.push(JSON.parse(line)); } catch {}
    }
    return entries;
  } catch {
    return [];
  }
}

/** лог — дерево (resume/форки): идём от последней записи вверх по parentUuid */
function chainFromEntries(entries) {
  const byUuid = new Map();
  for (const e of entries) if (e && e.uuid) byUuid.set(e.uuid, e);
  let last = null;
  for (let i = entries.length - 1; i >= 0; i--) {
    const e = entries[i];
    if (e && e.uuid && (e.type === 'user' || e.type === 'assistant')) { last = e; break; }
  }
  if (!last) return [];
  const chain = [];
  const seen = new Set();
  let cur = last;
  while (cur && cur.uuid && !seen.has(cur.uuid)) {
    seen.add(cur.uuid);
    chain.push(cur);
    cur = cur.parentUuid ? byUuid.get(cur.parentUuid) : null;
  }
  return chain.reverse();
}

function stopTail() {
  if (currentTail) { currentTail.stop(); currentTail = null; }
}

/** incremental tail: offset + fs.watch, дочитываем только новые байты.
 * Файла может ещё не быть (свежая сессия до первого промпта) — ждём появления. */
function startTail(sessionId, file) {
  stopTail();
  let offset = 0;
  try { offset = fs.statSync(file).size; } catch {} // нет файла — начнём с нуля
  let rest = '';
  let busy = false;
  let watcher = null;

  const read = () => {
    if (busy) return;
    busy = true;
    fs.stat(file, (err, st) => {
      if (err) { busy = false; return; } // файла ещё нет — придём поллом
      if (!watcher) {
        try { watcher = fs.watch(file, read); } catch {}
      }
      if (st.size < offset) offset = 0; // файл переписали с нуля — начинаем заново
      if (st.size === offset) { busy = false; return; }
      const stream = fs.createReadStream(file, { start: offset, end: st.size - 1, encoding: 'utf8' });
      let chunk = '';
      stream.on('data', (c) => { chunk += c; });
      stream.on('error', () => { busy = false; });
      stream.on('end', () => {
        offset = st.size;
        busy = false;
        const lines = (rest + chunk).split('\n');
        rest = lines.pop(); // неполная строка ждёт следующего чтения
        const items = [];
        for (const line of lines) {
          if (!line.trim()) continue;
          try { items.push(...toChatItems(JSON.parse(line))); } catch {}
        }
        if (items.length && panel && !panel.isDestroyed()) {
          panel.webContents.send('chat:append', { sessionId, items });
        }
      });
    });
  };

  read();
  const poll = setInterval(read, 2000); // fs.watch на macOS иногда молчит — страховка
  currentTail = { stop: () => { try { watcher?.close(); } catch {} clearInterval(poll); } };
}

/* ================= идентичность сессий ================= */
/* Несколько агентов в одном проекте должны различаться с одного взгляда:
 * ветка из транскрипта, живая активность из tool-событий, смысловой
 * заголовок (готовый summary из транскрипта или Haiku по тумблеру),
 * и обратный канал — tmux-окно подписывает сам терминал. */

/** слой 2: последняя команда + тронутые директории из tool-событий */
function trackActivity(s, name, input) {
  const label = shortToolLabel(name, input);
  if (input && typeof input === 'object') {
    if (typeof input.command === 'string') {
      s.lastCmd = oneLine(input.command).slice(0, 48);
    } else if (typeof input.file_path === 'string') {
      let rel = input.file_path;
      if (s.cwd && rel.startsWith(s.cwd)) rel = rel.slice(s.cwd.length + 1);
      const dir = path.dirname(rel);
      const spot = dir === '.' ? path.basename(rel) : `${dir}/`;
      s.touched = (s.touched || []).filter((d) => d !== spot);
      s.touched.push(spot);
      while (s.touched.length > 3) s.touched.shift();
    }
  }
  const touched = s.touched && s.touched.length ? ` · трогает ${s.touched.join(' ')}` : '';
  if (typeof input?.command === 'string') {
    s.detail = `▸ ${s.lastCmd}${touched}`.slice(0, 140);
  } else if (label && label !== 'tool') {
    s.detail = `▸ ${label}`.slice(0, 140);
  }
}

/* ================= русификация ================= */
/* Системные сообщения Claude Code — статикой; заголовки задач/сессий,
 * пришедшие не по-русски, — ленивым haiku-переводом с кэшем на диске
 * (каждая уникальная строка стоит один вызов за всю жизнь кэша). */

function ruNotification(msg) {
  const m = String(msg || '');
  if (/waiting for your input/i.test(m)) return 'Ждёт твоего ввода';
  const perm = m.match(/needs your permission to use\s+(.+)/i);
  if (perm) return `Нужен пермишен: ${oneLine(perm[1])}`;
  if (/needs your permission/i.test(m)) return 'Нужен пермишен';
  return m;
}

const TR_FILE = path.join(JARVIS_DIR, 'translations.json');
let translations = {};
try {
  const parsed = JSON.parse(fs.readFileSync(TR_FILE, 'utf8'));
  if (parsed && typeof parsed === 'object') translations = parsed;
} catch {}
const trQueue = new Set();
let trBusy = false;

const hasCyrillic = (s) => /[а-яё]/i.test(s);

/** вернуть русскую версию строки; не-русское — в очередь на перевод */
function ru(text) {
  const t = oneLine(text);
  if (!t || hasCyrillic(t)) return t;
  if (translations[t]) return translations[t];
  trQueue.add(t);
  setTimeout(pumpTranslations, 300);
  return t; // покажем оригинал, перевод догонит
}

function pumpTranslations() {
  if (trBusy || !trQueue.size) return;
  const bin = resolveClaudeBin();
  if (!bin) return;
  const batch = [...trQueue].slice(0, 6);
  for (const t of batch) trQueue.delete(t);
  trBusy = true;
  const numbered = batch.map((t, i) => `${i + 1}. ${t}`).join('\n');
  execFile(bin, [
    '-p', '--no-session-persistence', '--model', 'haiku',
    `Переведи строки на русский. Это заголовки задач разработки: технические термины, имена файлов и команд не переводи. Ответь только пронумерованными переводами, по одному на строку.\n\n${numbered}`,
  ], {
    cwd: os.tmpdir(),
    timeout: 60 * 1000,
    maxBuffer: 1024 * 1024,
    env: { ...process.env, JARVIS_IGNORE: '1' },
  }, (err, out) => {
    trBusy = false;
    if (!err) {
      const lines = String(out).split('\n')
        .map((l) => l.replace(/^\s*\d+[.)]\s*/, '').trim())
        .filter(Boolean);
      batch.forEach((src, i) => {
        if (lines[i] && hasCyrillic(lines[i])) translations[src] = oneLine(lines[i]).slice(0, 120);
      });
      try { fs.writeFileSync(TR_FILE, JSON.stringify(translations, null, 1)); } catch {}
      applyTranslations();
    }
    if (trQueue.size) setTimeout(pumpTranslations, 800);
  });
}

/** долить готовые переводы в реестр (s.title/s.task хранят оригинал до перевода) */
function applyTranslations() {
  let changed = false;
  for (const s of sessions.values()) {
    for (const f of ['title', 'task']) {
      if (s[f] && translations[s[f]]) { s[f] = translations[s[f]]; changed = true; }
    }
  }
  if (changed) push();
}

/** «чем занята сейчас»: задачи сессии из ~/.claude/tasks/<id>/N.json
 * (их ведёт сам Claude Code через TaskCreate/TaskUpdate) */
function refreshTasks(s) {
  if (s.tasksBusy) return;
  s.tasksBusy = true;
  setTimeout(() => {
    s.tasksBusy = false;
    const dir = path.join(os.homedir(), '.claude', 'tasks', s.id);
    let tasks = [];
    try {
      tasks = fs.readdirSync(dir)
        .filter((f) => f.endsWith('.json'))
        .map((f) => {
          try { return JSON.parse(fs.readFileSync(path.join(dir, f), 'utf8')); } catch { return null; }
        })
        .filter((t) => t && typeof t.subject === 'string');
    } catch { return; } // каталога нет — сессия тасками не пользуется
    if (!tasks.length) return;
    tasks.sort((a, b) => Number(a.id) - Number(b.id));
    applyTasks(s, tasks.map((t) => ({ text: t.subject, status: t.status })));
  }, 800);
}

/** fallback для агентов на старом TodoWrite: todos прямо в payload хука
 * (бывает и массивом, и JSON-строкой — парсим defensively) */
function parseTodos(s, input) {
  let todos = input && input.todos;
  if (typeof todos === 'string') {
    try { todos = JSON.parse(todos); } catch { return; }
  }
  if (!Array.isArray(todos) || !todos.length) return;
  applyTasks(s, todos
    .filter((t) => t && typeof t === 'object')
    .map((t) => ({ text: t.activeForm || t.content || '', status: t.status })));
}

function applyTasks(s, items) {
  const total = items.length;
  if (!total) return;
  const done = items.filter((t) => t.status === 'completed').length;
  const cur = items.find((t) => t.status === 'in_progress');
  const task = cur ? ru(oneLine(cur.text).slice(0, 100)) : null;
  const progress = `${done}/${total}`;
  s.todoList = items.slice(0, 12).map((t) =>
    `${t.status === 'completed' ? '✓' : t.status === 'in_progress' ? '▸' : '○'} ${oneLine(t.text).slice(0, 70)}`);
  if (s.task !== task || s.taskProgress !== progress) {
    s.task = task;
    s.taskProgress = progress;
    push();
  }
}

/** слой 1+3: ветка и готовый summary из хвоста транскрипта (дебаунс per-сессия) */
function refreshMeta(s) {
  if (!s.transcript || s.metaBusy) return;
  s.metaBusy = true;
  setTimeout(() => {
    s.metaBusy = false;
    const entries = readRecentEntries(s.transcript, 64 * 1024);
    let changed = false;
    for (let i = entries.length - 1; i >= 0; i--) {
      const e = entries[i];
      // ветка лежит в метаданных каждой записи
      if (e && typeof e.gitBranch === 'string' && e.gitBranch && e.gitBranch !== 'HEAD') {
        if (s.branch !== e.gitBranch) { s.branch = e.gitBranch; changed = true; }
        break;
      }
    }
    for (let i = entries.length - 1; i >= 0; i--) {
      const e = entries[i];
      // Claude Code сам пишет заголовок сессии — type:ai-title / summary
      const t = e && (e.type === 'ai-title' ? e.aiTitle : e.type === 'summary' ? e.summary : null);
      if (typeof t === 'string' && t.trim()) {
        const title = ru(oneLine(t).slice(0, 60));
        if (s.title !== title) { s.title = title; changed = true; maybeRenameWindow(s); }
        break;
      }
    }
    // модель — бесплатно из транскрипта; не перетираем свежий ручной выбор
    // (modelAt) — транскрипт догонит позже
    if (!s.modelAt || Date.now() - s.modelAt > 30000) {
      let m = null;
      for (let i = entries.length - 1; i >= 0; i--) {
        const e = entries[i];
        if (e && e.type === 'assistant' && e.message && e.message.model) { m = friendlyModel(e.message.model); break; }
      }
      if (!m) m = readModelFromProject(s.cwd); // transcript_path форкнут — ищем в каталоге
      if (m && s.model !== m) { s.model = m; changed = true; }
    }
    if (changed) push();
  }, 1500);
}

/* уровни effort берём из `claude --help`, чтобы не отставать от релизов CLI */
let effortLevels = ['low', 'medium', 'high', 'xhigh', 'max']; // fallback
function detectEffortLevels() {
  const bin = resolveClaudeBin();
  if (!bin) return;
  execFile(bin, ['--help'], {
    timeout: 20000,
    maxBuffer: 1024 * 1024,
    env: { ...process.env, JARVIS_IGNORE: '1' },
  }, (err, out) => {
    if (err) return;
    const m = String(out).match(/--effort <level>[^(]*\(([^)]+)\)/s);
    if (!m) return;
    const levels = m[1].split(',').map((x) => x.trim()).filter((x) => /^[a-z]+$/.test(x));
    if (levels.length >= 3) {
      effortLevels = levels;
      console.log(`[jarvis] effort-уровни из CLI: ${levels.join(', ')}`);
    }
  });
}

/** саммаризация последних задач сессии (haiku) — живой контекст строки списка.
 * Обновляется на stop с кулдауном 2 мин; уникальный хвост → один вызов. */
function genSummary(s) {
  if (s.summaryBusy) return;
  if (s.summaryAt && Date.now() - s.summaryAt < 2 * 60 * 1000) return;
  if (!s.transcript) return;
  const bin = resolveClaudeBin();
  if (!bin) return;
  const turns = chainFromEntries(readRecentEntries(s.transcript))
    .flatMap(toChatItems)
    .filter((i) => i.kind === 'text')
    .slice(-12);
  if (turns.length < 2) return;
  const convo = turns
    .map((i) => `${i.role === 'user' ? 'Юзер' : 'Агент'}: ${i.text.slice(0, 240)}`)
    .join('\n');

  s.summaryBusy = true;
  execFile(bin, [
    '-p', '--no-session-persistence', '--model', 'haiku',
    `Хвост диалога рабочей сессии:\n${convo}\n\nЗадача: одной строкой по-русски (до 90 символов) суммаризируй последние задачи — что просил юзер и что сделал агент. Без кавычек, без вступлений, технические термины не переводи.`,
  ], {
    cwd: os.tmpdir(),
    timeout: 90 * 1000,
    maxBuffer: 1024 * 1024,
    env: { ...process.env, JARVIS_IGNORE: '1' },
  }, (err, stdout) => {
    s.summaryBusy = false;
    if (err) return; // без квоты/сети живём на lastPrompt/title
    const t = oneLine(stdout).replace(/^["«]|["»]$/g, '').slice(0, 110);
    if (!t || sessions.get(s.id) !== s) return;
    s.summary = t;
    s.summaryAt = Date.now();
    push();
  });
}

/** обратный канал: терминал подписывает сам себя (таб iTerm2 / заголовок окна) */
function maybeRenameWindow(s) {
  if (!s.tmuxPane || !s.title) return;
  const name = s.title.slice(0, 24);
  if (s.renamedTo === name) return;
  tmuxJ(['rename-window', '-t', s.tmuxPane, name])
    .then(() => { s.renamedTo = name; })
    .catch(() => {});
}

/* ================= ответ в сессию ================= */
/* Канал 1: tmux-вставка в пану (-L jarvis, наш отдельный сервер).
 * Канал 2: headless `claude -p --resume <id>` — для сессий вне tmux.
 * Текст всегда уходит элементом argv, никакой интерполяции в shell-строку. */

function tmuxJ(args) {
  return new Promise((resolve, reject) => {
    execFile('tmux', ['-L', 'jarvis', ...args], { timeout: 5000 }, (err, out) => {
      if (err) reject(err);
      else resolve(String(out));
    });
  });
}

async function tmuxPaneAlive(pane) {
  try {
    await tmuxJ(['display-message', '-p', '-t', pane, 'ok']);
    return true;
  } catch {
    return false;
  }
}

function capturePane(pane) {
  return tmuxJ(['capture-pane', '-t', pane, '-p']).then((s) => String(s)).catch(() => null);
}

/* ===== детектор интерактивных промптов на экране =====
 * Подтверждения slash-команд («Switch model?») и прочие пикеры выполняет сам
 * клиент Claude Code — они НЕ tool-call'ы, в хуки и транскрипт не попадают.
 * Единственный источник — экран паны. Парсим его, переиспользуя question-UI. */

const SCREEN_PROMPT = /Enter to select|↑\/↓ to navigate|to confirm|\(y\/n\)|Do you want|Switch model\?/i;
const SCREEN_IDLE = /bypass permissions on|for agents|esc to interrupt/i;

async function detectStuckPrompt(s) {
  if (!s.tmuxPane) return;
  if (s.question && !s.question.fromScreen) return; // вопрос уже показан хуком
  // во время активной генерации на экран не лезем; idle/done/waiting — смотрим
  // (подтверждение появляется как раз когда сессия НЕ генерит)
  if (s.status === 'working' && Date.now() - s.updatedAt < 8000) return;
  const screen = await capturePane(s.tmuxPane);
  if (screen == null) return;
  const tail = screen.split('\n').slice(-18);
  const text = tail.join('\n');

  const interactive = SCREEN_PROMPT.test(text) && /^\s*[❯>]?\s*1[.)]\s+\S/m.test(text);
  if (!interactive) {
    if (s.question && s.question.fromScreen && SCREEN_IDLE.test(text)) {
      s.question = null; // подтверждение ушло — снимаем экранный вопрос
      s.status = 'idle';
      s.updatedAt = Date.now();
      push();
    }
    return;
  }

  const opts = [];
  let multi = false;
  for (const raw of tail) {
    const m = raw.match(/^\s*[❯>]?\s*\d+[.)]\s+(.+?)\s*$/);
    if (!m) continue;
    let label = m[1].trim();
    if (/\[[ xX✔]\]/.test(label)) { multi = true; label = label.replace(/\[[ xX✔]\]\s*/, '').trim(); }
    if (!/^(Type something|Chat about this)\.?$/i.test(label)) opts.push(label);
  }
  if (!opts.length) return;

  // блок вопроса = строки между рамкой-разделителем и первой опцией
  const firstIdx = tail.findIndex((l) => /^\s*[❯>]?\s*1[.)]\s+/.test(l));
  let startIdx = 0;
  for (let i = firstIdx - 1; i >= 0; i--) {
    if (/^\s*[─━_]{3,}\s*$/.test(tail[i])) { startIdx = i + 1; break; }
  }
  const cands = [];
  for (let i = startIdx; i < firstIdx; i++) {
    const l = tail[i].trim();
    if (!l || /^[─━_]+$/.test(l) || /^[☐☒✔]/.test(l)) continue;
    cands.push(l);
  }
  // заголовок: строка-вопрос (с «?») либо самая верхняя строка блока
  const title = oneLine(cands.find((l) => l.endsWith('?')) || cands[0] || '')
    .slice(0, 200) || 'Подтверждение в терминале';

  const prev = s.question && s.question.fromScreen && s.question.questions[0];
  if (prev && prev.question === title && prev.options.length === opts.length) return; // не дёргаем

  s.status = 'waiting';
  s.question = {
    fromScreen: true,
    at: Date.now(),
    questions: [{
      question: title,
      header: '',
      multiSelect: multi,
      options: opts.slice(0, 9).map((label) => ({ label: oneLine(label).slice(0, 80), description: '' })),
    }],
  };
  s.detail = title.slice(0, 140);
  if (settings.load().notifyWaiting) notify(`${s.project} — спрашивает`, s.detail, s.id, 'waiting');
  push();
  console.log(`[jarvis] экранный промпт у ${s.project}: ${title}`);
}

function resolveClaudeBin() {
  const dirs = String(process.env.PATH || '').split(':').filter(Boolean);
  for (const extra of [path.join(os.homedir(), '.local', 'bin'), '/opt/homebrew/bin', '/usr/local/bin']) {
    if (!dirs.includes(extra)) dirs.push(extra);
  }
  const shims = path.join(JARVIS_DIR, 'shims');
  for (const d of dirs) {
    if (d === shims) continue; // настоящий бинарь, не наш шим
    const p = path.join(d, 'claude');
    try {
      fs.accessSync(p, fs.constants.X_OK);
      return p;
    } catch {}
  }
  return null;
}

function markPromptSent(s, prompt) {
  s.status = 'working';
  s.detail = oneLine(prompt).slice(0, 140);
  s.updatedAt = Date.now();
  push();
}

async function replyViaTmux(s, prompt) {
  // C-u срезает недописанный черновик в строке ввода — иначе вставка
  // доклеится к нему и Enter отправит склейку
  await tmuxJ(['send-keys', '-t', s.tmuxPane, 'C-u']);
  // set-buffer → paste-buffer (bracketed, ради многострочных) → отдельный Enter
  await tmuxJ(['set-buffer', '-b', 'jarvis-reply', '--', prompt]);
  await tmuxJ(['paste-buffer', '-p', '-d', '-b', 'jarvis-reply', '-t', s.tmuxPane]);
  await tmuxJ(['send-keys', '-t', s.tmuxPane, 'Enter']);
}

// вне tmux мы не вставляем текст — сессией нельзя управлять, пока она не в tmux.
// Подсказываем команду: shim завернёт этот `claude --resume` в наш tmux-сервер.
function tmuxNeededResult(sessionId) {
  return { ok: false, needsTmux: true, resumeCmd: `claude --resume ${sessionId}` };
}

/* пульт: модель и effort — слэш-команды с аргументом, формы без слайдера
 * (проверено на живом пикере: /model sonnet, /effort high исполняются сразу) */
async function pasteSlash(s, text) {
  await tmuxJ(['send-keys', '-t', s.tmuxPane, 'C-u']); // не клеимся к черновику
  await tmuxJ(['set-buffer', '-b', 'jarvis-cmd', '--', text]);
  await tmuxJ(['paste-buffer', '-p', '-d', '-b', 'jarvis-cmd', '-t', s.tmuxPane]);
  await tmuxJ(['send-keys', '-t', s.tmuxPane, 'Enter']);
  // на длинной сессии /model показывает «Switch model?» — подтверждаем
  // выделенный по умолчанию вариант (Yes) ещё одним Enter, если он есть
  await sleep(700);
  const screen = await capturePane(s.tmuxPane);
  if (screen && /Switch model\?|Enter to select|to confirm/i.test(screen.split('\n').slice(-12).join('\n'))) {
    await tmuxJ(['send-keys', '-t', s.tmuxPane, 'Enter']);
  }
}

function friendlyModel(id) {
  const v = String(id || '').toLowerCase();
  if (v.includes('opus')) return 'Opus';
  if (v.includes('sonnet')) return 'Sonnet';
  if (v.includes('haiku')) return 'Haiku';
  if (v.includes('fable')) return 'Fable';
  if (v.includes('mythos')) return 'Mythos';
  return id ? String(id).split('-')[0] : '';
}

// Claude Code кодирует cwd в имя каталога проекта, заменяя / и . на -
function projectDirFor(cwd) {
  if (!cwd) return null;
  return path.join(os.homedir(), '.claude', 'projects', cwd.replace(/[/.]/g, '-'));
}

// transcript_path из хука бывает форкнут (диалог уезжает в новый файл) —
// читаем модель из самого свежего транскрипта в каталоге проекта
function readModelFromProject(cwd) {
  const dir = projectDirFor(cwd);
  if (!dir) return null;
  let files;
  try {
    files = fs.readdirSync(dir)
      .filter((f) => f.endsWith('.jsonl'))
      .map((f) => { const p = path.join(dir, f); return { p, m: fs.statSync(p).mtimeMs }; })
      .sort((a, b) => b.m - a.m)
      .slice(0, 4);
  } catch { return null; }
  for (const { p } of files) {
    const entries = readRecentEntries(p, 64 * 1024);
    for (let i = entries.length - 1; i >= 0; i--) {
      const e = entries[i];
      if (e && e.type === 'assistant' && e.message && e.message.model) {
        return friendlyModel(e.message.model);
      }
    }
  }
  return null;
}

/* ================= переход к терминалу ================= */

function ps1(pid) {
  return new Promise((resolve) => {
    execFile('ps', ['-o', 'ppid=,command=', '-p', String(pid)], (err, out) => {
      if (err) return resolve(null);
      const m = String(out).trim().match(/^(\d+)\s+(.*)$/);
      resolve(m ? { ppid: Number(m[1]), command: m[2] } : null);
    });
  });
}

/** вверх по цепочке родителей до GUI-приложения (.app) — IDE-терминалы и пр. */
async function guiAncestorApp(pid) {
  let cur = pid;
  for (let i = 0; i < 10 && cur && cur > 1; i++) {
    const info = await ps1(cur);
    if (!info) return null;
    const m = info.command.match(/\/([^/]+)\.app\/Contents\/MacOS\//);
    if (m) return { pid: cur, name: m[1] };
    cur = info.ppid;
  }
  return null;
}

function activateAppByPid(pid) {
  const script = `tell application "System Events" to set frontmost of (first application process whose unix id is ${Number(pid)}) to true`;
  return new Promise((resolve) => {
    execFile('osascript', ['-e', script], { timeout: 4000 }, (err) => resolve(!err));
  });
}

const FOCUS_SCRIPT = `
on run argv
  set theTty to item 1 of argv
  try
    if application "iTerm2" is running then
      tell application "iTerm2"
        repeat with w in windows
          repeat with t in tabs of w
            repeat with se in sessions of t
              if tty of se is theTty then
                select se
                select t
                activate
                return "ok"
              end if
            end repeat
          end repeat
        end repeat
      end tell
    end if
  end try
  try
    if application "Terminal" is running then
      tell application "Terminal"
        repeat with w in windows
          repeat with t in tabs of w
            if tty of t is theTty then
              set selected of t to true
              set index of w to 1
              activate
              return "ok"
            end if
          end repeat
        end repeat
      end tell
    end if
  end try
  return "no"
end run`;

function focusTerminalByTty(tty) {
  return new Promise((resolve) => {
    execFile('osascript', ['-e', FOCUS_SCRIPT, tty], { timeout: 5000 }, (err, out) => {
      resolve(!err && String(out).trim() === 'ok');
    });
  });
}

/* ================= лимит провайдера ================= */
/* Лимит — состояние аккаунта, не сессии: упёрлась одна — встали все.
 * Сигнал: хук StopFailure (ход умер об API). Время сброса — официальное
 * (claude -p "/usage"). После сброса ждавшим tmux-сессиям шлём «продолжай»
 * со стаггером, чтобы не сжечь свежее окно залпом. */

const limitState = { active: false, kind: '', plan: '', resetAt: 0, since: 0 };
let resumeTimer = null;

function pushLimit() {
  if (panel && !panel.isDestroyed()) {
    panel.webContents.send('limit-state', { ...limitState });
  }
}

function limitKindLabel(kind) {
  return kind === 'billing' ? 'биллинг' : kind === 'overloaded' ? 'перегрузка API' : 'лимит использования';
}

function fmtResetIn(ts) {
  const min = Math.max(0, Math.round((ts - Date.now()) / 60000));
  return min < 60 ? `${min}м` : `${Math.floor(min / 60)}ч ${min % 60}м`;
}

// точная классификация StopFailure: НЕ дефолтим в rate_limit (перегрузка/сбой
// сети — частые и НЕ лимит аккаунта). Аккаунтный баннер — только при явном
// rate-limit И подтверждении официальным usage.
function classifyFailure(rawPayload) {
  const raw = JSON.stringify(rawPayload || {}).toLowerCase();
  if (/billing|payment|insufficient|credit/.test(raw)) return 'billing';
  if (/rate.?limit|usage limit|quota|429|limit reached|limit_exceeded/.test(raw)) return 'rate_limit';
  if (/overload|503|529|capacity/.test(raw)) return 'overloaded';
  return 'transient'; // неизвестная ошибка хода — НЕ лимит
}

function onStopFailure(s, rawPayload) {
  const kind = classifyFailure(rawPayload);
  const off = usage.officialInfo();
  const plan = (off && off.account && off.account.plan) || '';
  const sessPct = off && off.session ? off.session.pct : null;

  // аккаунтный лимит подтверждаем официальным usage: если /usage знает и
  // показывает <85% — это НЕ упирание в стену, а транзиентный сбой
  const realLimit = kind === 'rate_limit' && (sessPct == null || sessPct >= 85);

  if (!realLimit) {
    // транзиент: помечаем только сессию, без аккаунтного баннера и авто-резюма
    s.status = 'idle';
    s.detail = kind === 'overloaded' ? 'API перегружен — попробуй ещё раз'
      : kind === 'billing' ? 'ошибка биллинга'
      : 'ход прервался ошибкой';
    console.log(`[jarvis] stop-failure (${s.project}): ${kind}, sessPct=${sessPct} → транзиент, баннер не показываю`);
    return;
  }

  const resetAt = (off && off.session && off.session.resetAt) || (Date.now() + 60 * 60e3);
  s.status = 'limit';
  s.limitWait = true;
  s.detail = `лимит использования · сброс через ${fmtResetIn(resetAt)}`;

  limitState.active = true;
  limitState.kind = 'rate_limit';
  limitState.plan = plan;
  limitState.resetAt = resetAt;
  limitState.since = Date.now();
  pushLimit();
  usage.refreshOfficial();
  scheduleAutoResume();

  notify(
    `Claude${plan ? ` ${plan}` : ''} — лимит использования`,
    `Сброс через ${fmtResetIn(resetAt)} · ${s.project} ${settings.load().autoResume ? '— продолжу сам' : 'ждёт'}`,
    s.id,
    'limit',
  );
  console.log(`[jarvis] stop-failure (${s.project}): подтверждённый лимит (sessPct=${sessPct}), сброс ~${new Date(resetAt).toLocaleTimeString()}`);
}

/** самоисцеление баннера: официальный usage упал ниже 80% или окно сброшено */
function reconcileLimit() {
  if (!limitState.active) return;
  const off = usage.officialInfo();
  const pct = off && off.session ? off.session.pct : null;
  if ((pct != null && pct < 80) || (limitState.resetAt && Date.now() > limitState.resetAt)) {
    limitState.active = false;
    for (const x of sessions.values()) if (x.status === 'limit') { x.status = 'idle'; x.limitWait = false; }
    pushLimit();
    push();
    console.log('[jarvis] лимит-баннер снят (usage упал / окно сброшено)');
  }
}

function scheduleAutoResume() {
  clearTimeout(resumeTimer);
  if (!settings.load().autoResume) return;
  const delay = Math.min(
    Math.max(10e3, limitState.resetAt - Date.now() + 90e3), // +90с джиттера после сброса
    6 * 3600e3,
  );
  resumeTimer = setTimeout(runAutoResume, delay);
  console.log(`[jarvis] авто-продолжение через ${Math.round(delay / 60000)} мин`);
}

async function runAutoResume() {
  limitState.active = false;
  pushLimit();
  const waiters = [...sessions.values()].filter((x) => x.limitWait && x.tmuxPane);
  let i = 0;
  for (const s of waiters) {
    const delay = i++ * 120e3; // стаггер 2 мин — не сжигаем свежее окно залпом
    setTimeout(async () => {
      if (!s.limitWait || !sessions.has(s.id)) return;
      if (!(await tmuxPaneAlive(s.tmuxPane))) return;
      try {
        await replyViaTmux(s, 'продолжай');
        s.limitWait = false;
        markPromptSent(s, 'продолжай (авто после сброса лимита)');
        console.log(`[jarvis] авто-продолжил ${s.project}`);
      } catch {}
    }, delay);
  }
  if (waiters.length) {
    notify('Claude — лимит сброшен', `Продолжаю ${waiters.length} ${waiters.length === 1 ? 'сессию' : 'сессии'}`, null, 'done');
  }
}

/* ================= сжатый последний вывод модели ================= */

/** маркдаун → одна плотная строка для тоста: код-блоки вон, рез по предложению */
function squeezeReply(t) {
  let x = String(t || '')
    .replace(/```[\s\S]*?```/g, ' ')
    .replace(/`([^`]*)`/g, '$1')
    .replace(/\*\*([^*]*)\*\*/g, '$1')
    .replace(/^[#>\-•*\s]+/gm, ' ')
    .replace(/[★─━]{1,}/g, ' ');
  x = oneLine(x);
  if (x.length <= 220) return x;
  const cut = x.slice(0, 220);
  const dot = Math.max(cut.lastIndexOf('. '), cut.lastIndexOf('! '), cut.lastIndexOf('? '));
  if (dot > 90) return cut.slice(0, dot + 1);
  const sp = cut.lastIndexOf(' ');
  return `${cut.slice(0, sp > 150 ? sp : 220)}…`;
}

/** последний СОДЕРЖАТЕЛЬНЫЙ ответ ассистента: финальные реплики часто
 * короткие подводки («Теперь preload и рендерер:») — идём с конца к первому
 * осмысленному тексту, иначе берём самый длинный из хвоста */
function lastAssistantReply(s) {
  if (!s.transcript) return null;
  try {
    const texts = chainFromEntries(readRecentEntries(s.transcript, 128 * 1024))
      .flatMap(toChatItems)
      .filter((i) => i.kind === 'text' && i.role === 'assistant')
      .slice(-5)
      .map((i) => squeezeReply(i.text))
      .filter(Boolean);
    for (let i = texts.length - 1; i >= 0; i--) {
      if (texts[i].length >= 60 && !/[:：]\s*$/.test(texts[i])) return texts[i];
    }
    return texts.sort((a, b) => b.length - a.length)[0] || null;
  } catch {
    return null;
  }
}

/* ================= notifications ================= */

/* Уведомления — собственный тост снизу экрана: приходит ВСЕГДА (не зависит
 * от разрешений macOS и Focus-режимов), кликом открывает чат сессии. */

/* Стек тостов: несколько карточек копятся столбиком (новые снизу), у каждой
 * свой таймер и клик; renderer управляет жизнью карточек и сообщает высоту,
 * main двигает окно (низ прибит к краю экрана). */

const TOAST_W = 440;
const TOAST_MAX_H = 480;
let toastWin = null;
let toastSeq = 0;

function ensureToast() {
  if (toastWin && !toastWin.isDestroyed()) return;
  toastWin = new BrowserWindow({
    width: TOAST_W,
    height: 120,
    show: false,
    frame: false,
    transparent: true,
    resizable: false,
    movable: false,
    minimizable: false,
    maximizable: false,
    fullscreenable: false,
    skipTaskbar: true,
    hasShadow: false,
    roundedCorners: false, // форму рисует карточка, а не системное окно
    focusable: false, // клики работают, фокус не воруется
    webPreferences: {
      preload: path.join(__dirname, 'preload-toast.js'),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });
  toastWin.setAlwaysOnTop(true, 'screen-saver');
  toastWin.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true });
  toastWin.loadFile(path.join(__dirname, 'renderer', 'toast.html'));
}

function notify(title, body, sessionId, kind) {
  ensureToast();
  const payload = {
    id: `t${++toastSeq}`,
    title,
    body: body || '',
    sessionId: sessionId || null,
    kind: kind || 'done',
  };
  const send = () => { try { toastWin.webContents.send('toast-add', payload); } catch {} };
  if (toastWin.webContents.isLoading()) toastWin.webContents.once('did-finish-load', send);
  else send();
  return payload.id;
}

/** полный финальный ответ агента (все текст-блоки после последнего промпта юзера) */
function fullFinalReply(s) {
  if (!s.transcript) return null;
  try {
    const items = chainFromEntries(readRecentEntries(s.transcript, 256 * 1024))
      .flatMap(toChatItems)
      .filter((i) => i.kind === 'text');
    let lastUser = -1;
    for (let i = items.length - 1; i >= 0; i--) {
      if (items[i].role === 'user') { lastUser = i; break; }
    }
    const reply = items.slice(lastUser + 1).filter((i) => i.role === 'assistant').map((i) => i.text).join('\n');
    return reply.trim().slice(0, 6000) || null;
  } catch {
    return null;
  }
}

/** ИИ-выжимка финального ответа для тоста: haiku, ~4 строки; карточка
 * показывается сразу с черновым текстом и обновляется, когда выжимка готова */
function aiToastSummary(s, toastId) {
  const bin = resolveClaudeBin();
  if (!bin || s.aiSumBusy) return;
  const reply = fullFinalReply(s);
  if (!reply || reply.length < 80) return; // короткий ответ и так влез целиком
  s.aiSumBusy = true;
  execFile(bin, [
    '-p', '--no-session-persistence', '--model', 'haiku',
    `Ответ агента:\n${reply}\n\nЗадача: сожми этот ответ в выжимку до 280 символов по-русски — что сделано и каков итог. Без вступлений, без markdown, технические термины не переводи. Только текст выжимки.`,
  ], {
    cwd: os.tmpdir(),
    timeout: 60 * 1000,
    maxBuffer: 1024 * 1024,
    env: { ...process.env, JARVIS_IGNORE: '1' },
  }, (err, stdout) => {
    s.aiSumBusy = false;
    if (err) return; // тост остаётся с черновым текстом
    const text = oneLine(stdout).slice(0, 320);
    if (!text) return;
    console.log(`[jarvis] ai-toast (${s.project}): ${text.slice(0, 80)}…`);
    try { toastWin?.webContents.send('toast-update', { id: toastId, body: text }); } catch {}
  });
}

// renderer сообщает нужную высоту стека; 0 — спрятаться
ipcMain.handle('toast:resize', (_e, h) => {
  if (!toastWin || toastWin.isDestroyed()) return;
  const height = Math.max(1, Math.min(TOAST_MAX_H, Math.round(h) || 0));
  if (!h) { try { toastWin.hide(); } catch {} return; }
  const { workArea } = screen.getDisplayNearestPoint(screen.getCursorScreenPoint());
  toastWin.setBounds({
    x: workArea.x + Math.round((workArea.width - TOAST_W) / 2),
    y: workArea.y + workArea.height - height - 14, // низ прибит, окно растёт вверх
    width: TOAST_W,
    height,
  });
  if (!toastWin.isVisible()) toastWin.showInactive();
});

ipcMain.handle('toast:click', (_e, sessionId) => {
  showPanelFocused();
  if (sessionId && panel && !panel.isDestroyed()) {
    panel.webContents.send('open-session', sessionId);
  }
});

/* ================= unix-socket server ================= */

function startServer() {
  fs.mkdirSync(JARVIS_DIR, { recursive: true });
  try { fs.unlinkSync(SOCK); } catch {}

  server = http.createServer((req, res) => {
    if (req.method === 'GET') {
      if (req.url === '/state') { // самодиагностика: что в реестре
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify(snapshot(), null, 2) + '\n');
        return;
      }
      res.writeHead(200, { 'content-type': 'text/plain' });
      res.end('jarvis ok\n');
      return;
    }
    if (req.method !== 'POST') {
      res.writeHead(405);
      res.end();
      return;
    }
    let body = '';
    req.on('data', (c) => {
      body += c;
      if (body.length > 4e6) req.destroy(); // защита от мусора (диффы Edit бывают жирными)
    });
    req.on('end', () => {
      try {
        reduce(JSON.parse(body));
        res.writeHead(204);
      } catch {
        res.writeHead(400);
      }
      res.end();
    });
  });

  server.on('error', (err) => {
    console.error('[jarvis] server error:', err.message);
  });

  server.listen(SOCK, () => {
    try { fs.chmodSync(SOCK, 0o600); } catch {}
    console.log(`[jarvis] слушаю ${SOCK}`);
  });
}

/* ================= panel window ================= */

function createPanel() {
  panel = new BrowserWindow({
    width: PANEL_W,
    height: PANEL_H,
    show: false,
    frame: false,
    // настоящий блюр подложки: нативный NSVisualEffectView, не CSS
    transparent: false,
    backgroundColor: '#00000000',
    vibrancy: 'under-window',
    visualEffectState: 'active', // блюр не гаснет у неактивного окна (тихий показ)
    roundedCorners: true,
    resizable: false,
    minimizable: false,
    maximizable: false,
    fullscreenable: false,
    skipTaskbar: true,
    hasShadow: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });

  // Поверх всего, на всех Spaces, над фуллскрином — но фокус не ворует
  panel.setAlwaysOnTop(true, 'screen-saver');
  panel.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true });
  panel.loadFile(path.join(__dirname, 'renderer', 'index.html'));
  panel.webContents.on('did-finish-load', push);

  panel.on('close', (e) => {
    // ⌘W и крестик — просто прячем, демон живёт
    if (!app.isQuittingForReal) {
      e.preventDefault();
      panel.hide();
    }
  });

  // raycast-режим: потеря фокуса — спрятаться
  panel.on('blur', () => {
    if (panelFocusMode && panel.isVisible()) panel.hide();
  });

  positionPanel();
}

function positionPanel() {
  const display = screen.getDisplayNearestPoint(screen.getCursorScreenPoint());
  const { workArea } = display;
  const [w, h] = panel.getSize();
  if (settings.load().position === 'corner') {
    panel.setPosition(workArea.x + workArea.width - w - 12, workArea.y + 12);
  } else {
    // центр по горизонтали, ~⅓ сверху — как Raycast
    panel.setPosition(
      workArea.x + Math.round((workArea.width - w) / 2),
      workArea.y + Math.round((workArea.height - h) / 3),
    );
  }
}

function showPanel() { // тихий режим: трей, клик по уведомлению
  if (!panel || panel.isDestroyed()) createPanel();
  panelFocusMode = false;
  positionPanel();
  panel.webContents.send('panel-shown');
  panel.showInactive(); // показать, не забирая фокус у кино/терминала
  push();
}

function showPanelFocused() { // raycast-режим: хоткей
  if (!panel || panel.isDestroyed()) createPanel();
  panelFocusMode = true;
  positionPanel();
  panel.webContents.send('panel-shown');
  panel.show();
  panel.focus();
  push();
}

function togglePanel() { // трей
  if (panel && panel.isVisible()) panel.hide();
  else showPanel();
}

function toggleHotkeyPanel() { // глобальный хоткей
  if (panel && panel.isVisible()) panel.hide();
  else showPanelFocused();
}

function registerHotkey(accelerator) {
  const current = settings.load().hotkey;
  if (accelerator === current && globalShortcut.isRegistered(accelerator)) {
    return { ok: true };
  }
  if (accelerator !== current) {
    try { globalShortcut.unregister(current); } catch {}
  }
  let ok = false;
  try { ok = globalShortcut.register(accelerator, toggleHotkeyPanel); } catch {}
  if (!ok) {
    if (accelerator !== current) {
      try { globalShortcut.register(current, toggleHotkeyPanel); } catch {}
    }
    return { ok: false, error: `Сочетание ${accelerator} занято системой` };
  }
  return { ok };
}

/* ================= tray ================= */

function updateTray(list = snapshot()) {
  if (!tray) return;
  const waiting = list.filter((s) => s.status === 'waiting').length;
  const working = list.filter((s) => s.status === 'working').length;
  const done = list.filter((s) => s.status === 'done').length;

  let title = '◇';
  const badges = pluginHost.badges(); // ☕ assertion, ⌒ clamshell — как у Amphetamine
  if (badges) title += ' ' + badges;
  const parts = [];
  if (waiting) parts.push(`⏸${waiting}`);
  if (working) parts.push(`⚙${working}`);
  if (!parts.length && done) parts.push(`✓${done}`);
  if (parts.length) title += ' ' + parts.join(' ');
  tray.setTitle(title);
}

function createTray() {
  // Пустая иконка + текстовый title — на macOS этого достаточно для MVP
  tray = new Tray(nativeImage.createEmpty());
  tray.setToolTip('Jarvis — монитор сессий Claude Code');
  updateTray();

  tray.on('click', togglePanel);
  tray.on('right-click', async () => {
    // секции плагинов (Не спать / Крышка) собираются асинхронно с таймаутом
    let pluginItems = [];
    try { pluginItems = await pluginHost.trayMenus(); } catch {}
    tray.popUpContextMenu(Menu.buildFromTemplate([
      { label: 'Показать панель', click: showPanel },
      { label: 'Тестовое уведомление', click: () => notify('Jarvis на связи', 'Уведомления работают') },
      ...(pluginItems.length ? [{ type: 'separator' }, ...pluginItems] : []),
      { type: 'separator' },
      {
        label: 'Запускать при входе',
        type: 'checkbox',
        checked: app.getLoginItemSettings().openAtLogin,
        click: (item) => app.setLoginItemSettings({ openAtLogin: item.checked }),
      },
      { type: 'separator' },
      { label: 'Выйти', click: () => { app.isQuittingForReal = true; app.quit(); } },
    ]));
  });
}

/* ================= ipc ================= */

ipcMain.handle('state:get', () => snapshot());
ipcMain.handle('state:clear', () => {
  for (const [id, s] of sessions) {
    if (s.status === 'done' || s.status === 'idle') sessions.delete(id);
  }
  push();
});
ipcMain.handle('panel:hide', () => panel?.hide());

ipcMain.handle('settings:get', () => ({
  ...settings.load(),
  openAtLogin: app.getLoginItemSettings().openAtLogin,
}));

ipcMain.handle('settings:set', (_e, patch) => {
  if (!patch || typeof patch !== 'object') return { ok: false, error: 'bad patch' };
  const { openAtLogin, hotkey, ...rest } = patch;

  if (typeof openAtLogin === 'boolean') app.setLoginItemSettings({ openAtLogin });

  if (typeof hotkey === 'string' && hotkey) {
    const res = registerHotkey(hotkey);
    if (!res.ok) return res;
    settings.save({ hotkey });
  }

  if (Object.keys(rest).length) settings.save(rest);
  if (panel && panel.isVisible()) positionPanel(); // позиция могла смениться
  return { ok: true };
});

ipcMain.handle('chat:open', (_e, sessionId) => {
  const s = sessions.get(sessionId);
  if (!s) return { ok: false, error: 'Сессия не найдена' };
  if (!s.transcript) {
    return { ok: false, error: 'Нет транскрипта — сессия ещё не слала событий (перезапусти claude)' };
  }
  const items = chainFromEntries(readRecentEntries(s.transcript))
    .flatMap(toChatItems)
    .slice(-80);
  startTail(sessionId, s.transcript);
  console.log(`[jarvis] chat:open ${sessionId.slice(0, 8)} items=${items.length} file=${shortHome(s.transcript)}`);
  return { ok: true, items, project: s.project };
});

ipcMain.handle('chat:close', () => stopTail());

ipcMain.handle('commands:get', (_e, sessionId) => {
  const s = sessions.get(sessionId);
  return commands.getForCwd(s && s.cwd);
});

ipcMain.handle('app:meta', () => ({ effortLevels }));

ipcMain.handle('plugins:status', () => pluginHost.statuses());
ipcMain.handle('plugins:cmd', (_e, id, cmd, args) => pluginHost.cmd(id, cmd, args));

ipcMain.handle('usage:summary', (_e, period) => usage.stats(period));
ipcMain.handle('limit:get', () => ({ ...limitState }));
ipcMain.handle('history:get', () => history.projects());
ipcMain.handle('usage:session', (_e, id) => usage.forSession(id));

ipcMain.handle('session:setPin', (_e, sessionId, pinned) => {
  const s = sessions.get(sessionId);
  if (s) { s.pinned = !!pinned; push(); }
  return { ok: !!s };
});

ipcMain.handle('session:setModel', async (_e, sessionId, model) => {
  const s = sessions.get(sessionId);
  if (!s) return { ok: false, error: 'Сессия не найдена' };
  if (!s.tmuxPane || !(await tmuxPaneAlive(s.tmuxPane))) return tmuxNeededResult(sessionId);
  try {
    await pasteSlash(s, `/model ${model}`);
    s.model = friendlyModel(model); // оптимистично; транскрипт подтвердит
    s.modelAt = Date.now();
    push();
    return { ok: true };
  } catch (err) {
    return { ok: false, error: oneLine(err.message).slice(0, 100) };
  }
});

ipcMain.handle('session:setEffort', async (_e, sessionId, level) => {
  const s = sessions.get(sessionId);
  if (!s) return { ok: false, error: 'Сессия не найдена' };
  if (!s.tmuxPane || !(await tmuxPaneAlive(s.tmuxPane))) return tmuxNeededResult(sessionId);
  try {
    await pasteSlash(s, `/effort ${level}`);
    s.effort = level; // effort снаружи не читается — ведём оптимистично
    push();
    return { ok: true };
  } catch (err) {
    return { ok: false, error: oneLine(err.message).slice(0, 100) };
  }
});

// «где это?» — секундный оверлей прямо в терминале сессии, фокус не воруем
ipcMain.handle('terminal:ping', async (_e, sessionId) => {
  const s = sessions.get(sessionId);
  if (!s) return { ok: false, error: 'Сессия не найдена' };
  if (!s.tmuxPane) return { ok: false, error: 'Сессия не в tmux — пингануть нечем' };
  // popup рисуется в подключённом клиенте — у detached-сессии его нет
  const clients = await tmuxJ(['list-clients', '-t', s.tmuxPane, '-F', '#{client_name}']).catch(() => '');
  if (!oneLine(clients)) {
    return { ok: false, error: 'Окно терминала не подключено (detached) — показать негде' };
  }
  try {
    await tmuxJ(['display-popup', '-t', s.tmuxPane, '-w', '34', '-h', '3', '-E',
      'printf "\\n   ◇ Jarvis: вот эта сессия"; sleep 1']);
    return { ok: true };
  } catch (err) {
    return { ok: false, error: `Поповер не показался: ${oneLine(err.message).slice(0, 80)}` };
  }
});

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/* Ответ на AskUserQuestion клавишами в пану (механика проверена на живом пикере):
 * single-select — цифра выбирает и подтверждает сразу;
 * multiSelect — цифры тогглят чекбоксы, Right ведёт на Submit-таб,
 * там Review-экран, где «1» = Submit answers. */
ipcMain.handle('question:answer', async (_e, sessionId, choice) => {
  const s = sessions.get(sessionId);
  if (!s || !s.question) return { ok: false, error: 'Вопрос уже неактуален' };
  if (!s.tmuxPane) return { ok: false, error: 'Сессия вне tmux — ответь в терминале' };
  if (!(await tmuxPaneAlive(s.tmuxPane))) return { ok: false, error: 'Пана сессии не отвечает' };

  const idx = (Array.isArray(choice?.indices) ? choice.indices : [])
    .filter((n) => Number.isInteger(n) && n >= 1 && n <= 9);
  if (!idx.length) return { ok: false, error: 'Пустой выбор' };

  try {
    if (choice?.multiSelect) {
      for (const n of idx) {
        await tmuxJ(['send-keys', '-t', s.tmuxPane, String(n)]);
        await sleep(150);
      }
      await tmuxJ(['send-keys', '-t', s.tmuxPane, 'Right']);
      await sleep(200);
      await tmuxJ(['send-keys', '-t', s.tmuxPane, '1']); // Review: «1. Submit answers»
    } else {
      await tmuxJ(['send-keys', '-t', s.tmuxPane, String(idx[0])]);
    }
    // у хук-вопроса карточку закроет post-tool; у экранного — событий нет,
    // снимаем сами (детектор подтвердит по idle-экрану на следующем проходе)
    if (s.question && s.question.fromScreen) {
      s.question = null;
      s.status = 'working';
      s.updatedAt = Date.now();
      push();
    }
    return { ok: true };
  } catch (err) {
    return { ok: false, error: oneLine(err.message).slice(0, 100) };
  }
});

ipcMain.handle('session:reply', async (_e, sessionId, text) => {
  const s = sessions.get(sessionId);
  if (!s) return { ok: false, error: 'Сессия не найдена' };
  const prompt = String(text || '').trim();
  if (!prompt) return { ok: false, error: 'Пустой текст' };

  if (s.tmuxPane) {
    if (await tmuxPaneAlive(s.tmuxPane)) {
      try {
        await replyViaTmux(s, prompt);
        markPromptSent(s, prompt);
        console.log(`[jarvis] reply→tmux ${s.tmuxPane} (${s.project})`);
        return { ok: true, channel: 'tmux' };
      } catch (err) {
        console.error(`[jarvis] reply tmux fail: ${err.message}`);
        return { ok: false, error: `tmux: ${oneLine(err.message).slice(0, 120)}` };
      }
    }
    delete s.tmuxPane; // пана умерла
    push();
  }
  // вне tmux отправку не делаем — говорим, что запустить, чтобы сессия попала в tmux
  return tmuxNeededResult(sessionId);
});

function activateAppByName(name) {
  return new Promise((resolve) => {
    execFile('osascript', ['-e', `tell application ${JSON.stringify(String(name))} to activate`],
      { timeout: 4000 }, (err) => resolve(!err));
  });
}

/* Лесенка «показать терминал»: tmux → вкладка по tty (Terminal/iTerm2) →
 * GUI-приложение-владелец (JetBrains, VS Code…). Нижняя ступень — не тост,
 * а чат сессии в самой панели: renderer открывает его сам при ok:false. */
ipcMain.handle('terminal:focus', async (_e, sessionId) => {
  const s = sessions.get(sessionId);
  if (!s) return { ok: false, error: 'Сессия не найдена' };

  // 1) tmux — точнее некуда
  if (s.tmuxPane) {
    const ok = await new Promise((resolve) => {
      execFile('tmux', ['switch-client', '-t', s.tmuxPane], (err) => {
        if (!err) return resolve(true);
        execFile('tmux', ['select-window', '-t', s.tmuxPane], (e2) => resolve(!e2));
      });
    });
    if (ok) return { ok: true };
  }

  // 2) скриптуемые терминалы: точный фокус вкладки по tty
  if (s.tty && await focusTerminalByTty(`/dev/${s.tty}`)) return { ok: true };

  // 3) GUI-приложение, в котором живёт терминал (JediTerm и прочие без API)
  if (s.app && await activateAppByName(s.app)) return { ok: true, app: s.app };
  if (s.pid) {
    const app = await guiAncestorApp(s.pid);
    if (app && await activateAppByPid(app.pid)) return { ok: true, app: app.name };
  }

  return { ok: false, error: 'Терминал не нашёлся — открываю чат', fallbackChat: true };
});

/* ================= app lifecycle ================= */

const gotLock = app.requestSingleInstanceLock();
if (!gotLock) {
  app.quit();
} else {
  app.on('second-instance', showPanel);

  app.whenReady().then(() => {
    app.setName('Jarvis');
    nativeTheme.themeSource = 'dark'; // вибранси всегда тёмный, как дизайн
    app.dock?.hide(); // чистое меню-бар приложение
    restoreState();
    createPanel();
    createTray();
    startServer();
    // плагины (Не спать, Крышка…) — после трея: их changed() дёргает updateTray
    pluginHost.init({
      settingsStore: settings,
      sessions: snapshot,
      notify,
      changed: () => {
        updateTray();
        if (panel && !panel.isDestroyed()) {
          panel.webContents.send('plugins', pluginHost.statuses());
        }
      },
    });
    registerHotkey(settings.load().hotkey);
    sweepSessions();
    setInterval(sweepSessions, 30000);
    setInterval(reconcileLimit, 60000); // снять ложный лимит-баннер по официальному usage
    // детект интерактивных промптов на экране (подтверждения slash-команд и пр.)
    setInterval(() => {
      for (const s of sessions.values()) detectStuckPrompt(s).catch(() => {});
    }, 7000);
    detectEffortLevels();
    usage.start({
      resolveBin: resolveClaudeBin,
      // предупреждение до стены: окно пересекло 90%
      onLimitWarning: (w) => notify(
        `Claude${w.account.plan ? ` ${w.account.plan}` : ''} — окно почти исчерпано`,
        `${w.pct}% использовано · сброс через ${fmtResetIn(w.resetAt)}`,
        null,
        'limit',
      ),
    });
    history.start({ tokenLookup: (id) => usage.forSession(id) }); // история чатов по проектам
    // автокомплит команд самообновляется при правке файлов команд
    try { fs.watch(commands.USER_CMDS, () => commands.invalidate()); } catch {}
  });

  app.on('will-quit', () => globalShortcut.unregisterAll());

  // Меню-бар приложение: окна закрыты — живём дальше
  app.on('window-all-closed', () => {});

  app.on('before-quit', () => {
    app.isQuittingForReal = true;
    clearTimeout(persistTimer);
    writeStateNow(); // реестр переживает перезапуск
    try { pluginHost.dispose(); } catch {} // снять assertion, вернуть disablesleep
    try { server?.close(); } catch {}
    try { fs.unlinkSync(SOCK); } catch {}
  });
}
