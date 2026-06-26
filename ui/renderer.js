/* Панель Jarvis: сессии, чат сессии, настройки. Все данные — через textContent, без innerHTML. */

const panelEl = document.getElementById('panel');
const listEl = document.getElementById('list');
const chatEl = document.getElementById('chat');
const chatlogEl = document.getElementById('chatlog');
const chatTitleEl = document.getElementById('chatTitle');
const chatChannelEl = document.getElementById('chatChannel');
const chatModelEl = document.getElementById('chatModel');
const chatDotEl = document.getElementById('chatDot');
const settingsEl = document.getElementById('settings');
const queryEl = document.getElementById('query');
const replyEl = document.getElementById('reply');
const footerLeftEl = document.getElementById('footerLeft');
const tabSessionsEl = document.getElementById('tabSessions');
const tabSettingsEl = document.getElementById('tabSettings');

const STATUS_LABEL = {
  working: 'работает',
  waiting: 'ждёт тебя',
  done: 'готово',
  idle: 'простаивает',
  limit: 'лимит — ждёт сброса',
};

let state = [];
let sel = 0;
let view = 'list'; // list | chat | settings
let chatSessionId = null;

/* ---------- helpers ---------- */

const SVG_NS = 'http://www.w3.org/2000/svg';
function svgIcon(paths, size = 13) {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('width', size);
  svg.setAttribute('height', size);
  svg.setAttribute('fill', 'none');
  svg.setAttribute('stroke', 'currentColor');
  svg.setAttribute('stroke-width', '2');
  svg.setAttribute('stroke-linecap', 'round');
  svg.setAttribute('stroke-linejoin', 'round');
  for (const d of paths) {
    const p = document.createElementNS(SVG_NS, 'path');
    p.setAttribute('d', d);
    svg.appendChild(p);
  }
  return svg;
}
// закладка (lucide bookmark) — чистая вертикальная иконка для закреплённых
const BOOKMARK_PATHS = ['m19 21-7-4-7 4V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z'];

const pad2 = (n) => String(n).padStart(2, '0');

// время старта сессии (createdAt): сегодня → ЧЧ:ММ, вчера → «вчера ЧЧ:ММ», раньше → ДД.ММ
function startLabel(ts) {
  if (!ts) return '';
  const d = new Date(ts);
  const hm = `${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
  const now = new Date();
  const sameDate = (a, b) =>
    a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
  if (sameDate(d, now)) return hm;
  const yest = new Date(now);
  yest.setDate(now.getDate() - 1);
  if (sameDate(d, yest)) return `вчера ${hm}`;
  return `${pad2(d.getDate())}.${pad2(d.getMonth() + 1)}`;
}

function startTitle(ts) {
  if (!ts) return '';
  const d = new Date(ts);
  return `Запущена ${pad2(d.getDate())}.${pad2(d.getMonth() + 1)} в ${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
}

function plural(n, one, few, many) {
  const m10 = n % 10, m100 = n % 100;
  if (m10 === 1 && m100 !== 11) return one;
  if (m10 >= 2 && m10 <= 4 && (m100 < 12 || m100 > 14)) return few;
  return many;
}

let toastTimer = null;
function showToast(text) {
  document.querySelector('.toast')?.remove();
  clearTimeout(toastTimer);
  const t = document.createElement('div');
  t.className = 'toast';
  t.textContent = text;
  document.body.appendChild(t);
  toastTimer = setTimeout(() => t.remove(), 2200);
}

/* ---------- вьюхи ---------- */

function setView(next) {
  if (view === 'chat' && next !== 'chat') {
    window.jarvis.closeChat();
    chatSessionId = null;
  }
  if (view === 'question' && next !== 'question') qSessionId = null;
  if (next === 'history' && view !== 'history') histProject = null; // вкладка всегда открывается со списка проектов
  view = next;
  closeActions();
  // в чате и на экране вопроса поиск и табы не нужны — чистый фокус-режим
  const minimal = next === 'chat' || next === 'question';
  document.querySelector('.cmdrow').hidden = minimal;
  document.querySelector('.tabs').hidden = minimal;
  listEl.hidden = next !== 'list';
  chatEl.hidden = next !== 'chat';
  qviewEl.hidden = next !== 'question';
  settingsEl.hidden = next !== 'settings';
  statsEl.hidden = next !== 'stats';
  historyEl.hidden = next !== 'history';
  // чат и вопрос несут собственные нижние бары — парящий футер только тут
  footerEl.hidden = next === 'chat' || next === 'question';
  if (next === 'list') { primaryLabelEl.textContent = 'Открыть чат'; primaryKeyEl.textContent = '↵'; }
  else if (next === 'history') { primaryLabelEl.textContent = 'Открыть проект'; primaryKeyEl.textContent = '↵'; }
  else { primaryLabelEl.textContent = 'Назад'; primaryKeyEl.textContent = 'esc'; }
  tabSettingsEl.classList.toggle('active', next === 'settings');
  tabStatsEl.classList.toggle('active', next === 'stats');
  tabHistoryEl.classList.toggle('active', next === 'history');
  tabSessionsEl.classList.toggle('active', next === 'list' || next === 'chat');
  if (next === 'settings') loadSettings();
  if (next === 'stats') renderStats();
  if (next === 'history') renderHistory();
  else if (recording) { recording = false; recordingBtn.classList.remove('recording'); }
  if (next === 'list') queryEl.focus();
}

// Клик/Enter по сессии: всегда открываем чат; если у сессии есть вопрос —
// сразу поднимаем слайд-овер вариантов поверх чата (видно переписку И варианты).
function openSession(s) {
  openChat(s.id, s.project).then(() => {
    if (questionOf(s)) openVarPanel();
  });
}

/* ---------- список сессий ---------- */

const HOST_LABEL = {
  'iTerm.app': 'iTerm',
  Apple_Terminal: 'Terminal',
  vscode: 'VS Code',
  'JetBrains-JediTerm': 'JetBrains',
  WezTerm: 'WezTerm',
  ghostty: 'Ghostty',
};

function hostLabel(s) {
  if (s.app) return s.app; // точное имя GUI-приложения (WebStorm, IDEA…)
  return HOST_LABEL[s.host] || null;
}

let displayOrder = []; // зафиксированный порядок строк (id); полная сортировка — только при открытии

// закреплённые сверху, дальше — по свежести последнего завершённого ответа:
// чат, который только что отработал, всплывает наверх; кто ещё ни разу не финишировал — по времени старта
function sortCmp(a, b) {
  if (!!a.pinned !== !!b.pinned) return a.pinned ? -1 : 1;
  return (b.doneAt || b.createdAt || 0) - (a.doneAt || a.createdAt || 0);
}

// полная пересортировка — вызывается только при открытии панели
function rebuildOrder() {
  displayOrder = state.slice().sort(sortCmp).map((s) => s.id);
}

// порядок стабилен, пока панель открыта: ушедшие выпадают, новые — в конец,
// закреплённые всплывают наверх (сохраняя относительный порядок)
function orderedSessions() {
  const byId = new Map(state.map((s) => [s.id, s]));
  displayOrder = displayOrder.filter((id) => byId.has(id));
  const known = new Set(displayOrder);
  for (const s of state.filter((x) => !known.has(x.id)).sort(sortCmp)) displayOrder.push(s.id);
  const ordered = displayOrder.map((id) => byId.get(id));
  return [
    ...ordered.filter((s) => s.pinned),
    ...ordered.filter((s) => !s.pinned),
  ];
}

function filtered() {
  const ordered = orderedSessions();
  const q = queryEl.value.trim().toLowerCase();
  if (!q) return ordered;
  return ordered.filter((s) =>
    `${s.project || ''} ${s.detail || ''} ${s.agent || ''}`.toLowerCase().includes(q));
}

function render() {
  if (view !== 'list') return;
  // hover-выбор разоружаем на каждую перерисовку: дальше его снова взведёт только
  // реальное mousemove (см. listEl.mousemove). Иначе фон-обновления (data push)
  // пересоздают строки под неподвижным курсором → mouseenter таскает выделение.
  palHoverEnabled = false;
  // палитра быстрых команд: «/» в главном поиске вместо списка сессий
  if (argMode || queryEl.value.trim().startsWith('/')) { renderCmdPalette(); return; }
  argMode = null;
  listEl.textContent = '';

  footerLeftEl.textContent = footerText();

  const list = filtered();
  sel = Math.min(sel, Math.max(0, list.length - 1));

  if (!list.length) {
    const empty = document.createElement('div');
    empty.className = 'empty';
    empty.textContent = state.length
      ? 'Ничего не найдено'
      : 'Нет активных сессий — запусти claude в любом терминале, они появятся здесь сами.';
    listEl.appendChild(empty);
    return;
  }

  list.forEach((s, i) => {
    const row = document.createElement('div');
    row.className = `row ${s.status}${i === sel ? ' selected' : ''}`;
    row.title = [s.cwd, s.title, ...(s.todoList || [])].filter(Boolean).join('\n');

    const dot = document.createElement('span');
    dot.className = 'dot';

    const name = document.createElement('span');
    name.className = 'name';
    name.textContent = s.project || '?';

    let branch = null;
    if (s.branch) {
      branch = document.createElement('span');
      branch.className = 'gitbranch';
      branch.textContent = `⎇ ${s.branch}`;
    }

    // бейдж агента: показываем только для не-claude (codex), чтобы отличать
    // сессии разных бэкендов в общем списке; для claude — как раньше (без пилла).
    let agentBadge = null;
    if (s.agent && s.agent !== 'claude') {
      agentBadge = document.createElement('span');
      agentBadge.className = 'badge agent';
      agentBadge.textContent = s.agent;
    }

    const badge = document.createElement('span');
    badge.className = 'badge';
    // claude: s.model||'claude' (как было). codex: модель из payload, иначе «…»
    // (не дублируем «codex» — его показывает agentBadge).
    badge.textContent = s.model || (!s.agent || s.agent === 'claude' ? 'claude' : '…');

    const host = hostLabel(s);
    let hostBadge = null;
    if (host) {
      hostBadge = document.createElement('span');
      hostBadge.className = 'badge host';
      hostBadge.textContent = host;
    }

    const summary = document.createElement('span');
    summary.className = 'summary';
    // контекст по убыванию точности: текущая задача → саммари последних задач → промпт → ai-title
    const live = s.detail || STATUS_LABEL[s.status] || '';
    const ctx = s.task
      ? `${s.taskProgress ? s.taskProgress + ' · ' : ''}${s.task}`
      : (s.summary || s.lastPrompt || s.title || '');
    if (s.status === 'working' || s.status === 'waiting') {
      summary.textContent = ctx && ctx !== live ? `${ctx} — ${live}` : (ctx || live);
    } else {
      summary.textContent = ctx || live;
    }

    const time = document.createElement('span');
    time.className = 'time';
    time.textContent = startLabel(s.createdAt);
    time.title = startTitle(s.createdAt);

    row.append(dot, name);
    if (branch) row.appendChild(branch);
    if (agentBadge) row.appendChild(agentBadge);
    row.appendChild(badge);
    if (hostBadge) row.appendChild(hostBadge);
    row.append(summary);

    if (s.pinned) { // чистая метка-закладка; клик — открепить (пин ставится ⌘P)
      const pin = document.createElement('button');
      pin.className = 'pin on';
      pin.title = 'Открепить';
      pin.appendChild(svgIcon(BOOKMARK_PATHS, 12));
      pin.addEventListener('click', (e) => { e.stopPropagation(); window.jarvis.setPin(s.id, false); });
      row.appendChild(pin);
    }

    row.appendChild(time);
    row.addEventListener('mouseenter', () => {
      // hover двигает выбор только при реальном движении мыши (Raycast-стиль):
      // стрелки и фон-перерисовки выделение не теряют
      if (!palHoverEnabled || sel === i) return;
      sel = i; render();
    });
    row.addEventListener('click', () => openSession(s));
    listEl.appendChild(row);
  });
}

/* ---------- чат сессии ---------- */

/* --- мини-маркдаун для реплик ассистента: абзацы, списки, код. Без innerHTML. --- */

function renderInline(el, text) {
  for (const t of text.split(/(`[^`]+`|\*\*[^*]+\*\*)/g)) {
    if (!t) continue;
    if (t.length > 2 && t.startsWith('`') && t.endsWith('`')) {
      const code = document.createElement('code');
      code.textContent = t.slice(1, -1);
      el.appendChild(code);
    } else if (t.length > 4 && t.startsWith('**') && t.endsWith('**')) {
      const b = document.createElement('strong');
      b.textContent = t.slice(2, -2);
      el.appendChild(b);
    } else {
      el.appendChild(document.createTextNode(t));
    }
  }
}

// «N заметок» с правильным русским склонением
function notesWord(n) {
  const d = n % 100, u = n % 10;
  if (d >= 11 && d <= 14) return 'заметок';
  if (u === 1) return 'заметка';
  if (u >= 2 && u <= 4) return 'заметки';
  return 'заметок';
}

function renderMarkdown(root, text) {
  const para = [];
  const code = [];
  let inCode = false;
  let ul = null;
  let callout = null; // .callout (свёрнутый Insight), пока открыт для записи
  let calloutBody = null; // .callout-body — туда пишется содержимое
  let calloutLabel = null; // span с текстом «Insight», в конце дополняем счётчиком

  const target = () => calloutBody || root;

  // финализировать Insight: посчитать заметки и подписать «· N заметок»
  const closeCallout = () => {
    if (!callout) return;
    const n = calloutBody.querySelectorAll('li, p').length;
    if (n) calloutLabel.textContent = `Insight · ${n} ${notesWord(n)}`;
    callout = calloutBody = calloutLabel = null;
  };

  const flushPara = () => {
    if (!para.length) return;
    const p = document.createElement('p');
    para.forEach((line, i) => {
      if (i) p.appendChild(document.createElement('br'));
      renderInline(p, line.replace(/^#+\s+/, ''));
    });
    if (/^#+\s/.test(para[0])) p.classList.add('md-h');
    target().appendChild(p);
    para.length = 0;
  };
  const flushCode = () => {
    const pre = document.createElement('pre');
    pre.textContent = code.join('\n');
    target().appendChild(pre);
    code.length = 0;
  };

  for (const raw of String(text).split('\n')) {
    const line = raw.trimEnd();
    // фенс — только строка, начинающаяся с ``` (упоминание ``` в тексте — не фенс)
    if (/^\s*```/.test(line)) {
      if (inCode) flushCode();
      else { flushPara(); ul = null; }
      inCode = !inCode;
      continue;
    }
    if (inCode) { code.push(raw); continue; }

    // `★ Insight ───` → свёрнутая Insight-строка (раскрывается кликом)
    const co = line.match(/^\s*`?\s*[★✦☆]\s*([^─━`]*?)\s*[─━]{3,}\s*`?\s*$/);
    if (co) {
      flushPara();
      ul = null;
      closeCallout(); // вложенных Insight не бывает — закрываем предыдущий
      callout = document.createElement('div');
      callout.className = 'callout';
      const title = document.createElement('div');
      title.className = 'callout-title';
      title.appendChild(svgEl('svg', { width: '11', height: '11', viewBox: '0 0 12 12', fill: 'none', class: 'istar' }))
        .appendChild(svgEl('path', { d: 'M6 1 L7.3 4.4 L11 4.6 L8.1 6.9 L9.1 10.4 L6 8.4 L2.9 10.4 L3.9 6.9 L1 4.6 L4.7 4.4 Z', stroke: 'currentColor', 'stroke-width': '1.1', 'stroke-linejoin': 'round' }));
      calloutLabel = document.createElement('span');
      calloutLabel.textContent = co[1].trim() || 'Insight';
      title.appendChild(calloutLabel);
      const chev = svgEl('svg', { width: '9', height: '9', viewBox: '0 0 10 10', fill: 'none', class: 'ichev' });
      chev.appendChild(svgEl('path', { d: 'M3 2 L7 5 L3 8', stroke: 'currentColor', 'stroke-width': '1.4', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }));
      title.appendChild(chev);
      title.addEventListener('click', () => title.parentElement.classList.toggle('open'));
      callout.appendChild(title);
      calloutBody = document.createElement('div');
      calloutBody.className = 'callout-body';
      callout.appendChild(calloutBody);
      root.appendChild(callout);
      continue;
    }
    // линия из ─ : закрытие Insight либо просто разделитель
    if (/^\s*`?\s*[─━]{3,}\s*`?\s*$/.test(line)) {
      flushPara();
      ul = null;
      if (callout) closeCallout();
      else root.appendChild(Object.assign(document.createElement('div'), { className: 'md-hr' }));
      continue;
    }

    if (!line.trim()) { flushPara(); ul = null; continue; }
    const m = line.match(/^\s*[-*•]\s+(.*)$/);
    if (m) {
      flushPara();
      if (!ul) { ul = document.createElement('ul'); target().appendChild(ul); }
      const li = document.createElement('li');
      renderInline(li, m[1]);
      ul.appendChild(li);
      continue;
    }
    ul = null;
    para.push(line);
  }
  flushPara();
  if (inCode && code.length) flushCode(); // незакрытый фенс — дорендерим как код
  closeCallout(); // Insight без замыкающей линии ─ — финализируем счётчиком
}

/* --- лента чата: подряд идущие тулзы группируются в чипы, повторы ×N --- */

let toolsGroup = null; // текущая группа чипов (обнуляется текстовой репликой)

// имя тула (англ., как в транскрипте) → [русский глагол, иконка]
const TOOL_VERB = {
  edit: ['изменил', 'pencil'], multiedit: ['изменил', 'pencil'],
  notebookedit: ['изменил', 'pencil'], update: ['изменил', 'pencil'],
  write: ['создал', 'pencil'],
  read: ['читал', 'doc'], notebookread: ['читал', 'doc'],
  bash: ['выполнил', 'term'],
  grep: ['искал', 'search'], glob: ['искал', 'search'],
  search: ['искал', 'search'], websearch: ['искал', 'search'],
  webfetch: ['загрузил', 'globe'], fetch: ['загрузил', 'globe'],
  task: ['запустил', 'spark'], agent: ['запустил', 'spark'],
  todowrite: ['обновил план', 'check'],
  taskcreate: ['задача', 'check'], taskupdate: ['задача', 'check'], taskget: ['задача', 'check'],
};

// маленькие inline-иконки тулов (через DOM — без innerHTML)
const TOOL_ICON_PATHS = {
  pencil: ['M8.5 1.5 L10.5 3.5 L4 10 L1.5 10.5 L2 8 Z'],
  doc: ['M3 1.5 H7 L9 3.5 V10.5 H3 Z', 'M7 1.5 V3.5 H9'],
  term: ['M2 3 L4.5 6 L2 9', 'M6 9.2 H10'],
  search: ['M9.5 9.5 L7 7'],
  globe: ['M1.5 6 H10.5', 'M6 1.5 C 3.2 3.6 3.2 8.4 6 10.5 C 8.8 8.4 8.8 3.6 6 1.5 Z'],
  spark: ['M6 1 L7.3 4.4 L11 4.6 L8.1 6.9 L9.1 10.4 L6 8.4 L2.9 10.4 L3.9 6.9 L1 4.6 L4.7 4.4 Z'],
  check: ['M2.5 6.5 L5 9 L9.5 3'],
};
function toolIcon(kind) {
  const svg = svgEl('svg', { width: '11', height: '11', viewBox: '0 0 12 12', fill: 'none' });
  if (kind === 'search') svg.appendChild(svgEl('circle', { cx: '5', cy: '5', r: '3.2', stroke: 'currentColor', 'stroke-width': '1.2' }));
  if (kind === 'globe') svg.appendChild(svgEl('circle', { cx: '6', cy: '6', r: '4.5', stroke: 'currentColor', 'stroke-width': '1.2' }));
  for (const d of TOOL_ICON_PATHS[kind] || []) {
    svg.appendChild(svgEl('path', { d, stroke: 'currentColor', 'stroke-width': '1.2', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }));
  }
  return svg;
}

// "Edit · index.html" → { tool:'Edit', arg:'index.html' }
function toolParts(label) {
  const i = label.indexOf(' · ');
  return i < 0 ? { tool: label, arg: '' } : { tool: label.slice(0, i), arg: label.slice(i + 3) };
}

function bumpCount(chip) {
  const n = (Number(chip.dataset.count) || 1) + 1;
  chip.dataset.count = String(n);
  let c = chip.querySelector('.tcount');
  if (!c) { c = document.createElement('span'); c.className = 'tcount'; chip.appendChild(c); }
  c.textContent = `×${n}`;
}

function addToolChip(label) {
  if (!toolsGroup) {
    toolsGroup = document.createElement('div');
    toolsGroup.className = 'msg tools';
    chatlogEl.appendChild(toolsGroup);
  }
  const last = toolsGroup.lastElementChild;
  if (last && last.dataset.label === label) { bumpCount(last); return; }

  const { tool, arg } = toolParts(label);
  const [verb, icon] = TOOL_VERB[tool.toLowerCase()] || [tool, null];

  const chip = document.createElement('span');
  chip.className = 'chip';
  chip.dataset.label = label;
  chip.title = label;
  if (icon) chip.appendChild(toolIcon(icon));
  const v = document.createElement('span');
  v.className = 'tverb';
  v.textContent = verb;
  chip.appendChild(v);
  if (arg) {
    const a = document.createElement('span');
    a.className = 'targ';
    a.textContent = arg;
    chip.appendChild(a);
  }
  toolsGroup.appendChild(chip);
}

// epoch-мс → HH:MM локального времени (для метки времени над репликой)
function fmtClock(ts) {
  if (!ts) return '';
  const d = new Date(ts);
  if (isNaN(d)) return '';
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

// вытащить вложения юзера в чипы: [Image #N] и @path-упоминания файлов.
// Возвращает { chips:[{type,label}], text } — text без вынесенных маркеров.
function extractAttachments(raw) {
  const chips = [];
  let text = String(raw);
  text = text.replace(/\[Image #(\d+)\]/g, (_, n) => { chips.push({ type: 'image', label: `Image #${n}` }); return ''; });
  // @file: только если выглядит как путь (есть точка или слэш) — не трогаем @everyone и т.п.
  text = text.replace(/(^|\s)@([\w./\-]*[./][\w./\-]*)/g, (m, pre, p) => { chips.push({ type: 'file', label: `@${p}` }); return pre; });
  return { chips, text: text.replace(/[ \t]{2,}/g, ' ').trim() };
}

// inline-SVG картинки, собранный через DOM (без innerHTML — политика файла)
const SVGNS = 'http://www.w3.org/2000/svg';
function svgEl(tag, attrs) {
  const e = document.createElementNS(SVGNS, tag);
  for (const k in attrs) e.setAttribute(k, attrs[k]);
  return e;
}
function makeImageIcon() {
  const svg = svgEl('svg', { width: '11', height: '11', viewBox: '0 0 12 12', fill: 'none' });
  svg.appendChild(svgEl('rect', { x: '1', y: '1.5', width: '10', height: '9', rx: '1.5', stroke: 'currentColor', 'stroke-width': '1.2' }));
  svg.appendChild(svgEl('circle', { cx: '4', cy: '4.6', r: '1', fill: 'currentColor' }));
  svg.appendChild(svgEl('path', { d: 'M1.5 9 L4.5 6.4 L7 8.6 L8.6 7.2 L10.5 9', stroke: 'currentColor', 'stroke-width': '1.2', 'stroke-linejoin': 'round' }));
  return svg;
}

function userBubble(rawText) {
  const { chips, text } = extractAttachments(rawText);
  const bubble = document.createElement('div');
  bubble.className = 'bubble';
  if (chips.length) {
    const wrap = document.createElement('div');
    wrap.className = 'attachs';
    for (const c of chips) {
      const chip = document.createElement('div');
      chip.className = 'attach';
      if (c.type === 'image') chip.appendChild(makeImageIcon());
      const lbl = document.createElement('span');
      lbl.textContent = c.label;
      chip.appendChild(lbl);
      wrap.appendChild(chip);
    }
    bubble.appendChild(wrap);
  }
  // если после выноса вложений остался текст — отдельным блоком под чипами
  if (text || !chips.length) {
    const body = document.createElement('div');
    body.textContent = text;
    bubble.appendChild(body);
  }
  return bubble;
}

function assistantMsg(it) {
  const msg = document.createElement('div');
  msg.className = 'msg assistant';
  const head = document.createElement('div');
  head.className = 'mhead';
  const who = document.createElement('span');
  who.className = 'mwho';
  who.textContent = 'Jarvis';
  head.appendChild(who);
  const tm = fmtClock(it.ts);
  if (tm) {
    const t = document.createElement('span');
    t.className = 'mtime';
    t.textContent = tm;
    head.appendChild(t);
  }
  msg.appendChild(head);
  const bubble = document.createElement('div');
  bubble.className = 'bubble';
  renderMarkdown(bubble, it.text);
  msg.appendChild(bubble);
  return msg;
}

// оптимистично показанные ответы юзера, ждут «эха» из транскрипта (для дедупа)
let pendingReplies = [];

// сразу показать отправленную реплику в ленте — иначе при занятой сессии она
// уходит в очередь Claude и в чате до обработки не видна («ничего не происходит»)
function appendPendingReply(text, queued) {
  chatlogEl.querySelector('.chatempty')?.remove();
  toolsGroup = null;
  const msg = document.createElement('div');
  msg.className = 'msg user pending';
  msg.appendChild(userBubble(text));
  const st = document.createElement('div');
  st.className = 'msg-status';
  st.textContent = queued ? 'в очереди — доставлю, как освободится' : 'отправлено';
  msg.appendChild(st);
  chatlogEl.appendChild(msg);
  chatlogEl.scrollTop = chatlogEl.scrollHeight;
  pendingReplies.push({ text: text.trim(), el: msg });
}

function appendChatItems(items) {
  const nearBottom =
    chatlogEl.scrollHeight - chatlogEl.scrollTop - chatlogEl.clientHeight < 60;
  chatlogEl.querySelector('.chatempty')?.remove();
  for (const it of items) {
    if (it.kind === 'tool') {
      addToolChip(it.text);
      continue;
    }
    toolsGroup = null;
    if (it.role === 'user') {
      // реальная реплика из транскрипта пришла — снимаем оптимистичный дубль
      const pi = pendingReplies.findIndex((p) => p.text === it.text.trim());
      if (pi >= 0) { pendingReplies[pi].el.remove(); pendingReplies.splice(pi, 1); }
      const msg = document.createElement('div');
      msg.className = 'msg user';
      msg.appendChild(userBubble(it.text));
      chatlogEl.appendChild(msg);
    } else {
      chatlogEl.appendChild(assistantMsg(it));
    }
  }
  if (items.length && nearBottom) chatlogEl.scrollTop = chatlogEl.scrollHeight;
}

function updateChatChannelMark() {
  const s = state.find((x) => x.id === chatSessionId);
  // модель — бейдж рядом с именем проекта (как в строке списка)
  const model = s && (s.model || s.agent);
  chatModelEl.textContent = model || '';
  chatModelEl.hidden = !model;
  // tmux-сессии — без пометки; вне tmux помечаем
  chatChannelEl.hidden = !s || !!s.tmuxPane;
  // статус-точка справа — цвет по состоянию, пульс если работает
  chatDotEl.className = `chatdot ${s ? s.status : ''}`;
  // правый край: расход сессии (или ветка, пока usage грузится) — тихо, моно
  const subEl = document.getElementById('chatSub');
  subEl.textContent = s && s.branch ? `⎇ ${s.branch}` : '';
  if (s) {
    window.jarvis.getSessionUsage(s.id).then((us) => {
      if (!us || chatSessionId !== s.id) return;
      const money = (us.billing && us.billing !== 'plan') ? `$${us.cost.toFixed(2)}` : `~$${us.cost.toFixed(2)}`;
      subEl.textContent = `${fmtTok(us.tok)} ткн · ${money}`;
    }).catch(() => {});
  }
  gateReply(s);
  updateChatStatus(s);
  renderTaskBoard(s);
  renderVarBtn(s);
}

const tmuxHintEl = document.getElementById('tmuxHint');
const chatStatusEl = document.getElementById('chatStatus');

const MODELS = [['fable', 'Fable'], ['opus', 'Opus'], ['sonnet', 'Sonnet'], ['haiku', 'Haiku']];

// базовые уровни приезжают из `claude --help` через демона (не отстаём от CLI);
// ultracode в help не публикуется — это знание с живого слайдера Fable/Opus
let effortBase = ['low', 'medium', 'high', 'xhigh', 'max'];
window.jarvis.getMeta().then((m) => {
  if (m && Array.isArray(m.effortLevels) && m.effortLevels.length) effortBase = m.effortLevels;
}).catch(() => {});

const EFFORT_SHORT = { medium: 'med' };

function effortsFor(model) {
  const m = (model || '').toLowerCase();
  const list = [['auto', 'auto'], ...effortBase.map((l) => [l, EFFORT_SHORT[l] || l])];
  if (m === 'fable' || m === 'opus') list.push(['ultracode', 'ultracode']);
  return list;
}

// вне tmux ответ недоступен — гасим поле и показываем, что запустить
function gateReply(s) {
  const isTmux = !!(s && s.tmuxPane);
  tmuxHintEl.hidden = !s || isTmux;
  replyEl.disabled = !isTmux;
  replyEl.placeholder = isTmux ? 'Ответить агенту…  ( / — команды )' : 'Сессия вне tmux';
  if (!s || isTmux) return;
  tmuxHintEl.textContent = '';
  tmuxHintEl.appendChild(document.createTextNode('Сессия не в tmux — управлять из Jarvis нельзя. Запусти в терминале: '));
  const code = document.createElement('code');
  code.className = 'tmuxcmd';
  code.textContent = `claude --resume ${s.id}`;
  code.title = 'Скопировать';
  code.addEventListener('click', () => {
    navigator.clipboard?.writeText(`claude --resume ${s.id}`);
    showToast('Скопировано');
  });
  tmuxHintEl.appendChild(code);
  tmuxHintEl.appendChild(document.createTextNode(' — shim подхватит её в tmux.'));
}

// индикатор: думает / выполняет тул / генерирует / ждёт
function updateChatStatus(s) {
  chatStatusEl.textContent = '';
  if (!s || s.status === 'idle' || s.status === 'done') { chatStatusEl.hidden = true; return; }
  if (s.status === 'working') {
    chatStatusEl.className = 'chatstatus working';
    const d = s.detail || '';
    if (d.startsWith('▸')) {
      chatStatusEl.appendChild(document.createTextNode(`выполняет: ${d.slice(1).trim()}`));
    } else {
      chatStatusEl.appendChild(document.createTextNode('думает и генерирует ответ'));
      const dots = document.createElement('span');
      dots.className = 'dots';
      chatStatusEl.appendChild(dots);
    }
    chatStatusEl.hidden = false;
  } else if (s.status === 'waiting') {
    chatStatusEl.className = 'chatstatus waiting';
    chatStatusEl.appendChild(document.createTextNode('ждёт твоего ответа'));
    chatStatusEl.hidden = false;
  } else if (s.status === 'limit') {
    chatStatusEl.className = 'chatstatus waiting';
    chatStatusEl.appendChild(document.createTextNode(
      limitInfo && limitInfo.active
        ? `упёрлись в лимит · сброс через ${Math.max(0, Math.round((limitInfo.resetAt - Date.now()) / 60000))}м · продолжу сам`
        : 'упёрлись в лимит провайдера',
    ));
    chatStatusEl.hidden = false;
  } else {
    chatStatusEl.hidden = true;
  }
}

/* ---------- экран вопроса AskUserQuestion (клавиатурный пикер) ---------- */

const qviewEl = document.getElementById('qview');
const qOptsEl = document.getElementById('qOpts');
const qHeaderEl = document.getElementById('qHeader');
const qTitleEl = document.getElementById('qTitle');
const qFootEl = document.getElementById('qFoot');
let qSessionId = null;
let qData = null; // первый вопрос опроса
let qSel = 0;
let qChosen = new Set();

function keycap(text) {
  const k = document.createElement('span');
  k.className = 'keycap';
  k.textContent = text;
  return k;
}

function openQuestion(s) {
  const q = s.question.questions[0];
  qSessionId = s.id;
  qData = q;
  qSel = 0;
  qChosen = new Set();
  setView('question');
  renderQuestion();
  qOptsEl.focus?.();
}

let activeQOpts = qOptsEl; // контейнер опций: полноэкранный qview или слайд-овер вариантов

function paintQOptions() {
  for (const [i, btn] of [...activeQOpts.children].entries()) {
    btn.classList.toggle('sel', i === qSel);
    btn.classList.toggle('chosen', qChosen.has(i + 1));
  }
  activeQOpts.children[qSel]?.scrollIntoView({ block: 'nearest' });
}

// Рендер списка вариантов и подсказок в заданные контейнеры — общий для
// полноэкранного экрана вопроса и слайд-овера вариантов поверх чата.
function renderQOpts(optsEl, footEl) {
  optsEl.textContent = '';
  qData.options.forEach((o, i) => {
    const btn = document.createElement('div');
    btn.className = 'qopt';
    const num = document.createElement('span');
    num.className = 'qnum';
    num.textContent = String(i + 1);
    const body = document.createElement('span');
    body.className = 'qbody';
    const label = document.createElement('span');
    label.className = 'qlabel';
    label.textContent = o.label;
    body.appendChild(label);
    if (o.description) {
      const desc = document.createElement('span');
      desc.className = 'qdesc';
      desc.textContent = o.description;
      body.appendChild(desc);
    }
    btn.append(num, body);
    if (qData.multiSelect) {
      const ck = document.createElement('span');
      ck.className = 'qcheck';
      ck.textContent = '✓';
      btn.appendChild(ck);
    }
    btn.addEventListener('mouseenter', () => { qSel = i; paintQOptions(); });
    btn.addEventListener('click', () => { qSel = i; activateQ(); });
    optsEl.appendChild(btn);
  });
  paintQOptions();

  footEl.textContent = '';
  const hint = (cap, text) => {
    const h = document.createElement('span');
    h.appendChild(keycap(cap));
    h.appendChild(document.createTextNode(text));
    return h;
  };
  footEl.appendChild(hint('↑↓', 'выбрать'));
  if (qData.multiSelect) {
    footEl.appendChild(hint('␣', 'отметить'));
    footEl.appendChild(hint('↵', 'отправить'));
  } else {
    footEl.appendChild(hint('↵', 'ответить'));
    footEl.appendChild(hint('1–9', 'быстрый выбор'));
  }
  footEl.appendChild(hint('esc', 'назад'));
}

function renderQuestion() {
  qHeaderEl.textContent = qData.header || '';
  qHeaderEl.hidden = !qData.header;
  qTitleEl.textContent = qData.question;
  activeQOpts = qOptsEl;
  renderQOpts(qOptsEl, qFootEl);
}

function toggleQ(i) {
  const n = i + 1;
  if (qChosen.has(n)) qChosen.delete(n);
  else qChosen.add(n);
  paintQOptions();
}

function activateQ() {
  if (qData.multiSelect) toggleQ(qSel);
  else submitQ();
}

async function submitQ() {
  const indices = qData.multiSelect
    ? [...qChosen].sort((a, b) => a - b)
    : [qSel + 1];
  if (!indices.length) { showToast('Отметь хотя бы один вариант'); return; }
  const sid = qSessionId;
  const res = await window.jarvis.answerQuestion(sid, { indices, multiSelect: qData.multiSelect });
  if (res.ok) {
    if (varOpen) closeVarPanel(); // отвечали из слайд-овера — закрываем, остаёмся в чате
    else { setView('list'); render(); }
  } else showToast(res.error || 'Не удалось ответить');
}

document.getElementById('qBack').addEventListener('click', () => { setView('list'); render(); });

async function openChat(sessionId, project) {
  const res = await window.jarvis.openChat(sessionId);
  if (!res.ok) { showToast(res.error || 'Не удалось открыть чат'); return; }
  chatSessionId = sessionId;
  chatTitleEl.textContent = res.project || project || '';
  boardExpanded = 0;
  closeBoard(); // доска прошлого чата не должна оставаться открытой
  closeVarPanel(); // и слайд-овер вариантов прошлого чата
  setChatMode('dialog'); // всегда открываем чат в режиме диалога
  updateChatChannelMark();
  chatlogEl.textContent = '';
  toolsGroup = null;
  pendingReplies = []; // оптимистичные реплики прошлого чата не тащим в новый
  replyEl.value = ''; // черновик прошлого чата не должен уехать в этот
  hidePalette();
  loadCommands();
  setView('chat');
  replyEl.focus();
  if (res.items.length) {
    appendChatItems(res.items);
    chatlogEl.scrollTop = chatlogEl.scrollHeight;
  } else {
    const empty = document.createElement('div');
    empty.className = 'chatempty';
    empty.textContent = 'Пока пусто — новые реплики появятся здесь по мере работы агента.';
    chatlogEl.appendChild(empty);
  }
}

window.jarvis.onChatAppend(({ sessionId, items }) => {
  if (view === 'chat' && sessionId === chatSessionId) appendChatItems(items);
});

document.getElementById('chatBack').addEventListener('click', () => { closeBoard(); setView('list'); render(); });

/* ---------- доска задач (инкремент 6) ----------
 * ГРАНИЦА: панель ЧИТАЕТ доску из состояния сессии (источник — оркестратор) и
 * отображает её. Кнопки доски не мутируют доску — действие лишь префилит
 * composer текстом-инструкцией; доска меняется только на следующий TodoWrite. */

const tasksBtn = document.getElementById('tasksBtn');
const tasksBtnCount = document.getElementById('tasksBtnCount');
const tasksRingFg = document.getElementById('tasksRingFg');
const taskWrap = document.getElementById('taskWrap');
const tpListEl = document.getElementById('tpList');
const tpStripEl = document.getElementById('tpStrip');
const RING_C = 2 * Math.PI * 7; // = 43.98, радиус кольца 7

let boardOpen = false;
let boardExpanded = 0; // номер раскрытой задачи (0 — ни одной)

const boardOf = (s) => (s && s.board && s.board.tasks && s.board.tasks.length ? s.board : null);

// мс → компактная длительность: «42с» · «3м» · «1ч 12м»
function fmtDur(ms) {
  const sec = Math.max(0, Math.round(ms / 1000));
  if (sec < 60) return `${sec}с`;
  const m = Math.floor(sec / 60);
  if (m < 60) return `${m}м`;
  return `${Math.floor(m / 60)}ч ${m % 60}м`;
}

function renderTaskBoard(s) {
  const b = boardOf(s);
  if (!b) { tasksBtn.hidden = true; if (boardOpen) closeBoard(); return; }
  const total = b.tasks.length;
  const done = b.tasks.filter((t) => t.status === 'completed').length;
  tasksBtn.hidden = false;
  tasksBtnCount.textContent = `${done}/${total}`;
  tasksRingFg.style.strokeDashoffset = String(RING_C * (1 - (total ? done / total : 0)));
  tasksRingFg.setAttribute('stroke', b.stopped ? '#F2A33C' : '#41C98E'); // мёртвая доска — янтарь
  tasksBtn.classList.toggle('open', boardOpen);
  if (boardOpen) renderBoardPanel(s, b);
}

function openBoard() {
  const s = curSession();
  if (!boardOf(s)) return;
  boardOpen = true;
  taskWrap.hidden = false;
  tasksBtn.classList.add('open');
  renderBoardPanel(s, boardOf(s));
}

function closeBoard() {
  boardOpen = false;
  taskWrap.hidden = true;
  tasksBtn.classList.remove('open');
}

tasksBtn.addEventListener('click', () => (boardOpen ? closeBoard() : openBoard()));
document.getElementById('tpClose').addEventListener('click', closeBoard);
document.getElementById('taskScrim').addEventListener('click', closeBoard);
// Esc закрывает доску раньше, чем сработает «назад» (capture-фаза)
window.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && boardOpen) { e.preventDefault(); e.stopImmediatePropagation(); closeBoard(); }
}, true);

/* ---------- слайд-овер вариантов ответа (поверх чата, по образцу доски задач) ----------
 * Переиспользует рендер опций (renderQOpts) и логику ответа (submitQ/activateQ)
 * экрана вопроса; отличие — оверлей над чатом вместо отдельного view. */

const varBtn = document.getElementById('varBtn');
const varBtnLabel = document.getElementById('varBtnLabel');
const qWrap = document.getElementById('qWrap');
const qpOptsEl = document.getElementById('qpOpts');
const qpFootEl = document.getElementById('qpFoot');
const qpHeaderEl = document.getElementById('qpHeader');
const qpTitleEl = document.getElementById('qpTitle');
let varOpen = false;

const questionOf = (s) =>
  s && s.question && s.question.questions && s.question.questions.length ? s.question.questions[0] : null;

function renderVarBtn(s) {
  const q = questionOf(s);
  if (!q) { varBtn.hidden = true; if (varOpen) closeVarPanel(); return; }
  varBtn.hidden = false;
  const n = q.options.length;
  varBtnLabel.textContent = `${n} ${plural(n, 'вариант', 'варианта', 'вариантов')}`;
  varBtn.classList.toggle('open', varOpen);
  if (varOpen) renderVarPanel(s);
}

function renderVarPanel(s) {
  const q = questionOf(s);
  if (!q) { closeVarPanel(); return; }
  qData = q;
  qpHeaderEl.textContent = q.header || '';
  qpHeaderEl.hidden = !q.header;
  qpTitleEl.textContent = q.question;
  activeQOpts = qpOptsEl;
  renderQOpts(qpOptsEl, qpFootEl);
  if (q.multiSelect) { // мульти-выбор: клик-сабмит (на полноэкранном экране это Enter)
    const send = document.createElement('button');
    send.className = 'qp-send';
    send.textContent = 'Отправить';
    send.addEventListener('click', submitQ);
    qpFootEl.appendChild(send);
  }
}

function openVarPanel() {
  const s = curSession();
  const q = questionOf(s);
  if (!q) return;
  qSessionId = s.id;
  qData = q;
  qSel = 0;
  qChosen = new Set();
  varOpen = true;
  qWrap.hidden = false;
  varBtn.classList.add('open');
  replyEl.blur?.(); // освобождаем поле ввода — клавиши уходят пикеру
  renderVarPanel(s);
}

function closeVarPanel() {
  varOpen = false;
  qWrap.hidden = true;
  varBtn.classList.remove('open');
  activeQOpts = qOptsEl;
}

varBtn.addEventListener('click', () => (varOpen ? closeVarPanel() : openVarPanel()));
document.getElementById('qpClose').addEventListener('click', closeVarPanel);
document.getElementById('qScrim').addEventListener('click', closeVarPanel);

// Клавиатура слайд-овера — capture-фаза, чтобы перехватить раньше обработчиков чата
window.addEventListener('keydown', (e) => {
  if (!varOpen) return;
  if (e.key === 'Escape') { e.preventDefault(); e.stopImmediatePropagation(); closeVarPanel(); return; }
  if (!qData) return;
  const stop = () => { e.preventDefault(); e.stopImmediatePropagation(); };
  if (e.key === 'ArrowDown') { stop(); qSel = Math.min(qData.options.length - 1, qSel + 1); paintQOptions(); return; }
  if (e.key === 'ArrowUp') { stop(); qSel = Math.max(0, qSel - 1); paintQOptions(); return; }
  if (e.key === ' ') { stop(); if (qData.multiSelect) toggleQ(qSel); return; }
  if (e.key === 'Enter') { stop(); submitQ(); return; }
  if (/^[1-9]$/.test(e.key)) {
    const n = Number(e.key);
    if (n <= qData.options.length) { stop(); qSel = n - 1; activateQ(); }
  }
}, true);

// иконка статуса задачи (через DOM — без innerHTML)
function tpStatusIcon(status) {
  if (status === 'in_progress') {
    const sp = document.createElement('span');
    sp.className = 'tp-pulse';
    return sp;
  }
  const svg = svgEl('svg', { width: '15', height: '15', viewBox: '0 0 16 16', fill: 'none' });
  if (status === 'completed') {
    svg.appendChild(svgEl('circle', { cx: '8', cy: '8', r: '6.6', stroke: '#41C98E', 'stroke-width': '1.4' }));
    svg.appendChild(svgEl('path', { d: 'M5 8.2 L7.1 10.3 L11 5.8', stroke: '#41C98E', 'stroke-width': '1.5', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }));
  } else if (status === 'interrupted') {
    svg.appendChild(svgEl('circle', { cx: '8', cy: '8', r: '6.4', stroke: '#F2A33C', 'stroke-width': '1.4' }));
    svg.appendChild(svgEl('path', { d: 'M5.4 8 H10.6', stroke: '#F2A33C', 'stroke-width': '1.5', 'stroke-linecap': 'round' }));
  } else {
    // pending / очередь — пунктирное кольцо
    svg.appendChild(svgEl('circle', { cx: '8', cy: '8', r: '6.4', stroke: '#55555C', 'stroke-width': '1.4', 'stroke-dasharray': '2.5 2.5' }));
  }
  return svg;
}

// правый текст строки: модель · время / статус
function tpRight(t, stopped) {
  const parts = [];
  if (t.model) parts.push(t.model);
  if (t.status === 'completed' && t.durMs != null) parts.push(fmtDur(t.durMs));
  else if (t.status === 'in_progress') parts.push(stopped ? 'прервано' : (t.startedAt ? fmtDur(Date.now() - t.startedAt) : 'идёт'));
  else if (t.status === 'interrupted') parts.push('прервано');
  else if (t.status === 'pending') parts.push('в очереди');
  return parts.join(' · ');
}

// действия для задачи по её статусу. Готовой — ничего (просто заметка);
// активной/в очереди — перейти/пропустить; прерванной — снова перейти.
function tpActionsFor(status) {
  if (status === 'completed') return [];
  if (status === 'interrupted') return [['goto', 'Перейти']];
  return [['goto', 'Перейти'], ['skip', 'Пропустить']]; // pending / in_progress
}

function renderBoardPanel(s, b) {
  const total = b.tasks.length;
  const done = b.tasks.filter((t) => t.status === 'completed').length;
  const run = b.tasks.filter((t) => t.status === 'in_progress').length;
  const queued = b.tasks.filter((t) => t.status === 'pending').length;

  // шапка
  const cnt = document.getElementById('tpCount');
  cnt.textContent = String(done);
  const of = document.createElement('span');
  of.className = 'tp-of';
  of.textContent = `/${total}`;
  cnt.appendChild(of);
  document.getElementById('tpAggText').textContent =
    `выполнено · ${run} в работе · ${queued} в очереди` + (b.stopped ? ' · остановлена' : '');
  document.getElementById('tpBarFill').style.width = `${total ? Math.round((done / total) * 100) : 0}%`;
  const mins = Math.max(0, Math.floor((Date.now() - (s.createdAt || Date.now())) / 60000));
  document.getElementById('tpSub').textContent = `сессия · ${mins < 60 ? mins + 'м' : Math.floor(mins / 60) + 'ч ' + (mins % 60) + 'м'}`;

  // список задач
  tpListEl.textContent = '';
  for (const t of b.tasks) {
    const row = document.createElement('div');
    row.className = 'tp-row';

    // строка не раскрывается; заголовок пишем целиком (перенос по строкам)
    const main = document.createElement('div');
    main.className = 'tp-rowmain';
    const ic = document.createElement('span');
    ic.className = 'tp-ic';
    ic.appendChild(tpStatusIcon(t.status));
    main.appendChild(ic);
    const n = document.createElement('span');
    n.className = 'tp-n';
    n.textContent = `Task ${t.n}`;
    main.appendChild(n);
    const title = document.createElement('span');
    title.className = 'tp-title2' + (t.status === 'pending' ? ' dim' : '');
    title.textContent = t.text;
    main.appendChild(title);
    const right = document.createElement('span');
    right.className = 'tp-right';
    right.textContent = tpRight(t, b.stopped);
    main.appendChild(right);
    row.appendChild(main);

    // пульт goto/skip — всегда виден на активной задаче (без раскрытия);
    // на готовой кнопок нет. Префил composer, без отправки и без мутации доски.
    const actions = b.stopped ? [] : tpActionsFor(t.status);
    if (actions.length) {
      const acts = document.createElement('div');
      acts.className = 'tp-acts';
      for (const [action, label] of actions) {
        const btn = document.createElement('button');
        btn.className = 'tp-act';
        btn.textContent = label;
        btn.addEventListener('click', () => runTaskAction(t.n, action));
        acts.appendChild(btn);
      }
      row.appendChild(acts);
    }
    tpListEl.appendChild(row);
  }

  // полоска несопоставленных сабагентов
  const subs = b.subagents || [];
  if (!subs.length) {
    tpStripEl.hidden = true;
  } else {
    tpStripEl.hidden = false;
    tpStripEl.textContent = '';
    const lab = document.createElement('span');
    lab.className = 'tp-striplabel';
    lab.textContent = 'сабагенты: ';
    tpStripEl.appendChild(lab);
    const parts = subs.slice(0, 5).map((sa) => {
      const seg = [sa.kind || sa.name];
      if (sa.model) seg.push(sa.model);
      const dur = sa.stoppedAt ? sa.stoppedAt - sa.startedAt : Date.now() - sa.startedAt;
      seg.push(fmtDur(dur) + (sa.stoppedAt ? '' : '…'));
      return seg.join(' · ');
    });
    tpStripEl.appendChild(document.createTextNode(parts.join('   ·   ')));
  }
}

// действие с доски: получаем текст-инструкцию и ПРЕФИЛИМ composer (не шлём!)
async function runTaskAction(taskRef, action) {
  if (!chatSessionId) return;
  const res = await window.jarvis.taskAction(chatSessionId, taskRef, action);
  if (!res || !res.ok) { showToast((res && res.error) || 'Не вышло'); return; }
  closeBoard();
  replyEl.value = res.text;
  replyEl.focus();
  replyEl.setSelectionRange(replyEl.value.length, replyEl.value.length);
  showToast('Проверь и отправь — Jarvis не шлёт сам');
}

// живой посекундный отсчёт у in-progress задач, пока доска открыта
setInterval(() => {
  if (!boardOpen) return;
  const b = boardOf(curSession());
  if (b && !b.stopped && b.tasks.some((t) => t.status === 'in_progress')) {
    renderBoardPanel(curSession(), b);
  }
}, 1000);

/* ---------- заготовка: режим чата Диалог/Саммари (Часть 3) ----------
 * Пока ни к чему не привязано — просто визуальный переключатель и плейсхолдер
 * под будущий саммаризированный вид сессии. */
const chatModeSeg = document.getElementById('chatModeSeg');
const chatSummaryEl = document.getElementById('chatSummary');
let chatMode = 'dialog';

function setChatMode(m) {
  chatMode = m;
  for (const b of chatModeSeg.querySelectorAll('.chatmode-btn')) {
    b.classList.toggle('active', b.dataset.m === m);
  }
  chatlogEl.hidden = m !== 'dialog';
  chatSummaryEl.hidden = m !== 'summary';
}

chatModeSeg.addEventListener('click', (e) => {
  const m = e.target && e.target.dataset ? e.target.dataset.m : null;
  if (m) setChatMode(m);
});

/* ---------- палитра команд: / в поле ответа ---------- */

const cmdPaletteEl = document.getElementById('cmdPalette');
let cmdCatalog = [];
let paletteItems = []; // обобщённые пункты: {name, hint, desc, badge, active, apply}
let cmdSel = 0;

async function loadCommands() {
  if (!chatSessionId) { cmdCatalog = []; return; }
  try { cmdCatalog = await window.jarvis.getCommands(chatSessionId); }
  catch { cmdCatalog = []; }
}

function curSession() { return state.find((x) => x.id === chatSessionId); }

function srcLabel(src) {
  return { builtin: 'встр', project: 'проект', user: 'мои', plugin: 'плагин' }[src] || '';
}

// /model и /effort без значения → свой пикер; иначе автокомплит команд
function refreshPalette() {
  const v = replyEl.value;
  if (/^\/model\s*$/i.test(v)) return buildValuePicker('model');
  if (/^\/effort\s*$/i.test(v)) return buildValuePicker('effort');
  if (!v.startsWith('/') || /\s/.test(v.slice(1))) { hidePalette(); return; }
  buildCmdItems(v.slice(1).toLowerCase());
}

function buildCmdItems(q) {
  const matches = cmdCatalog
    .filter((c) => c.name.toLowerCase().includes(q))
    .sort((a, b) => {
      const ap = a.name.toLowerCase().startsWith(q) ? 0 : 1;
      const bp = b.name.toLowerCase().startsWith(q) ? 0 : 1;
      if (ap !== bp) return ap - bp;
      if ((a.source === 'builtin') !== (b.source === 'builtin')) return a.source === 'builtin' ? -1 : 1;
      return a.name.localeCompare(b.name);
    })
    .slice(0, 50);
  paletteItems = matches.map((c) => ({
    name: '/' + c.name,
    hint: c.hint,
    desc: c.description || '',
    badge: srcLabel(c.source),
    apply: () => completeCommand(c),
  }));
  cmdSel = 0;
  paintPalette();
}

// пикер значений модели/effort вместо интерактивного слайдера TUI
function buildValuePicker(kind) {
  const s = curSession();
  if (kind === 'model') {
    const cur = ((s && s.model) || '').toLowerCase();
    paletteItems = MODELS.map(([val, label]) => ({
      name: label, desc: 'модель сессии', active: cur === label.toLowerCase(),
      apply: () => applyValue('setModel', val),
    }));
  } else {
    paletteItems = effortsFor(s && s.model).map(([val, label]) => ({
      name: label, desc: 'уровень рассуждения', active: !!(s && s.effort === val),
      apply: () => applyValue('setEffort', val),
    }));
  }
  cmdSel = Math.max(0, paletteItems.findIndex((i) => i.active));
  paintPalette();
}

async function applyValue(method, val) {
  if (!chatSessionId) return;
  const res = await window.jarvis[method](chatSessionId, val);
  replyEl.value = '';
  hidePalette();
  if (!res.ok) showToast(res.error || (res.needsTmux ? 'Сессия вне tmux' : 'Не удалось'));
  else replyEl.focus();
}

function paintPalette() {
  if (!paletteItems.length) { hidePalette(); return; }
  cmdPaletteEl.hidden = false;
  cmdPaletteEl.textContent = '';
  paletteItems.forEach((it, i) => {
    const row = document.createElement('div');
    row.className = 'cmdrow-item' + (i === cmdSel ? ' sel' : '');

    const name = document.createElement('span');
    name.className = 'cmdname';
    name.textContent = it.name;
    row.appendChild(name);

    if (it.hint) {
      const hint = document.createElement('span');
      hint.className = 'cmdhint';
      hint.textContent = it.hint;
      row.appendChild(hint);
    }
    if (it.active) {
      const ck = document.createElement('span');
      ck.className = 'cmdhint';
      ck.textContent = '✓ сейчас';
      row.appendChild(ck);
    }

    const desc = document.createElement('span');
    desc.className = 'cmddesc';
    desc.textContent = it.desc || '';
    row.appendChild(desc);

    if (it.badge) {
      const b = document.createElement('span');
      b.className = 'cmdsrc';
      b.textContent = it.badge;
      row.appendChild(b);
    }

    row.addEventListener('mouseenter', () => { cmdSel = i; paintPalette(); });
    row.addEventListener('click', () => it.apply());
    cmdPaletteEl.appendChild(row);
  });
  cmdPaletteEl.children[cmdSel]?.scrollIntoView({ block: 'nearest' });
}

function hidePalette() {
  cmdPaletteEl.hidden = true;
  paletteItems = [];
}

function paletteOpen() {
  return !cmdPaletteEl.hidden && paletteItems.length > 0;
}

// команда: с подсказкой — подставить имя (model/effort → откроется пикер), иначе отправить
function completeCommand(c) {
  if (c.hint) {
    replyEl.value = '/' + c.name + ' ';
    refreshPalette();
    replyEl.focus();
  } else {
    replyEl.value = '/' + c.name;
    hidePalette();
    sendReplyNow();
  }
}

/* ---------- отправка ответа: tmux-вставка или claude -p --resume ---------- */

let sending = false;
async function sendReplyNow() {
  const text = replyEl.value.trim();
  if (!text || sending || !chatSessionId) return;
  sending = true;
  replyEl.disabled = true;
  try {
    const res = await window.jarvis.sendReply(chatSessionId, text);
    if (res.ok) {
      replyEl.value = '';
      appendPendingReply(text, !!res.queued); // сразу видно в ленте (снимется эхом из транскрипта)
    } else if (res.needsTmux) {
      showToast('Сессия вне tmux — запусти команду из подсказки ниже');
    } else {
      showToast(res.error || 'Не удалось отправить');
    }
  } finally {
    sending = false;
    replyEl.disabled = false;
    replyEl.focus();
  }
}

replyEl.addEventListener('input', refreshPalette);

replyEl.addEventListener('keydown', (e) => {
  if (e.metaKey) return; // ⌘↵ — в терминал, обрабатывается глобально
  if (paletteOpen()) {
    if (e.key === 'ArrowDown') { e.preventDefault(); e.stopPropagation(); cmdSel = Math.min(paletteItems.length - 1, cmdSel + 1); paintPalette(); return; }
    if (e.key === 'ArrowUp') { e.preventDefault(); e.stopPropagation(); cmdSel = Math.max(0, cmdSel - 1); paintPalette(); return; }
    if (e.key === 'Tab' || e.key === 'Enter') { e.preventDefault(); e.stopPropagation(); paletteItems[cmdSel] && paletteItems[cmdSel].apply(); return; }
    if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); hidePalette(); return; }
  }
  if (e.key === 'Enter') {
    e.preventDefault();
    e.stopPropagation();
    sendReplyNow();
  }
});

/* ---------- переход к терминалу ---------- */

async function focusTerminal(sessionId, project) {
  const res = await window.jarvis.focusTerminal(sessionId);
  if (res.ok) { window.jarvis.hidePanel(); return; }
  // нижняя ступень лесенки — не ошибка, а чат сессии прямо в панели
  if (res.fallbackChat && view !== 'chat') openChat(sessionId, project);
  else showToast(res.error || 'Не нашёл терминал');
}

/* ---------- состояние от демона ---------- */

window.jarvis.onState((list) => {
  state = list;
  render();
  if (view === 'chat') updateChatChannelMark();
  if (view === 'question') {
    const s = state.find((x) => x.id === qSessionId);
    if (!s || !s.question) { setView('list'); render(); } // ответили в терминале — выходим
  }
});
window.jarvis.getState().then((list) => { state = list; rebuildOrder(); render(); });

/* ---------- лимит-баннер ---------- */

const limitBannerEl = document.getElementById('limitBanner');
let limitInfo = null;

function paintLimitBanner() {
  if (!limitInfo || !limitInfo.active) { limitBannerEl.hidden = true; return; }
  const min = Math.max(0, Math.round((limitInfo.resetAt - Date.now()) / 60000));
  const t = min < 60 ? `${min}м` : `${Math.floor(min / 60)}ч ${min % 60}м`;
  limitBannerEl.textContent =
    `Claude${limitInfo.plan ? ` ${limitInfo.plan}` : ''} · лимит использования · сброс через ${t} — сессии продолжатся сами`;
  limitBannerEl.hidden = false;
}

window.jarvis.onLimitState((l) => { limitInfo = l; paintLimitBanner(); });
window.jarvis.getLimit().then((l) => { limitInfo = l; paintLimitBanner(); }).catch(() => {});
setInterval(paintLimitBanner, 30000); // тикаем обратный отсчёт

/* ---------- плагины: Не спать (☕) и Крышка (⌒) ---------- */

let plugins = [];
let awakeLive = false; // активен таймер «Не спать» → нужен посекундный тик отсчёта

// посекундный отсчёт в карточке бодрости — только когда настройки открыты
// и реально тикает таймер (не жжём кадры впустую)
setInterval(() => {
  if (awakeLive && view === 'settings') renderPluginRows();
}, 1000);

const pluginById = (id) => plugins.find((p) => p.id === id);

// хвост футера: статус обоих режимов ВСЕГДА виден (вкл и выкл),
// как Current Session Details у Amphetamine
function powerSuffix() {
  const parts = [];
  const ka = pluginById('keep-awake');
  if (ka?.enabled) parts.push(ka.status?.active ? `☕ ${ka.status.line || 'вкл'}` : '☕ выкл');
  const cs = pluginById('clamshell');
  if (cs?.enabled) parts.push(cs.status?.armed ? '⌒ не уснёт закрытым' : '⌒ выкл');
  return parts.length ? ' · ' + parts.join(' · ') : '';
}

function footerText() {
  const base = state.length
    ? `${state.length} ${plural(state.length, 'сессия', 'сессии', 'сессий')} · демон активен`
    : 'демон активен';
  return base + powerSuffix();
}

function srow(label, control, { dim = false, hint = '', sub = false } = {}) {
  const row = document.createElement('div');
  row.className = sub ? 'srow sub' : 'srow';
  const lab = document.createElement('span');
  lab.className = 'slabel';
  lab.textContent = label;
  if (dim) lab.style.opacity = '0.6';
  row.appendChild(lab);
  if (hint) {
    const h = document.createElement('span');
    h.className = 'shint';
    h.textContent = hint;
    row.appendChild(h);
  }
  const sp = document.createElement('span');
  sp.className = 'spacer';
  row.appendChild(sp);
  row.appendChild(control);
  return row;
}

/** шапка режима: слева «☕ Не спать», справа состояние словами + точка.
 *  Состояние видно ВСЕГДА — и когда включено, и когда нет (как Current
 *  Session у Amphetamine), чтобы было ясно, держит мак сон или нет. */
function headRow(label, stateText, on, gap = false) {
  const row = document.createElement('div');
  row.className = gap ? 'srow blockgap' : 'srow';
  const lab = document.createElement('span');
  lab.className = 'slabel';
  lab.textContent = label;
  row.appendChild(lab);
  const sp = document.createElement('span');
  sp.className = 'spacer';
  row.appendChild(sp);
  const chip = document.createElement('span');
  chip.className = on ? 'sval on' : 'sval';
  chip.textContent = stateText;
  row.appendChild(chip);
  const dot = document.createElement('span');
  dot.className = on ? 'sdot on' : 'sdot';
  row.appendChild(dot);
  return row;
}

/** причина текущего состояния слева + ОДНА главная кнопка справа.
 *  Кнопка всегда делает очевидное: держит → «Выключить», не держит → «Включить». */
function actionRow(text, on, btnLabel, onClick) {
  const row = document.createElement('div');
  row.className = 'srow sub';
  const val = document.createElement('span');
  val.className = on ? 'sval on' : 'sval';
  val.textContent = text;
  row.appendChild(val);
  const sp = document.createElement('span');
  sp.className = 'spacer';
  row.appendChild(sp);
  const btn = document.createElement('button');
  btn.className = 'keycap kbig';
  btn.textContent = btnLabel;
  btn.addEventListener('click', onClick);
  row.appendChild(btn);
  return row;
}

/** «› тонкая настройка» — раскрывашка для редких опций, чтобы не маячили */
function discRow(open, onToggle) {
  const row = document.createElement('div');
  row.className = 'srow sub sdisc';
  const lab = document.createElement('span');
  lab.className = 'slabel';
  lab.textContent = (open ? '⌄' : '›') + ' тонкая настройка';
  row.appendChild(lab);
  row.addEventListener('click', onToggle);
  return row;
}

/** пресеты «включить на время» для «Не спать» */
function presetRow() {
  const row = document.createElement('div');
  row.className = 'srow sub';
  const lab = document.createElement('span');
  lab.className = 'slabel';
  lab.style.opacity = '0.6';
  lab.textContent = 'Включить на время';
  row.appendChild(lab);
  const sp = document.createElement('span');
  sp.className = 'spacer';
  row.appendChild(sp);
  for (const [min, label] of [[15, '15м'], [60, '1ч'], [240, '4ч']]) {
    const b = document.createElement('button');
    b.className = 'keycap';
    b.textContent = label;
    b.addEventListener('click', () => pluginCmd('keep-awake', 'start-timer', { minutes: min }));
    row.appendChild(b);
  }
  return row;
}

function stoggle(checked, onChange, disabled = false) {
  const t = document.createElement('input');
  t.type = 'checkbox';
  t.className = 'toggle';
  t.checked = !!checked;
  t.disabled = disabled;
  t.addEventListener('change', () => onChange(t.checked));
  return t;
}

async function pluginCmd(id, cmd, args) {
  const res = await window.jarvis.pluginCmd(id, cmd, args);
  if (res && res.ok === false && res.error) showToast(res.error);
  plugins = await window.jarvis.getPlugins();
  renderPluginRows();
  footerLeftEl.textContent = footerText();
}

// маленькие глифы для карточки бодрости (через DOM — без innerHTML)
function awakeGlyph(kind) {
  const svg = svgEl('svg', { width: '15', height: '15', viewBox: '0 0 16 16', fill: 'none' });
  const path = (d, w) => svg.appendChild(svgEl('path', { d, stroke: '#C8C8CE', 'stroke-width': String(w || 1.3), 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }));
  if (kind === 'coffee') {
    path('M3 6.5 H10.5 V9.5 A2.5 2.5 0 0 1 8 12 H5.5 A2.5 2.5 0 0 1 3 9.5 Z');
    path('M10.5 7 H12 A1.5 1.5 0 0 1 12 10 H10.5');
    svg.appendChild(svgEl('path', { d: 'M5.2 2.6 V4', stroke: '#76767E', 'stroke-width': '1.2', 'stroke-linecap': 'round' }));
    svg.appendChild(svgEl('path', { d: 'M8 2.6 V4', stroke: '#76767E', 'stroke-width': '1.2', 'stroke-linecap': 'round' }));
  } else if (kind === 'lid') {
    path('M2.5 10.5 Q8 4 13.5 10.5');
    svg.appendChild(svgEl('path', { d: 'M1.5 12 H14.5', stroke: '#76767E', 'stroke-width': '1.3', 'stroke-linecap': 'round' }));
  }
  return svg;
}

// остаток таймера → «59:44» / «3:59:44»
function fmtAwakeLeft(ms) {
  const t = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(t / 3600), m = Math.floor((t % 3600) / 60), s = t % 60;
  const p = (n) => String(n).padStart(2, '0');
  return h > 0 ? `${h}:${p(m)}:${p(s)}` : `${m}:${p(s)}`;
}

// длительности сегмента «Не спать»: id → подпись + команда плагину
const AWAKE_SEG = [
  { id: 'off', label: 'Выкл', run: () => pluginCmd('keep-awake', 'stop') },
  { id: '15m', label: '15м', run: () => pluginCmd('keep-awake', 'start-timer', { minutes: 15 }) },
  { id: '1h', label: '1ч', run: () => pluginCmd('keep-awake', 'start-timer', { minutes: 60 }) },
  { id: '4h', label: '4ч', run: () => pluginCmd('keep-awake', 'start-timer', { minutes: 240 }) },
  { id: 'inf', label: '∞', run: () => pluginCmd('keep-awake', 'start-manual') },
];
const TIMER_LABEL_TO_SEG = { '15м': '15m', '60м': '1h', '240м': '4h' };

// какой сегмент длительности подсвечен + текст статуса справа
function awakeState(st) {
  if (!st || !st.active) return { seg: 'off', active: false, status: 'спит как обычно' };
  const manual = st.manual;
  const kind = manual && manual.kind;
  if (kind === 'manual') return { seg: 'inf', active: true, status: 'не уснёт' };
  if (kind === 'timer') {
    const left = (manual.until || 0) - Date.now();
    return { seg: TIMER_LABEL_TO_SEG[manual.label] || '', active: true, status: 'ещё ' + fmtAwakeLeft(left), live: true };
  }
  // активна только авто-гранта (агенты работают) или процесс — длительность не выбрана
  return { seg: 'off', active: true, status: st.line || 'активно' };
}

// единая карточка «Бодрость»: статус+отсчёт, сегмент длительности,
// под-тогглы (авто / экран), крышка. Один источник истины — статусы плагинов.
function renderPluginRows() {
  const box = document.getElementById('awakeCard');
  if (!box) return;
  box.textContent = '';

  const ka = pluginById('keep-awake');
  const st = ka && ka.status;
  const a = awakeState(st);
  awakeLive = !!a.live; // нужен ли посекундный тик отсчёта

  // шапка
  const head = document.createElement('div');
  head.className = 'awakehead';
  head.appendChild(awakeGlyph('coffee'));
  const title = document.createElement('span');
  title.className = 'atitle';
  title.textContent = 'Не давать маку спать';
  head.appendChild(title);
  head.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
  if (a.active) {
    const pulse = document.createElement('span');
    pulse.className = 'apulse';
    head.appendChild(pulse);
  }
  const status = document.createElement('span');
  status.className = a.active ? 'astatus on' : 'astatus';
  status.textContent = a.status;
  head.appendChild(status);
  box.appendChild(head);

  // сегмент длительности
  const seg = document.createElement('div');
  seg.className = 'aseg';
  for (const o of AWAKE_SEG) {
    const b = document.createElement('button');
    b.className = 'asegbtn' + (a.seg === o.id ? ' active' : '') + (o.id === 'off' ? ' off' : '');
    b.textContent = o.label;
    b.addEventListener('click', o.run);
    seg.appendChild(b);
  }
  box.appendChild(seg);

  // под-тогглы
  box.appendChild(arow('Держать, пока работают агенты',
    stoggle(st && st.autoEnabled, (v) => pluginCmd('keep-awake', 'set', { auto: v })), { hairtop: true }));
  box.appendChild(arow('Не гасить заодно и экран',
    stoggle(st && st.keepDisplayOn, (v) => pluginCmd('keep-awake', 'set', { keepDisplayOn: v }))));

  // крышка (clamshell) — сегмент Спать / Не спать
  const cs = pluginById('clamshell');
  const armed = !!(cs && cs.status && cs.status.armed);
  const lid = document.createElement('div');
  lid.className = 'arow lid hairtop';
  lid.appendChild(awakeGlyph('lid'));
  const ll = document.createElement('span');
  ll.className = 'alabel';
  ll.textContent = 'При закрытой крышке';
  lid.appendChild(ll);
  lid.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
  const lidSeg = document.createElement('div');
  lidSeg.className = 'seg';
  for (const [val, label] of [['sleep', 'Спать'], ['keep', 'Не спать']]) {
    const b = document.createElement('button');
    b.className = 'segbtn' + ((val === 'keep') === armed ? ' active' : '');
    b.textContent = label;
    b.addEventListener('click', async () => {
      if (val === 'keep') {
        if (cs && !cs.enabled) await window.jarvis.pluginCmd('clamshell', '_enable', { on: true });
        pluginCmd('clamshell', 'arm');
      } else {
        pluginCmd('clamshell', 'disarm');
      }
    });
    lidSeg.appendChild(b);
  }
  lid.appendChild(lidSeg);
  box.appendChild(lid);

  // подсказка
  const hint = document.createElement('div');
  hint.className = 'ahint';
  hint.append('Быстрее: наберите ');
  const code = document.createElement('code');
  code.textContent = '/amf 1ч';
  hint.append(code, ' в поиске');
  box.appendChild(hint);
}

// строка карточки: подпись слева, контрол справа
function arow(label, control, { hairtop = false } = {}) {
  const row = document.createElement('div');
  row.className = 'arow' + (hairtop ? ' hairtop' : '');
  const lab = document.createElement('span');
  lab.className = 'alabel';
  lab.textContent = label;
  row.appendChild(lab);
  row.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
  row.appendChild(control);
  return row;
}

window.jarvis.onPlugins((list) => {
  plugins = list;
  footerLeftEl.textContent = footerText();
  renderPluginRows();
});
window.jarvis.getPlugins().then((list) => {
  plugins = list;
  footerLeftEl.textContent = footerText();
}).catch(() => {});

// клик по уведомлению: панель уже показана демоном — открываем чат сессии
window.jarvis.onOpenSession(async (id) => {
  if (!state.length) {
    try { state = await window.jarvis.getState(); rebuildOrder(); } catch {}
  }
  const s = state.find((x) => x.id === id);
  if (s) openSession(s);
});

// Анимация появления в стиле Raycast: scale(.98)→1 + fade, 120ms.
// Порядок строк пересобираем ТОЛЬКО здесь — при открытии панели, не во время просмотра.
window.jarvis.onShown(() => {
  // перезапуск входной анимации на каждый показ: снять класс → форс-рефлоу → вернуть.
  // keyframe стартует с opacity:0 (fill both держит 0 до показа окна), поэтому
  // реверс-fade и «моргание» исключены, даже если панель уже была видима.
  panelEl.classList.remove('entering');
  void panelEl.offsetWidth;
  panelEl.classList.add('entering');
  // Окно при скрытии не уничтожается — view и открытый чат живы. Возвращаем
  // на то же место (клик мимо / Cmd+J прячут панель как есть). Чат или вопрос
  // уже закрытой сессии (могла завершиться, пока панель была спрятана) —
  // мягко роняем в список.
  const sess = (id) => state.find((s) => s.id === id);
  const stale =
    (view === 'chat' && !sess(chatSessionId)) ||
    (view === 'question' && !sess(qSessionId)?.question);

  if (view === 'list' || stale) {
    queryEl.value = ''; // список открывается свежим: чистый поиск + фокус
    sel = 0;
    rebuildOrder();
    setView('list');
    render();
  }
});

queryEl.addEventListener('input', () => {
  if (view === 'history') { renderHistory(); return; }
  sel = 0;
  cmdRootSel = 0;
  if (!queryEl.value.trim().startsWith('/')) argMode = null;
  render();
});

/* ---------- палитра быстрых команд (/) ----------
 * Часть 2 спеки «команды и быстрые настройки в поиске». Триггер — «/» в
 * главном поиске. Парсер общий для строки и arg-полей; действия идут в тот же
 * IPC, что и карточка «Бодрость» / настройки (single source of truth). */

let cmdRootSel = 0;
let palHoverEnabled = true; // ховер-выбор отключается на время стрелочной навигации,
                            // иначе re-render под неподвижным курсором фолбэчит выбор назад
let argMode = null;       // null | 'amf' (Raycast-поля Часы/Минуты)
let argH = '';
let argM = '';
let argFocus = 'h';       // 'h' | 'm'
let palSettings = null;   // кэш {position, notifyDone, notifyWaiting} для подсветки чипов

const ROOT_COMMANDS = [
  { cmd: '/amf', kind: 'amf', glyph: 'coffee', desc: 'Не давать маку спать', args: 'выкл · 15м · 1ч · 4ч · ∞' },
  { cmd: '/lid', kind: 'lid', glyph: 'lid', desc: 'Поведение при закрытой крышке', args: 'спать · не спать' },
  { cmd: '/pos', kind: 'pos', glyph: 'pos', desc: 'Позиция панели', args: 'центр · угол' },
  { cmd: '/notify', kind: 'notify', glyph: 'bell', desc: 'Уведомления', args: 'вкл · выкл' },
];

// реальное движение мыши снова отдаёт выбор ховеру (после стрелок)
listEl.addEventListener('mousemove', () => { palHoverEnabled = true; });

// глиф команды (coffee/lid — из карточки бодрости; pos/bell — здесь)
function cmdGlyph(kind) {
  if (kind === 'coffee' || kind === 'lid') return awakeGlyph(kind);
  const svg = svgEl('svg', { width: '15', height: '15', viewBox: '0 0 16 16', fill: 'none' });
  const p = (d, w) => svg.appendChild(svgEl('path', { d, stroke: '#9A9AA2', 'stroke-width': String(w || 1.4), 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }));
  if (kind === 'pos') {
    p('M2.5 3.5 H13.5 V12.5 H2.5 Z');
    svg.appendChild(svgEl('circle', { cx: '11', cy: '6', r: '1.3', fill: '#9A9AA2' }));
  } else if (kind === 'bell') {
    p('M5 7 A3 3 0 0 1 11 7 V9.5 L12 11 H4 L5 9.5 Z');
    p('M6.7 11 A1.3 1.3 0 0 0 9.3 11', 1.2);
  }
  return svg;
}

function cmdMatches() {
  const q = queryEl.value.trim();
  if (q === '/') return ROOT_COMMANDS.slice();
  const tok = q.split(/\s+/)[0].toLowerCase();
  const m = ROOT_COMMANDS.filter((c) => c.cmd.startsWith(tok) || tok.startsWith(c.cmd));
  return m.length ? m : ROOT_COMMANDS.slice();
}

/* ----- парсеры (понимают рус/англ) ----- */
function amfArg(a) {
  a = a.toLowerCase();
  if (['off', 'выкл', '0', 'стоп'].includes(a)) return 'off';
  if (['15m', '15м', '15'].includes(a)) return '15m';
  if (['1h', '1ч', '1', '60'].includes(a)) return '1h';
  if (['4h', '4ч', '4'].includes(a)) return '4h';
  if (['inf', '∞', 'on', 'вкл', 'всегда', 'навсегда'].includes(a)) return 'inf';
  return null;
}

/* ----- исполнители: тот же IPC, что и карточка бодрости / настройки ----- */
function applyAmf(mode) {
  if (mode === 'off') pluginCmd('keep-awake', 'stop');
  else if (mode === 'inf') pluginCmd('keep-awake', 'start-manual');
  else pluginCmd('keep-awake', 'start-timer', { minutes: { '15m': 15, '1h': 60, '4h': 240 }[mode] });
  showToast(mode === 'off' ? 'Сон в обычном режиме' : mode === 'inf' ? 'Не сплю, пока не выключишь' : `Не сплю ещё ${{ '15m': '15м', '1h': '1ч', '4h': '4ч' }[mode]}`);
}
async function applyLid(m) {
  const cs = pluginById('clamshell');
  if (m === 'keep' && cs && !cs.enabled) await window.jarvis.pluginCmd('clamshell', '_enable', { on: true });
  pluginCmd('clamshell', m === 'keep' ? 'arm' : 'disarm');
  showToast(m === 'keep' ? 'Крышка закрыта — не уснёт' : 'Крышка закрыта — обычный сон');
}
async function applyPos(p) {
  await window.jarvis.setSettings({ position: p });
  palSettings = null;
  showToast(p === 'corner' ? 'Панель — в правом верхнем углу' : 'Панель — по центру');
}
async function applyNotify(on) {
  await window.jarvis.setSettings({ notifyDone: on, notifyWaiting: on });
  palSettings = null;
  showToast(on ? 'Уведомления включены' : 'Уведомления выключены');
}

// текущее активное значение команды — для подсветки чипа
function activeChip(kind) {
  if (kind === 'amf') return awakeState(pluginById('keep-awake') && pluginById('keep-awake').status).seg;
  if (kind === 'lid') return pluginById('clamshell') && pluginById('clamshell').status && pluginById('clamshell').status.armed ? 'keep' : 'sleep';
  if (kind === 'pos') return palSettings ? palSettings.position : null;
  if (kind === 'notify') return palSettings ? (palSettings.notifyDone && palSettings.notifyWaiting ? 'on' : 'off') : null;
  return null;
}

// чипы-аргументы под выбранной командой: [{id,label,run}]
function chipsFor(kind) {
  if (kind === 'amf') return [['off', 'Выкл'], ['15m', '15м'], ['1h', '1ч'], ['4h', '4ч'], ['inf', '∞']].map(([id, label]) => ({ id, label, run: () => { if (id === 'amf') return; applyAmf(id); } }));
  if (kind === 'lid') return [['sleep', 'Спать'], ['keep', 'Не спать']].map(([id, label]) => ({ id, label, run: () => applyLid(id) }));
  if (kind === 'pos') return [['center', 'Центр'], ['corner', 'Угол']].map(([id, label]) => ({ id, label, run: () => applyPos(id) }));
  if (kind === 'notify') return [['on', 'Вкл'], ['off', 'Выкл']].map(([id, label]) => ({ id, label, run: () => applyNotify(id === 'on') }));
  return [];
}

// одной строкой: «/amf 1ч», «/lid keep», «/pos угол» → исполнить, true если смогли
function runRootCommand(q) {
  const parts = q.trim().slice(1).split(/\s+/).filter(Boolean);
  const cmd = '/' + (parts[0] || '').toLowerCase();
  const arg = parts[1];
  if (!arg) return false;
  const a = arg.toLowerCase();
  if (cmd === '/amf') { const m = amfArg(a); if (!m) return false; applyAmf(m); clearCmd(); return true; }
  if (cmd === '/lid') {
    if (['keep', 'не', 'нет', 'awake', 'бодр'].includes(a)) applyLid('keep');
    else if (['sleep', 'спать', 'сон'].includes(a)) applyLid('sleep');
    else return false;
    clearCmd(); return true;
  }
  if (cmd === '/pos') {
    if (['center', 'центр', 'середина'].includes(a)) applyPos('center');
    else if (['corner', 'угол'].includes(a)) applyPos('corner');
    else return false;
    clearCmd(); return true;
  }
  if (cmd === '/notify') {
    if (['on', 'вкл', 'да'].includes(a)) applyNotify(true);
    else if (['off', 'выкл', 'нет'].includes(a)) applyNotify(false);
    else return false;
    clearCmd(); return true;
  }
  return false;
}

function clearCmd() {
  queryEl.value = '';
  cmdRootSel = 0;
  argMode = null;
  render();
  queryEl.focus();
}

/* ----- рендер палитры команд ----- */
function renderCmdPalette() {
  // подгружаем настройки для подсветки чипов pos/notify (один раз)
  if (palSettings === null && (queryEl.value.includes('/pos') || queryEl.value.includes('/notify') || queryEl.value.trim() === '/')) {
    palSettings = {};
    window.jarvis.getSettings().then((s) => { palSettings = { position: s.position, notifyDone: s.notifyDone, notifyWaiting: s.notifyWaiting }; if (view === 'list' && queryEl.value.startsWith('/')) renderCmdPalette(); }).catch(() => {});
  }
  if (argMode === 'amf') { renderArgMode(); return; }

  listEl.textContent = '';
  const box = document.createElement('div');
  box.className = 'cpal';
  const lab = document.createElement('div');
  lab.className = 'cpal-label';
  lab.textContent = 'Быстрые команды';
  box.appendChild(lab);

  const matches = cmdMatches();
  cmdRootSel = Math.min(cmdRootSel, matches.length - 1);
  matches.forEach((c, i) => {
    const row = document.createElement('div');
    row.className = 'cpal-row' + (i === cmdRootSel ? ' sel' : '');
    const g = document.createElement('span');
    g.className = 'glyph';
    g.appendChild(cmdGlyph(c.glyph));
    row.appendChild(g);
    const cmd = document.createElement('span');
    cmd.className = 'cpal-cmd';
    cmd.textContent = c.cmd;
    row.appendChild(cmd);
    const desc = document.createElement('span');
    desc.className = 'cpal-desc';
    desc.textContent = c.desc;
    row.appendChild(desc);
    row.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
    const args = document.createElement('span');
    args.className = 'cpal-args';
    args.textContent = c.args;
    row.appendChild(args);
    row.addEventListener('mouseenter', () => { if (!palHoverEnabled || cmdRootSel === i) return; cmdRootSel = i; renderCmdPalette(); });
    row.addEventListener('click', () => { if (c.kind === 'amf') enterArg(); else { queryEl.value = c.cmd + ' '; queryEl.focus(); renderCmdPalette(); } });
    box.appendChild(row);

    if (i === cmdRootSel) {
      const chips = document.createElement('div');
      chips.className = 'cpal-chips';
      const active = activeChip(c.kind);
      for (const ch of chipsFor(c.kind)) {
        const chip = document.createElement('div');
        chip.className = 'cpal-chip' + (ch.id === active ? ' active' : '');
        chip.textContent = ch.label;
        chip.addEventListener('click', () => { ch.run(); clearCmd(); });
        chips.appendChild(chip);
      }
      box.appendChild(chips);
    }
  });

  const hint = document.createElement('div');
  hint.className = 'cpal-hint';
  hint.append('Например ');
  const c1 = document.createElement('code'); c1.textContent = '/amf 1ч';
  const c2 = document.createElement('code'); c2.textContent = '/amf off';
  hint.append(c1, ' — не спать час · ', c2, ' — выключить · ↵ запустить');
  box.appendChild(hint);

  listEl.appendChild(box);
}

/* ----- arg-режим /amf: Raycast-поля Часы/Минуты ----- */
function humanDur(total) {
  const h = Math.floor(total / 60), m = total % 60;
  const parts = [];
  if (h) parts.push(h + ' ч');
  if (m) parts.push(m + ' мин');
  return parts.join(' ') || '0 мин';
}

function enterArg() {
  argMode = 'amf';
  argH = '';
  argM = '';
  argFocus = 'h';
  queryEl.value = '/amf ';
  renderArgMode();
}

function exitArgToList() {
  argMode = null;
  queryEl.value = '/';
  cmdRootSel = 0;
  render();
  queryEl.focus();
}

function runArg() {
  const total = (parseInt(argH || '0', 10) || 0) * 60 + (parseInt(argM || '0', 10) || 0);
  if (total <= 0) { showToast('Укажи время'); return; }
  pluginCmd('keep-awake', 'start-timer', { minutes: total });
  showToast('Не сплю ещё ' + humanDur(total));
  clearCmd();
}

let argHInput = null;
let argMInput = null;
let argTitleEl = null;

function argTotal() { return (parseInt(argH || '0', 10) || 0) * 60 + (parseInt(argM || '0', 10) || 0); }

function refreshArgTitle() {
  if (argTitleEl) {
    const t = argTotal();
    argTitleEl.textContent = t > 0 ? 'Не спать ' + humanDur(t) : 'Не спать — укажи время';
  }
}

function focusArgField() {
  const el = argFocus === 'h' ? argHInput : argMInput;
  if (el) el.focus();
}

function renderArgMode() {
  listEl.textContent = '';

  // строка полей: глиф + «Не спать» + Часы/Минуты + хинт
  const argrow = document.createElement('div');
  argrow.className = 'cpal-argrow';
  const g = document.createElement('span');
  g.className = 'glyph';
  g.appendChild(awakeGlyph('coffee'));
  argrow.appendChild(g);
  const nm = document.createElement('span');
  nm.className = 'atitle';
  nm.textContent = 'Не спать';
  argrow.appendChild(nm);

  const mkField = (val, ph, which) => {
    const inp = document.createElement('input');
    inp.className = 'cpal-field';
    inp.value = val;
    inp.placeholder = ph;
    inp.inputMode = 'numeric';
    inp.spellcheck = false;
    inp.autocomplete = 'off';
    inp.addEventListener('focus', () => { argFocus = which; });
    inp.addEventListener('input', () => {
      let v = inp.value.replace(/\D/g, '').slice(0, 2);
      if (v !== '') v = String(Math.min(which === 'h' ? 23 : 59, parseInt(v, 10)));
      inp.value = v;
      if (which === 'h') argH = v; else argM = v;
      refreshArgTitle();
    });
    return inp;
  };
  argHInput = mkField(argH, 'Часы', 'h');
  argMInput = mkField(argM, 'Минуты', 'm');
  argrow.appendChild(argHInput);
  argrow.appendChild(argMInput);
  argrow.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
  const ah = document.createElement('span');
  ah.className = 'cpal-arghint';
  ah.textContent = '⇥ поле · ↵ запустить · esc назад';
  argrow.appendChild(ah);
  listEl.appendChild(argrow);

  // Результаты: живой заголовок + аксессуар «Команда»
  const results = document.createElement('div');
  results.className = 'cpal-results';
  const rl = document.createElement('div');
  rl.className = 'cpal-label';
  rl.textContent = 'Результаты';
  results.appendChild(rl);
  const resrow = document.createElement('div');
  resrow.className = 'cpal-resrow';
  const ic = document.createElement('span');
  ic.className = 'cpal-resicon';
  ic.appendChild((() => { const s = svgEl('svg', { width: '15', height: '15', viewBox: '0 0 16 16', fill: 'none' }); s.appendChild(svgEl('path', { d: 'M3 6.5 H10.5 V9.5 A2.5 2.5 0 0 1 8 12 H5.5 A2.5 2.5 0 0 1 3 9.5 Z M10.5 7 H12 A1.5 1.5 0 0 1 12 10 H10.5', stroke: '#FFFFFF', 'stroke-width': '1.3', 'stroke-linejoin': 'round' })); return s; })());
  resrow.appendChild(ic);
  argTitleEl = document.createElement('span');
  argTitleEl.className = 'cpal-restitle';
  resrow.appendChild(argTitleEl);
  resrow.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
  const badge = document.createElement('span');
  badge.className = 'cpal-resbadge';
  badge.textContent = 'Команда';
  resrow.appendChild(badge);
  resrow.addEventListener('click', runArg);
  results.appendChild(resrow);

  // Быстро: пресеты заполняют поля
  const pl = document.createElement('div');
  pl.className = 'cpal-label';
  pl.style.paddingTop = '12px';
  pl.textContent = 'Быстро';
  results.appendChild(pl);
  const presets = document.createElement('div');
  presets.className = 'cpal-presets';
  for (const [label, h, m] of [['15 мин', 0, 15], ['30 мин', 0, 30], ['1 ч', 1, 0], ['2 ч', 2, 0], ['4 ч', 4, 0]]) {
    const chip = document.createElement('div');
    chip.className = 'cpal-chip';
    chip.textContent = label;
    chip.addEventListener('click', () => {
      argH = h ? String(h) : '';
      argM = m ? String(m) : '';
      if (argHInput) argHInput.value = argH;
      if (argMInput) argMInput.value = argM;
      refreshArgTitle();
    });
    presets.appendChild(chip);
  }
  results.appendChild(presets);
  listEl.appendChild(results);

  refreshArgTitle();
  setTimeout(() => focusArgField(), 20);
}

/* ---------- футер и меню действий (⌘K) ---------- */

const footerEl = document.getElementById('footer');
const primaryLabelEl = document.getElementById('primaryLabel');
const primaryKeyEl = document.getElementById('primaryKey');
const actionsPopEl = document.getElementById('actionsPop');
let apSel = 0;

function actionItems() {
  const s = view === 'list' ? filtered()[sel]
    : view === 'chat' ? state.find((x) => x.id === chatSessionId)
    : null;
  const items = [];
  if (s) {
    items.push({ label: 'Перейти в терминал', key: '⌘↵', run: () => focusTerminal(s.id, s.project) });
    items.push({ label: s.pinned ? 'Открепить' : 'Закрепить', key: '⌘P', run: () => window.jarvis.setPin(s.id, !s.pinned) });
    if (s.tmuxPane) items.push({ label: 'Где этот терминал?', key: '⌘G', run: () => window.jarvis.pingTerminal(s.id) });
  }
  if (view !== 'chat') items.push({ label: 'Очистить завершённые', key: '⌘⌫', run: () => window.jarvis.clearFinished() });
  items.push({ label: 'Проекты и история', key: '⌘2', run: () => setView('history') });
  items.push({ label: 'Статистика usage', key: '⌘3', run: () => setView('stats') });
  items.push({ label: 'Настройки', key: '⌘,', run: () => setView('settings') });
  return items;
}

function actionsOpen() { return !actionsPopEl.hidden; }

function closeActions() {
  actionsPopEl.hidden = true;
}

function paintActions(items) {
  actionsPopEl.textContent = '';
  items.forEach((it, i) => {
    const row = document.createElement('div');
    row.className = 'ap-item' + (i === apSel ? ' sel' : '');
    const label = document.createElement('span');
    label.textContent = it.label;
    const spacer = document.createElement('span');
    spacer.className = 'spacer';
    const key = document.createElement('span');
    key.className = 'keycap';
    key.textContent = it.key;
    row.append(label, spacer, key);
    row.addEventListener('mouseenter', () => { apSel = i; paintActions(items); });
    row.addEventListener('click', () => { closeActions(); it.run(); });
    actionsPopEl.appendChild(row);
  });
}

function toggleActions() {
  if (actionsOpen()) { closeActions(); return; }
  if (view === 'question') return; // на экране вопроса клавиатура занята пикером
  apSel = 0;
  paintActions(actionItems());
  actionsPopEl.hidden = false;
}

document.getElementById('actionsBtn').addEventListener('click', toggleActions);
document.getElementById('primaryHint').addEventListener('click', () => {
  if (view === 'list') { const s = filtered()[sel]; if (s) openSession(s); }
  else if (view === 'settings') { setView('list'); render(); }
});

tabSessionsEl.addEventListener('click', () => { setView('list'); render(); });

/* ---------- вкладка «Проекты» (история чатов по проектам) ---------- */

const historyEl = document.getElementById('history');
const tabHistoryEl = document.getElementById('tabHistory');
tabHistoryEl.addEventListener('click', () => setView('history'));

let historyData = [];
let histRows = []; // плоский список выбираемых строк: проекты или чаты (для ↑↓/Enter)
let histSel = 0;
let histProject = null; // ключ открытого проекта (cwd) — null = список проектов

function histTime(ts) {
  const d = new Date(ts);
  const now = new Date();
  const same = d.toDateString() === now.toDateString();
  return same ? `${pad2(d.getHours())}:${pad2(d.getMinutes())}` : `${pad2(d.getDate())}.${pad2(d.getMonth() + 1)}`;
}

function resumeCommand(s, cwd) {
  const base = `claude --resume ${s.id} --dangerously-skip-permissions`;
  return cwd ? `cd "${cwd}" && ${base}` : base;
}

async function copyResume(s, cwd) {
  try { await navigator.clipboard.writeText(resumeCommand(s, cwd)); showToast('Команда скопирована'); }
  catch { showToast('Не удалось скопировать'); }
}

function openHistProject(key) {
  histProject = key;
  queryEl.value = ''; // фильтр списка проектов внутри проекта не нужен
  renderHistory();
}

async function renderHistory() {
  try { historyData = await window.jarvis.getHistory(); } catch { historyData = []; }
  if (view !== 'history') return;
  historyEl.textContent = '';
  histRows = [];
  histSel = 0;

  const q = queryEl.value.trim().toLowerCase();

  let g = null;
  if (histProject != null) {
    g = historyData.find((x) => (x.cwd || x.project) === histProject);
    if (!g) histProject = null; // проект исчез с диска — назад к списку
  }

  if (!g) renderHistProjects(q);
  else renderHistChats(g, q);
  paintHistSel();
}

/* уровень 1: проекты */
function renderHistProjects(q) {
  primaryLabelEl.textContent = 'Открыть проект';
  const groups = q
    ? historyData.filter((x) => x.project.toLowerCase().includes(q) || x.sessions.some((s) => s.title.toLowerCase().includes(q)))
    : historyData;

  if (!groups.length) {
    historyEl.appendChild(Object.assign(document.createElement('div'), { className: 'empty', textContent: q ? 'Ничего не найдено' : 'История пуста' }));
    return;
  }

  for (const x of groups) {
    const key = x.cwd || x.project;
    const idx = histRows.length;
    histRows.push({ type: 'project', key });
    const row = document.createElement('div');
    row.className = 'hrow';
    row.dataset.idx = idx;
    row.title = x.cwd || x.project;

    row.appendChild(Object.assign(document.createElement('span'), { className: 'htitle', textContent: x.project }));
    row.appendChild(Object.assign(document.createElement('span'), {
      className: 'hmeta',
      textContent: `${x.count} ${plural(x.count, 'чат', 'чата', 'чатов')} · ${histTime(x.lastAt)}`,
    }));
    row.appendChild(Object.assign(document.createElement('span'), { className: 'hchev', textContent: '›' }));

    row.addEventListener('mouseenter', () => { histSel = idx; paintHistSel(); });
    row.addEventListener('click', () => openHistProject(key));
    historyEl.appendChild(row);
  }
}

/* уровень 2: чаты проекта */
function renderHistChats(g, q) {
  primaryLabelEl.textContent = 'Скопировать команду';
  const head = document.createElement('div');
  head.className = 'hgroup';
  const back = Object.assign(document.createElement('span'), { className: 'hback', textContent: '‹ Проекты' });
  back.addEventListener('click', () => { histProject = null; renderHistory(); });
  head.appendChild(back);
  head.appendChild(Object.assign(document.createElement('span'), { textContent: g.project }));
  head.appendChild(Object.assign(document.createElement('span'), { className: 'hcount', textContent: `${g.count} ${plural(g.count, 'чат', 'чата', 'чатов')}` }));
  historyEl.appendChild(head);

  historyEl.appendChild(Object.assign(document.createElement('div'), { className: 'hhint', textContent: '↵ — скопировать команду продолжения (--resume --dangerously-skip-permissions) · esc — к проектам' }));

  const sessions = q ? g.sessions.filter((s) => s.title.toLowerCase().includes(q)) : g.sessions;
  if (!sessions.length) {
    historyEl.appendChild(Object.assign(document.createElement('div'), { className: 'empty', textContent: 'Ничего не найдено' }));
    return;
  }

  for (const s of sessions) {
    const idx = histRows.length;
    histRows.push({ type: 'chat', s, cwd: g.cwd });
    const row = document.createElement('div');
    row.className = 'hrow';
    row.dataset.idx = idx;
    row.title = `${s.title}\n${resumeCommand(s, g.cwd)}`;

    const title = document.createElement('span');
    title.className = 'htitle';
    title.textContent = s.title || s.id.slice(0, 8);
    row.appendChild(title);

    const meta = document.createElement('span');
    meta.className = 'hmeta';
    const parts = [];
    if (s.model) parts.push(s.model);
    if (s.tokens) parts.push(fmtTok(s.tokens));
    parts.push(histTime(s.lastAt));
    meta.textContent = parts.join(' · ');
    row.appendChild(meta);

    row.appendChild(Object.assign(document.createElement('span'), { className: 'hcopy', textContent: 'копировать ↵' }));

    row.addEventListener('mouseenter', () => { histSel = idx; paintHistSel(); });
    row.addEventListener('click', () => copyResume(s, g.cwd));
    historyEl.appendChild(row);
  }
}

function paintHistSel() {
  for (const row of historyEl.querySelectorAll('.hrow')) {
    row.classList.toggle('selected', Number(row.dataset.idx) === histSel);
  }
  historyEl.querySelector('.hrow.selected')?.scrollIntoView({ block: 'nearest' });
}

/* ---------- вкладка «Статистика» ---------- */

const statsEl = document.getElementById('stats');
const tabStatsEl = document.getElementById('tabStats');
tabStatsEl.addEventListener('click', () => setView('stats'));

const fmtTok = (n) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${Math.round(n / 1e3)}K` : String(n || 0));

function moneyLine(api, plan) {
  const parts = [];
  if (api > 0.005) parts.push({ text: `$${api.toFixed(2)} API`, api: true });
  if (plan > 0.005) parts.push({ text: `~$${plan.toFixed(2)} план`, api: false });
  if (!parts.length) parts.push({ text: '—', api: false });
  return parts;
}

function el(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text != null) n.textContent = text;
  return n;
}

let statsPeriod = 'today'; // 'today' | 'week'
let statsDim = 'projects'; // 'models' | 'projects' | 'sessions'

const PERIODS = [['today', 'Сегодня'], ['week', '7 дней']];
const DIMS = [['models', 'Модели'], ['projects', 'Проекты'], ['sessions', 'Сессии'], ['billing', 'Биллинг']];

function segRow(items, current, onPick) {
  const seg = el('div', 'seg');
  for (const [val, label] of items) {
    const b = el('button', 'segbtn' + (val === current ? ' active' : ''), label);
    b.addEventListener('click', () => onPick(val));
    seg.appendChild(b);
  }
  return seg;
}

async function renderStats() {
  let u;
  try { u = await window.jarvis.getUsage(statsPeriod); } catch { return; }
  if (view !== 'stats') return;
  statsEl.textContent = '';

  // управление: период и разрез
  const controls = el('div', 'uctl');
  controls.appendChild(segRow(PERIODS, statsPeriod, (v) => { statsPeriod = v; renderStats(); }));
  controls.appendChild(segRow(DIMS, statsDim, (v) => { statsDim = v; renderStats(); }));
  controls.appendChild(el('span', 'uhint', '←→ период · 1-4 разрез'));
  statsEl.appendChild(controls);

  // тотал выбранного периода
  const b = el('div', 'ubig');
  b.appendChild(el('div', 'ulabel', statsPeriod === 'week' ? 'За 7 дней' : 'Сегодня · с 3:00 МСК'));
  b.appendChild(el('div', 'uval', fmtTok(u.total.tok)));
  const money = el('div', 'umoney');
  moneyLine(u.total.api, u.total.plan).forEach((p, i) => {
    if (i) money.appendChild(document.createTextNode(' · '));
    money.appendChild(el('span', p.api ? 'api' : '', p.text));
  });
  b.appendChild(money);
  statsEl.appendChild(b);

  // лимиты подписки — официальные (claude -p "/usage"), с планом и процентами
  if (u.official) {
    const o = u.official;
    const head = el('div', 'usect', `Лимиты подписки${o.account.plan ? ` · ${o.account.plan}` : ''}`);
    if (o.account.email) head.appendChild(el('span', 'uhover', o.account.email));
    statsEl.appendChild(head);

    const resetText = (ts) => {
      if (!ts) return '';
      const ms = ts - Date.now();
      if (ms <= 0) return 'скоро сброс';
      const min = Math.round(ms / 60000);
      if (min < 24 * 60) return `сброс через ${Math.floor(min / 60)}ч ${min % 60}м`;
      const d = new Date(ts);
      return `сброс ${pad2(d.getDate())}.${pad2(d.getMonth() + 1)} в ${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
    };

    const limitRow = (label, pct, extra) => {
      const row = el('div', 'ulim');
      row.appendChild(el('span', 'ulim-label', label));
      const track = el('div', 'ulim-track');
      const fill = el('div', 'ulim-fill');
      fill.style.width = `${Math.min(100, pct)}%`;
      if (pct > 90) fill.classList.add('crit');
      else if (pct > 75) fill.classList.add('warn');
      track.appendChild(fill);
      row.appendChild(track);
      row.appendChild(el('span', 'ulim-pct', `${pct}%`));
      row.appendChild(el('span', 'ulim-reset', extra || ''));
      statsEl.appendChild(row);
    };

    if (o.session) limitRow('Сессия', o.session.pct, `${resetText(o.session.resetAt)}${o.windowTokens ? ` · ${fmtTok(o.windowTokens)} ткн` : ''}`);
    if (o.week) limitRow('Неделя', o.week.pct, resetText(o.week.resetAt));
    if (o.weekSonnet) limitRow('Sonnet', o.weekSonnet.pct, '');
  } else if (u.window.resetInMs > 0) {
    // официальные данные ещё не приехали — локальная оценка
    const min = Math.round(u.window.resetInMs / 60000);
    const win = el('div', 'uwindow');
    win.appendChild(el('span', 'uwtok', `${fmtTok(u.window.tokens)} ткн`));
    win.appendChild(document.createTextNode(`за 5ч-окно (локальная оценка) · ~сброс через ${Math.floor(min / 60)}ч ${min % 60}м`));
    statsEl.appendChild(win);
  }

  // график периода
  const sect = el('div', 'usect', statsPeriod === 'week' ? 'По дням' : 'По часам');
  const hover = el('span', 'uhover', '');
  sect.appendChild(hover);
  statsEl.appendChild(sect);
  const chart = el('div', 'uchart');
  const max = Math.max(1, ...u.series.map((h) => h.tok));
  for (const h of u.series) {
    const wrap = el('div', 'ubar-wrap');
    const bar = el('div', 'ubar');
    bar.style.height = `${Math.max(3, Math.round((h.tok / max) * 100))}%`;
    if (!h.tok) bar.style.opacity = '0.25';
    wrap.appendChild(bar);
    wrap.addEventListener('mouseenter', () => { hover.textContent = `${h.label} · ${fmtTok(h.tok)}`; });
    chart.appendChild(wrap);
  }
  chart.addEventListener('mouseleave', () => { hover.textContent = ''; });
  statsEl.appendChild(chart);

  // одна таблица — выбранный разрез
  const isApiB = (b) => b && b !== 'plan';
  const planName = (u.official && u.official.account.plan) ? ` ${u.official.account.plan}` : '';
  const rows = statsDim === 'models'
    ? u.byModel.map((m) => ({ name: m.key, tok: m.tok, api: m.api, plan: m.plan }))
    : statsDim === 'projects'
      ? u.byProject.map((p) => ({
          name: p.key, badge: isApiB(p.billing) ? 'API' : 'план',
          titleAttr: isApiB(p.billing) ? p.billing.slice(4) : '',
          tok: p.tok, api: p.api, plan: p.plan,
        }))
      : statsDim === 'sessions'
        ? u.sessions.map((s) => ({
            name: `${s.project} · ${s.model}`, titleAttr: s.id, tok: s.tok,
            api: isApiB(s.billing) ? s.cost : 0, plan: isApiB(s.billing) ? 0 : s.cost,
          }))
        : (u.byBilling || []).map((b) => ({
            name: b.host || `Подписка${planName}`,
            badge: b.host ? 'API' : 'план',
            titleAttr: `проекты: ${b.projects.join(', ')}`,
            tok: b.tok, api: b.api, plan: b.plan,
          }));

  statsEl.appendChild(el('div', 'usect', DIMS.find(([v]) => v === statsDim)[1]));
  if (!rows.length) statsEl.appendChild(el('div', 'uwindow', 'пусто за период'));
  const maxTok = Math.max(1, ...rows.map((r) => r.tok));
  for (const r of rows) {
    const row = el('div', 'urow');
    const name = el('span', 'uname', r.name);
    if (r.titleAttr) name.title = r.titleAttr;
    row.appendChild(name);
    if (r.badge) row.appendChild(el('span', 'badge host', r.badge));
    const track = el('div', 'ubartrack');
    const fill = el('div', 'ubarfill');
    fill.style.width = `${Math.round((r.tok / maxTok) * 100)}%`;
    track.appendChild(fill);
    row.appendChild(track);
    row.appendChild(el('span', 'unum', fmtTok(r.tok)));
    row.appendChild(el('span', `unum money${r.api > 0.005 ? ' api' : ''}`,
      r.api > 0.005 ? `$${r.api.toFixed(2)}` : `~$${(r.plan ?? 0).toFixed(2)}`));
    statsEl.appendChild(row);
  }
}
tabSettingsEl.addEventListener('click', () => {
  if (view === 'settings') { setView('list'); render(); } else setView('settings');
});

// кнопка «Открыть настройки» из окна онбординга
window.jarvis.onGotoSettings(() => setView('settings'));

// Wake-word (инкр. 10): живой индикатор «слушаю»/срабатывание + рефреш после установки
window.jarvis.onAudioState((p) => {
  const pill = document.getElementById('wake-status-pill');
  if (!pill || !p) return;
  let txt = 'выключено', cls = '';
  if (p.muted || p.state === 'muted') txt = 'заглушено';
  else if (p.state === 'denied') txt = 'нет доступа к микрофону';
  else if (p.state === 'listening') { txt = 'слушаю'; cls = 'on'; }
  else if (p.state === 'no-device') txt = 'нет устройства';
  pill.textContent = txt;
  pill.className = 'astatus' + (cls ? ' ' + cls : '');
});
window.jarvis.onWake((p) => {
  if (!p || p.phase !== 'detected') return;
  const pill = document.getElementById('wake-status-pill');
  if (pill) { pill.textContent = 'сработало!'; pill.className = 'astatus on'; }
});
window.jarvis.onWakeInstallDone(() => { try { renderWakeCard(); renderModelManager(); } catch {} });
// STT-модели качаются по запросу (кнопка в карточке) — прогресс в строку,
// финал перерисовывает карточку, ошибку показываем тостом.
window.jarvis.onSttInstallProgress((step) => {
  const el = document.getElementById('stt-install-progress');
  if (el && step && step.msg) el.textContent = step.msg;
});
window.jarvis.onSttInstallDone((p) => {
  try {
    if (p && !p.ok && p.error) showToast('STT: не удалось — ' + p.error);
    renderSttCard();
    renderModelManager();
  } catch {}
});

/* ---------- настройки ---------- */

const hotkeyBtn = document.getElementById('hotkey');
const hotkeyErr = document.getElementById('hotkeyError');
let recording = false;
let recordingKey = 'hotkey'; // какой хоткей записываем (settings-ключ)
let recordingBtn = hotkeyBtn; // кнопка, что сейчас в режиме записи

// дефолты «прочих» хоткеев — чтобы кнопки показывали реальное значение
const HK_DEFAULTS = {
  continueHotkey: 'Command+Alt+C',
  repeatHotkey: 'Command+Alt+R',
  muteHotkey: 'Command+Alt+M',
  quietHotkey: 'Command+Alt+J',
};

function startRecording(btn, key) {
  recording = true;
  recordingKey = key;
  recordingBtn = btn;
  hotkeyErr.hidden = true;
  btn.classList.add('recording');
  btn.textContent = 'нажми сочетание…';
}

function displayHotkey(acc) {
  return acc
    .replace('CommandOrControl', '⌘').replace('Command', '⌘')
    .replace('Control', '⌃').replace('Option', '⌥').replace('Alt', '⌥')
    .replace('Shift', '⇧').replaceAll('+', ' ');
}

async function loadSettings() {
  const s = await window.jarvis.getSettings();
  hotkeyBtn.textContent = displayHotkey(s.hotkey);
  for (const btn of document.querySelectorAll('.keycap[data-hk]')) {
    const key = btn.dataset.hk;
    btn.textContent = displayHotkey(s[key] || HK_DEFAULTS[key] || '');
  }
  document.getElementById('notifyDone').checked = s.notifyDone;
  document.getElementById('notifyWaiting').checked = s.notifyWaiting;
  document.getElementById('autoResume').checked = s.autoResume !== false;
  document.getElementById('openAtLogin').checked = s.openAtLogin;
  document.getElementById('diagnostics').checked = !!s.diagnostics;
  for (const b of document.querySelectorAll('.segbtn')) {
    b.classList.toggle('active', b.dataset.v === s.position);
  }
  try {
    plugins = await window.jarvis.getPlugins();
    renderPluginRows();
  } catch {}
  renderModelManager();
  renderSttCard();
  renderWakeCard();
  renderVoiceCard();
  renderIntegrationCard();
}

/* ── формат размера на диске ── */
function fmtBytes(n) {
  if (!n) return '0 МБ';
  const mb = n / (1024 * 1024);
  if (mb >= 1024) return (mb / 1024).toFixed(mb >= 10240 ? 0 : 1) + ' ГБ';
  return Math.max(1, Math.round(mb)) + ' МБ';
}

/* ── карточка «Интеграция»: статус компонентов + удалить/переустановить ──
 *    + вложенная карточка моделей голоса (место/удаление). */
async function renderIntegrationCard() {
  const box = document.getElementById('integrationCard');
  const mbox = document.getElementById('modelsCard');
  if (!box) return;
  let info = null;
  try { info = await window.jarvis.integrationGet(); } catch {}
  if (!info) { box.textContent = ''; return; }
  const st = info.status || {};
  const integrated = st.hooks && st.shim;

  box.textContent = '';

  // шапка
  const head = document.createElement('div');
  head.className = 'awakehead';
  const title = document.createElement('span');
  title.className = 'atitle';
  title.textContent = 'Claude Code';
  head.appendChild(title);
  head.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
  const status = document.createElement('span');
  status.className = integrated ? 'astatus on' : 'astatus';
  status.textContent = integrated ? 'подключено' : 'не подключено';
  head.appendChild(status);
  box.appendChild(head);

  // строки компонентов
  const rows = [
    ['hooks', 'Хуки событий', st.hooks],
    ['shim', 'Шим запуска claude', st.shim],
    ['tmux_conf', 'tmux-транспорт', st.tmux_conf],
    ['path_block', 'PATH-блок в shell', st.path_block],
  ];
  for (const [, label, ok] of rows) {
    const r = document.createElement('div');
    r.className = 'istat hairtop' + (ok ? ' on' : '');
    r.appendChild(Object.assign(document.createElement('span'), { className: 'dot' }));
    r.appendChild(Object.assign(document.createElement('span'), { textContent: label }));
    r.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
    r.appendChild(Object.assign(document.createElement('span'), { className: 'sz', textContent: ok ? 'есть' : '—' }));
    box.appendChild(r);
  }

  // пометка про чужие хуки
  if (info.foreign_hooks > 0) {
    const h = document.createElement('div');
    h.className = 'ahint';
    h.textContent = `При удалении сохранятся ${info.foreign_hooks} чужих хук(ов) — трогаем только свои.`;
    box.appendChild(h);
  }

  // тумблер тихого режима (разработчик)
  box.appendChild(arow('Тихий режим (разработчик)',
    stoggle(!!info.quiet, (v) => window.jarvis.quietSet(v)), { hairtop: true }));
  const qhint = document.createElement('div');
  qhint.className = 'ahint';
  qhint.textContent = 'Фон копит статистику с хуков, но без тостов/голоса/показа. Тумблер — ⌘⌥J.';
  box.appendChild(qhint);

  // кнопки
  const brow = document.createElement('div');
  brow.className = 'abtnrow';
  const setup = document.createElement('button');
  setup.className = 'abtn primary';
  setup.textContent = integrated ? 'Переустановить' : 'Настроить';
  setup.addEventListener('click', () => { window.jarvis.onboardingOpen(); });
  brow.appendChild(setup);

  if (integrated) {
    const rm = document.createElement('button');
    rm.className = 'abtn danger';
    rm.textContent = 'Удалить интеграцию';
    let armed = false;
    rm.addEventListener('click', async () => {
      if (!armed) { armed = true; rm.textContent = 'Точно удалить?'; setTimeout(() => { armed = false; rm.textContent = 'Удалить интеграцию'; }, 3000); return; }
      rm.disabled = true; rm.textContent = 'Удаляю…';
      try { await window.jarvis.integrationRemove(); } catch {}
      renderIntegrationCard();
    });
    brow.appendChild(rm);
  }
  box.appendChild(brow);

  // вложенная карточка моделей
  renderModelsCard(mbox, info.models || []);
}

/* ── карточка «Голос: модели и место» ── */
function renderModelsCard(box, models) {
  if (!box) return;
  box.textContent = '';
  if (!models.length) { box.hidden = true; return; }
  box.hidden = false;

  const head = document.createElement('div');
  head.className = 'awakehead';
  head.appendChild(Object.assign(document.createElement('span'), { className: 'atitle', textContent: 'Модели голоса' }));
  head.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
  const total = models.reduce((a, m) => a + (m.bytes || 0), 0);
  head.appendChild(Object.assign(document.createElement('span'), { className: 'astatus on', textContent: fmtBytes(total) }));
  box.appendChild(head);

  for (const m of models) {
    const r = document.createElement('div');
    r.className = 'istat hairtop on';
    r.appendChild(Object.assign(document.createElement('span'), { className: 'dot' }));
    r.appendChild(Object.assign(document.createElement('span'), { textContent: m.label }));
    r.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
    r.appendChild(Object.assign(document.createElement('span'), { className: 'sz', textContent: fmtBytes(m.bytes) }));
    const del = document.createElement('button');
    del.className = 'abtn danger small';
    del.style.marginLeft = '10px';
    del.textContent = 'Удалить';
    let armed = false;
    del.addEventListener('click', async () => {
      if (!armed) { armed = true; del.textContent = 'Точно?'; setTimeout(() => { armed = false; del.textContent = 'Удалить'; }, 3000); return; }
      del.disabled = true; del.textContent = '…';
      try { await window.jarvis.modelDelete(m.id); } catch {}
      renderIntegrationCard();
    });
    r.appendChild(del);
    box.appendChild(r);
  }

  const hint = document.createElement('div');
  hint.className = 'ahint';
  hint.textContent = 'После удаления голос недоступен, пока не переустановишь интеграцию.';
  box.appendChild(hint);
}

/* ── карточка «Модели»: единый инвентарь всех моделей (STT/голос/wake) ──
 *    Инкремент 1 — только статус и размер. Скачать/удалить/активировать — далее. */
const MODEL_GROUPS = [
  ['stt', 'Распознавание речи'],
  ['voice', 'Голос'],
  ['wake', 'Wake-word'],
  ['runtime', 'Окружение'],
];

async function renderModelManager() {
  const box = document.getElementById('modelManagerCard');
  if (!box) return;
  box.textContent = '';
  let models = [];
  try { const r = await window.jarvis.modelsGet(); models = (r && r.models) || []; } catch {}
  if (!models.length) { box.hidden = true; return; }
  box.hidden = false;

  // шапка: суммарный размер на диске
  const head = document.createElement('div');
  head.className = 'awakehead';
  head.appendChild(Object.assign(document.createElement('span'), { className: 'atitle', textContent: 'Все модели' }));
  head.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
  const total = models.reduce((a, m) => a + (m.bytes || 0), 0);
  head.appendChild(Object.assign(document.createElement('span'), { className: 'astatus on', textContent: fmtBytes(total) }));
  box.appendChild(head);

  // строки, сгруппированные по виду модели
  for (const [kind, groupLabel] of MODEL_GROUPS) {
    const items = models.filter((m) => m.kind === kind);
    if (!items.length) continue;
    const sub = document.createElement('div');
    sub.className = 'ahint';
    sub.style.marginTop = '8px';
    sub.textContent = groupLabel;
    box.appendChild(sub);
    for (const m of items) box.appendChild(modelRow(m));
  }
}

// действие скачивания для не-скачанной модели (или null, если ставится иначе)
function downloadActionFor(m) {
  if (m.present) return null;
  switch (m.id) {
    case 'whisper-turbo': return { label: 'Скачать (~574 МБ)', run: () => window.jarvis.sttInstallWhisper() };
    case 'qwen3-0.6b': return { label: 'Скачать (~1 ГБ)', run: () => window.jarvis.sttInstallQwen('qwen3-0.6b') };
    case 'qwen3-1.7b': return { label: 'Скачать (~1 ГБ)', run: () => window.jarvis.sttInstallQwen('qwen3-1.7b') };
    case 'qwen3-runtime': return { label: 'Установить (~2.6 ГБ)', run: () => window.jarvis.sttInstallSidecar() };
    case 'hey_jarvis': return { label: 'Скачать', run: () => window.jarvis.wakeInstallModels() };
    default: return null; // silero ставится через настройку интеграции
  }
}

// можно ли удалить модель: скачана и не активный STT-движок
function canDeleteModel(m) {
  if (!m.present) return false;
  if (m.kind === 'stt' && m.active) return false; // активный движок не сносим
  return true;
}

// одна строка модели: статус-точка + имя + (активна) + размер + [Скачать|Удалить]
function modelRow(m) {
  const r = document.createElement('div');
  r.className = 'istat hairtop' + (m.present ? ' on' : '');
  r.appendChild(Object.assign(document.createElement('span'), { className: 'dot' }));
  r.appendChild(Object.assign(document.createElement('span'), { textContent: m.label }));
  if (m.kind === 'stt' && m.active && m.present) {
    const badge = Object.assign(document.createElement('span'), { className: 'astatus on', textContent: 'активна' });
    badge.style.marginLeft = '8px';
    r.appendChild(badge);
  }
  r.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
  r.appendChild(Object.assign(document.createElement('span'), {
    className: 'sz',
    textContent: m.present ? fmtBytes(m.bytes) : 'не скачана',
  }));

  const action = downloadActionFor(m);
  if (action) {
    const btn = document.createElement('button');
    btn.className = 'abtn small';
    btn.style.marginLeft = '10px';
    btn.textContent = action.label;
    btn.addEventListener('click', async () => {
      btn.disabled = true;
      btn.textContent = 'Качаю…';
      try { await action.run(); } catch {}
      // финал прилетит событием stt_install_done / wake_install_done → перерисует карточку
    });
    r.appendChild(btn);
    return r;
  }

  // скачана: «Сделать активной» (только не-активный STT-движок) + «Удалить»
  if (m.kind === 'stt' && !m.active) {
    const act = document.createElement('button');
    act.className = 'abtn small';
    act.style.marginLeft = '10px';
    act.textContent = 'Сделать активной';
    act.addEventListener('click', async () => {
      act.disabled = true;
      act.textContent = 'Включаю…';
      try {
        const res = await window.jarvis.sttSetEngine(m.id);
        if (res && res.ok === false) {
          showToast('Не удалось: ' + (res.error || ''));
          act.disabled = false; act.textContent = 'Сделать активной';
          return;
        }
        showToast(res && res.restart ? 'Активна после перезапуска Jarvis' : 'Активна: ' + m.label);
        renderModelManager();
        try { renderSttCard(); } catch {}
      } catch (e) {
        showToast('Ошибка: ' + e);
        act.disabled = false; act.textContent = 'Сделать активной';
      }
    });
    r.appendChild(act);
  }
  if (canDeleteModel(m)) {
    const del = document.createElement('button');
    del.className = 'abtn danger small';
    del.style.marginLeft = '10px';
    del.textContent = 'Удалить';
    let armed = false;
    del.addEventListener('click', async () => {
      if (!armed) { armed = true; del.textContent = 'Точно?'; setTimeout(() => { armed = false; del.textContent = 'Удалить'; }, 3000); return; }
      del.disabled = true; del.textContent = '…';
      try { await window.jarvis.modelDelete(m.id); } catch (e) { showToast('Не удалось удалить: ' + e); }
      renderModelManager();
      try { renderSttCard(); renderVoiceCard(); renderWakeCard(); } catch {}
    });
    r.appendChild(del);
  }
  return r;
}

// карточка «Голос»: движок, выбор спикера (Silero, живой), Тест, Без звука
async function renderVoiceCard() {
  const box = document.getElementById('voiceCard');
  if (!box) return;
  box.textContent = '';
  let v = null;
  try { v = await window.jarvis.voiceGet(); } catch {}
  if (!v) { box.textContent = ''; const n = document.createElement('div'); n.className = 'ahint'; n.textContent = 'Голос недоступен.'; box.appendChild(n); return; }
  const spacer = () => Object.assign(document.createElement('span'), { className: 'spacer' });

  const head = document.createElement('div');
  head.className = 'awakehead';
  const title = document.createElement('span');
  title.className = 'atitle';
  title.textContent = 'Озвучка событий';
  head.appendChild(title);
  head.appendChild(spacer());
  const eng = document.createElement('span');
  eng.className = 'astatus';
  eng.textContent = `движок: ${v.engine}`;
  head.appendChild(eng);
  box.appendChild(head);

  if (v.engine === 'silero') {
    const seg = document.createElement('div');
    seg.className = 'aseg';
    for (const sp of (v.speakers || [])) {
      const b = document.createElement('button');
      b.className = 'asegbtn' + (sp === v.speaker ? ' active' : '');
      b.textContent = sp;
      b.addEventListener('click', async () => {
        await window.jarvis.voiceSetSpeaker(sp); // живая смена + образец голосом
        renderVoiceCard();
      });
      seg.appendChild(b);
    }
    box.appendChild(seg);

    // скорость речи (живая)
    const RATE_LABELS = { slow: 'медленно', medium: 'норма', fast: 'быстро', 'x-fast': 'очень' };
    const rrow = document.createElement('div');
    rrow.className = 'arow hairtop';
    const rl = document.createElement('span');
    rl.className = 'alabel';
    rl.textContent = 'Скорость';
    rrow.appendChild(rl);
    rrow.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
    const rseg = document.createElement('div');
    rseg.className = 'seg';
    for (const rt of (v.rates || ['slow', 'medium', 'fast', 'x-fast'])) {
      const b = document.createElement('button');
      b.className = 'segbtn' + (rt === v.rate ? ' active' : '');
      b.textContent = RATE_LABELS[rt] || rt;
      b.addEventListener('click', async () => { await window.jarvis.voiceSetRate(rt); renderVoiceCard(); });
      rseg.appendChild(b);
    }
    rrow.appendChild(rseg);
    box.appendChild(rrow);
  }

  const row = document.createElement('div');
  row.className = 'arow hairtop';
  const test = document.createElement('button');
  test.className = 'keycap kbig';
  test.textContent = 'Тест';
  test.addEventListener('click', () => window.jarvis.voiceTest());
  row.appendChild(test);
  row.appendChild(spacer());
  const ml = document.createElement('span');
  ml.className = 'alabel';
  ml.textContent = 'Без звука';
  row.appendChild(ml);
  row.appendChild(stoggle(v.mute, (on) => window.jarvis.voiceSetMute(on)));
  box.appendChild(row);

  // пауза чужого медиа на время озвучки (как Siri)
  box.appendChild(arow('Пауза чужого звука',
    stoggle(v.duck !== false, (on) => window.jarvis.voiceSetDuck(on)), { hairtop: true }));
}

// ── карточка «Голосовой ввод (диктовка)» — STT (инкремент 9) ──────────────────
// ── Wake-word (инкремент 10): тумблер, mute, порог, тест фразы, модели ──
function wakeStatusLabel(v) {
  if (!v) return ['нет данных', ''];
  if (v.muted) return ['заглушено', ''];
  if (v.audio_state === 'denied') return ['нет доступа к микрофону', ''];
  if (v.listening) return ['слушаю', 'on'];
  if (v.enabled) return ['включено', 'on'];
  return ['выключено', ''];
}

async function renderWakeCard() {
  const box = document.getElementById('wake-card-root');
  if (!box) return;
  box.textContent = '';

  let v = null;
  try { v = await window.jarvis.wakeGet(); } catch {}

  const spacer = () => Object.assign(document.createElement('span'), { className: 'spacer' });
  const row = (cls) => { const d = document.createElement('div'); d.className = cls || 'arow'; return d; };
  const label = (t) => { const s = document.createElement('span'); s.className = 'alabel'; s.textContent = t; return s; };

  // шапка: заголовок + статус «слушаю»
  const head = row('awakehead');
  const title = document.createElement('span');
  title.className = 'atitle';
  title.textContent = 'Wake-word («Hey Jarvis»)';
  head.appendChild(title);
  head.appendChild(spacer());
  const [stxt, scls] = wakeStatusLabel(v);
  const pill = document.createElement('span');
  pill.className = 'astatus' + (scls ? ' ' + scls : '');
  pill.id = 'wake-status-pill';
  pill.textContent = stxt;
  head.appendChild(pill);
  box.appendChild(head);

  if (!v) {
    const hint = document.createElement('div');
    hint.className = 'ahint';
    hint.textContent = 'Данные wake-word недоступны.';
    box.appendChild(hint);
    return;
  }

  // тумблер вкл/выкл
  const enRow = row('arow hairtop');
  enRow.appendChild(label('Активация по фразе'));
  enRow.appendChild(spacer());
  const enToggle = document.createElement('input');
  enToggle.type = 'checkbox';
  enToggle.className = 'toggle';
  enToggle.checked = !!v.enabled;
  enToggle.addEventListener('change', async () => {
    await window.jarvis.wakeSetEnabled(enToggle.checked);
    renderWakeCard();
  });
  enRow.appendChild(enToggle);
  box.appendChild(enRow);

  // жёсткий mute (всегда доступен — глушит микрофон у источника)
  const muteRow = row('arow');
  muteRow.appendChild(label('Заглушить микрофон (mute)'));
  muteRow.appendChild(spacer());
  const muteToggle = document.createElement('input');
  muteToggle.type = 'checkbox';
  muteToggle.className = 'toggle';
  muteToggle.checked = !!v.muted;
  muteToggle.addEventListener('change', async () => {
    await window.jarvis.audioSetMute(muteToggle.checked);
    renderWakeCard();
  });
  muteRow.appendChild(muteToggle);
  box.appendChild(muteRow);

  // порог срабатывания
  const thRow = row('arow');
  thRow.appendChild(label('Порог срабатывания'));
  thRow.appendChild(spacer());
  const thVal = document.createElement('span');
  thVal.className = 'ahint';
  thVal.style.marginRight = '8px';
  thVal.textContent = Number(v.threshold ?? 0.5).toFixed(2);
  const th = document.createElement('input');
  th.type = 'range';
  th.min = '0'; th.max = '1'; th.step = '0.05';
  th.value = String(v.threshold ?? 0.5);
  th.addEventListener('input', () => { thVal.textContent = Number(th.value).toFixed(2); });
  th.addEventListener('change', async () => { await window.jarvis.wakeSetThreshold(Number(th.value)); });
  thRow.appendChild(thVal);
  thRow.appendChild(th);
  box.appendChild(thRow);

  // модели openWakeWord
  const mRow = row('arow');
  mRow.appendChild(label('Модели openWakeWord'));
  mRow.appendChild(spacer());
  if (v.model_present) {
    const ok = document.createElement('span');
    ok.className = 'astatus on';
    ok.textContent = 'на месте';
    mRow.appendChild(ok);
  } else {
    const btn = document.createElement('button');
    btn.className = 'abtn';
    btn.textContent = 'Скачать (~3.5 МБ)';
    btn.addEventListener('click', async () => {
      btn.disabled = true; btn.textContent = 'Скачиваю…';
      await window.jarvis.wakeInstallModels();
    });
    mRow.appendChild(btn);
  }
  box.appendChild(mRow);

  // верификация говорящего — шов (выключено)
  const vRow = row('arow');
  vRow.appendChild(label('Верификация говорящего'));
  vRow.appendChild(spacer());
  const vState = document.createElement('span');
  vState.className = 'ahint';
  vState.textContent = 'выключено · шов (реализация позже)';
  vRow.appendChild(vState);
  box.appendChild(vRow);

  // честная подсказка
  const hint = document.createElement('div');
  hint.className = 'ahint';
  hint.style.marginTop = '6px';
  hint.textContent = v.model_present
    ? 'Скажи «Hey Jarvis» — индикатор покажет «слушаю» при срабатывании. Работает офлайн.'
    : 'Скачай модели, затем включи активацию. Без моделей детектор инертен.';
  box.appendChild(hint);
}

async function renderSttCard() {
  const box = document.getElementById('stt-card-root');
  if (!box) return;
  box.textContent = '';

  let v = null;
  try { v = await window.jarvis.sttGet(); } catch {}

  const spacer = () => Object.assign(document.createElement('span'), { className: 'spacer' });

  // шапка: заголовок + текущий движок
  const head = document.createElement('div');
  head.className = 'awakehead';
  const title = document.createElement('span');
  title.className = 'atitle';
  title.textContent = 'Голосовой ввод (диктовка)';
  head.appendChild(title);
  head.appendChild(spacer());
  const engLabel = document.createElement('span');
  engLabel.className = v && v.available ? 'astatus on' : 'astatus';
  engLabel.textContent = v ? (v.available ? 'доступен' : 'недоступен') : 'нет данных';
  head.appendChild(engLabel);
  box.appendChild(head);

  if (!v) {
    const hint = document.createElement('div');
    hint.className = 'ahint';
    hint.textContent = 'STT-данные недоступны.';
    box.appendChild(hint);
    return;
  }

  // выбор движка (select)
  const engRow = document.createElement('div');
  engRow.className = 'arow hairtop';
  const engRowLabel = document.createElement('span');
  engRowLabel.className = 'alabel';
  engRowLabel.textContent = 'Движок';
  engRow.appendChild(engRowLabel);
  engRow.appendChild(spacer());
  const sel = document.createElement('select');
  sel.style.cssText = 'background:transparent;border:1px solid rgba(255,255,255,0.12);border-radius:6px;color:var(--text);font:inherit;font-size:12px;padding:3px 7px;outline:none;';
  for (const eng of (v.engines || ['whisper-turbo', 'qwen3-0.6b', 'qwen3-1.7b'])) {
    const opt = document.createElement('option');
    opt.value = eng;
    opt.textContent = eng;
    if (eng === v.engine) opt.selected = true;
    sel.appendChild(opt);
  }
  sel.addEventListener('change', async () => {
    const r = await window.jarvis.sttSetEngine(sel.value);
    if (r && r.restart) showToast('Движок изменён — перезапусти Jarvis для применения');
    renderSttCard();
  });
  engRow.appendChild(sel);
  box.appendChild(engRow);

  // статус моделей + предложение скачать недостающее (по умолчанию ничего не
  // тянем — пользователь жмёт кнопку сам; качается в фоне через события).
  // Строка с галкой/кнопкой: если модели нет — показываем кнопку «Скачать».
  const sttModelRow = (label, ready, onInstall, installLabel, idleLabel = '—') => {
    const r = document.createElement('div');
    r.className = 'istat hairtop' + (ready ? ' on' : '');
    r.appendChild(Object.assign(document.createElement('span'), { className: 'dot' }));
    r.appendChild(Object.assign(document.createElement('span'), { textContent: label }));
    r.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
    if (ready || !onInstall) {
      r.appendChild(Object.assign(document.createElement('span'), {
        className: 'sz', textContent: ready ? 'готово' : idleLabel,
      }));
    } else {
      const btn = document.createElement('button');
      btn.className = 'abtn small';
      btn.textContent = installLabel;
      btn.addEventListener('click', async () => {
        btn.disabled = true;
        btn.textContent = 'Качаю…';
        try { await onInstall(); } catch (e) { showToast(String(e)); btn.disabled = false; btn.textContent = installLabel; }
      });
      r.appendChild(btn);
    }
    box.appendChild(r);
  };

  sttModelRow(
    'Модель Whisper-turbo', v.whisperReady,
    () => window.jarvis.sttInstallWhisper(), 'Скачать (~574 МБ)',
  );
  // Qwen3: «готово» = сайдкар отвечает на health; если файлов нет — предлагаем
  // установить (venv + зависимости ~2.6 ГБ, веса догрузятся при первом запросе).
  sttModelRow(
    'Сайдкар Qwen3-ASR', v.qwen3Ready,
    v.qwen3Installed ? null : () => window.jarvis.sttInstallSidecar(),
    'Установить (~2.6 ГБ)',
    v.qwen3Installed ? 'установлен' : '—',
  );

  // строка прогресса скачивания/установки STT (обновляется событиями)
  const prog = document.createElement('div');
  prog.id = 'stt-install-progress';
  prog.className = 'ahint';
  prog.style.marginTop = '4px';
  box.appendChild(prog);

  // хоткей диктовки
  const hkRow = document.createElement('div');
  hkRow.className = 'arow hairtop';
  const hkLabel = document.createElement('span');
  hkLabel.className = 'alabel';
  hkLabel.textContent = `Зажми ${v.hotkey || 'F8'}, чтобы диктовать`;
  hkRow.appendChild(hkLabel);
  hkRow.appendChild(spacer());
  const hkCap = document.createElement('span');
  hkCap.className = 'keycap';
  hkCap.textContent = v.hotkey || 'F8';
  hkRow.appendChild(hkCap);
  box.appendChild(hkRow);

  // кнопка теста
  const testRow = document.createElement('div');
  testRow.className = 'abtnrow';
  const testBtn = document.createElement('button');
  testBtn.className = 'abtn small';
  testBtn.textContent = 'Тест (4 сек)';
  const resultEl = document.createElement('span');
  resultEl.className = 'ahint';
  resultEl.style.cssText = 'flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;';
  testBtn.addEventListener('click', async () => {
    testBtn.disabled = true;
    testBtn.textContent = 'Запись…';
    resultEl.textContent = '';
    try {
      const res = await window.jarvis.sttTest();
      if (res && res.ok) {
        resultEl.textContent = res.text || '(пусто)';
      } else {
        resultEl.textContent = res ? res.error : 'ошибка';
      }
    } catch (e) {
      resultEl.textContent = String(e);
    }
    testBtn.disabled = false;
    testBtn.textContent = 'Тест (4 сек)';
  });
  testRow.appendChild(testBtn);
  testRow.appendChild(resultEl);
  box.appendChild(testRow);
}

document.getElementById('diagnostics').addEventListener('change', (e) => {
  window.jarvis.setSettings({ diagnostics: e.target.checked });
});

for (const id of ['notifyDone', 'notifyWaiting', 'autoResume']) {
  document.getElementById(id).addEventListener('change', (e) => {
    window.jarvis.setSettings({ [id]: e.target.checked });
  });
}

// Автозапуск — отдельно: macOS может отказать (LaunchAgent), поэтому после
// переключения перечитываем РЕАЛЬНОЕ состояние из системы и честно говорим,
// если не сработало. Иначе галка «врёт», что включила.
document.getElementById('openAtLogin').addEventListener('change', async (e) => {
  const want = e.target.checked;
  await window.jarvis.setSettings({ openAtLogin: want });
  const s = await window.jarvis.getSettings();
  e.target.checked = !!s.openAtLogin; // отражаем то, что реально записалось в систему
  if (!!s.openAtLogin !== want) {
    showToast(want ? 'macOS не дала включить автозапуск' : 'Не вышло выключить автозапуск');
  } else {
    showToast(want ? 'Автозапуск включён' : 'Автозапуск выключен');
  }
});
document.getElementById('position').addEventListener('click', (e) => {
  const v = e.target.dataset ? e.target.dataset.v : null;
  if (!v) return;
  window.jarvis.setSettings({ position: v }).then(loadSettings);
});

/* рекордер хоткея */
const CODE_KEYS = { Space: 'Space', Enter: 'Enter', Backspace: 'Backspace', Tab: 'Tab' };

function accelFromEvent(e) {
  const mods = [];
  if (e.metaKey) mods.push('Command');
  if (e.ctrlKey) mods.push('Control');
  if (e.altKey) mods.push('Option');
  if (e.shiftKey) mods.push('Shift');
  if (!mods.some((m) => m !== 'Shift')) return null; // нужен не-Shift модификатор
  let key = null;
  if (/^Key[A-Z]$/.test(e.code)) key = e.code.slice(3);
  else if (/^Digit[0-9]$/.test(e.code)) key = e.code.slice(5);
  else if (/^F([1-9]|1[0-9]|2[0-4])$/.test(e.code)) key = e.code;
  else if (CODE_KEYS[e.code]) key = CODE_KEYS[e.code];
  if (!key) return null;
  return [...mods, key].join('+');
}

hotkeyBtn.addEventListener('click', () => startRecording(hotkeyBtn, 'hotkey'));
for (const btn of document.querySelectorAll('.keycap[data-hk]')) {
  btn.addEventListener('click', () => startRecording(btn, btn.dataset.hk));
}

/* ---------- клавиатура ---------- */

window.addEventListener('keydown', async (e) => {
  if (recording) {
    e.preventDefault();
    if (e.key === 'Escape') {
      recording = false;
      recordingBtn.classList.remove('recording');
      loadSettings();
      return;
    }
    const acc = accelFromEvent(e);
    if (!acc) return; // ждём полный аккорд
    recording = false;
    recordingBtn.classList.remove('recording');
    const res = await window.jarvis.setSettings({ [recordingKey]: acc });
    if (!res.ok) {
      hotkeyErr.textContent = res.error || 'Не удалось назначить';
      hotkeyErr.hidden = false;
    }
    loadSettings();
    return;
  }

  if (view === 'chat' && e.metaKey && e.key === 'Enter') { // ⌘↵ из чата — в терминал
    e.preventDefault();
    e.stopPropagation();
    if (chatSessionId) focusTerminal(chatSessionId, chatTitleEl.textContent);
    return;
  }

  if (actionsOpen()) { // меню действий: ↑↓ выбор, ↵ выполнить, esc/⌘K закрыть
    const items = actionItems();
    if (e.key === 'ArrowDown') { e.preventDefault(); apSel = Math.min(items.length - 1, apSel + 1); paintActions(items); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); apSel = Math.max(0, apSel - 1); paintActions(items); }
    else if (e.key === 'Enter') { e.preventDefault(); closeActions(); items[apSel] && items[apSel].run(); }
    else if (e.key === 'Escape' || (e.metaKey && (e.key === 'k' || e.key === 'K'))) { e.preventDefault(); closeActions(); }
    return;
  }

  if (e.metaKey && (e.key === 'k' || e.key === 'K')) { // ⌘K — меню действий
    e.preventDefault();
    toggleActions();
    return;
  }

  if (e.metaKey && e.key === '1') { // ⌘1 — Чаты
    e.preventDefault();
    setView('list');
    render();
    return;
  }
  if (e.metaKey && e.key === '2') { // ⌘2 — История
    e.preventDefault();
    setView('history');
    return;
  }
  if (e.metaKey && e.key === '3') { // ⌘3 — Статистика
    e.preventDefault();
    setView('stats');
    return;
  }

  // палитра быстрых команд: «/» в главном поиске (Часть 2). Раньше generic-Esc.
  if (view === 'list' && (argMode || queryEl.value.trim().startsWith('/'))) {
    if (argMode === 'amf') {
      if (e.key === 'Escape') { e.preventDefault(); exitArgToList(); return; }
      if (e.key === 'Enter') { e.preventDefault(); runArg(); return; }
      if (e.key === 'Tab') { e.preventDefault(); argFocus = argFocus === 'h' ? 'm' : 'h'; focusArgField(); return; }
      return; // цифры/Backspace идут в активное поле ввода
    }
    const matches = cmdMatches();
    cmdRootSel = Math.min(cmdRootSel, matches.length - 1);
    if (e.key === 'Escape') { e.preventDefault(); clearCmd(); return; }
    if (e.key === 'ArrowDown') { e.preventDefault(); palHoverEnabled = false; cmdRootSel = Math.min(matches.length - 1, cmdRootSel + 1); renderCmdPalette(); return; }
    if (e.key === 'ArrowUp') { e.preventDefault(); palHoverEnabled = false; cmdRootSel = Math.max(0, cmdRootSel - 1); renderCmdPalette(); return; }
    if (e.key === 'Tab') { e.preventDefault(); const c = matches[cmdRootSel]; if (c) { if (c.kind === 'amf') enterArg(); else { queryEl.value = c.cmd + ' '; renderCmdPalette(); } } return; }
    if (e.key === 'Enter') { e.preventDefault(); if (runRootCommand(queryEl.value)) return; const c = matches[cmdRootSel]; if (c) { if (c.kind === 'amf') enterArg(); else { queryEl.value = c.cmd + ' '; renderCmdPalette(); } } return; }
    return; // прочее (печать) идёт в #query
  }

  if (view === 'history') { // ↑↓ выбор · ↵ открыть проект / скопировать команду · esc — на уровень вверх
    if (e.key === 'ArrowDown') { e.preventDefault(); histSel = Math.min(histRows.length - 1, histSel + 1); paintHistSel(); return; }
    if (e.key === 'ArrowUp') { e.preventDefault(); histSel = Math.max(0, histSel - 1); paintHistSel(); return; }
    if (e.key === 'Enter' && histRows[histSel]) {
      e.preventDefault();
      const r = histRows[histSel];
      if (r.type === 'project') openHistProject(r.key);
      else copyResume(r.s, r.cwd);
      return;
    }
    if (e.key === 'Escape') {
      e.preventDefault();
      if (histProject != null) { histProject = null; renderHistory(); }
      else { setView('list'); render(); }
      return;
    }
    return; // прочее (печать в поиск) — пусть идёт в инпут
  }

  if (e.key === 'Escape') { // raycast: Esc — назад / закрыть
    if (view === 'chat' && paletteOpen()) return; // палитру закроет обработчик поля
    if (view !== 'list') { setView('list'); render(); }
    else window.jarvis.hidePanel();
    return;
  }

  // экран вопроса — только клавиатура
  if (view === 'question' && qData) {
    if (e.key === 'ArrowDown') { e.preventDefault(); qSel = Math.min(qData.options.length - 1, qSel + 1); paintQOptions(); return; }
    if (e.key === 'ArrowUp') { e.preventDefault(); qSel = Math.max(0, qSel - 1); paintQOptions(); return; }
    if (e.key === ' ') { e.preventDefault(); if (qData.multiSelect) toggleQ(qSel); return; }
    if (e.key === 'Enter') { e.preventDefault(); submitQ(); return; }
    if (/^[1-9]$/.test(e.key)) {
      const n = Number(e.key);
      if (n <= qData.options.length) { e.preventDefault(); qSel = n - 1; activateQ(); }
      return;
    }
    return; // прочие клавиши экран вопроса проглатывает
  }

  if (e.metaKey && e.key === ',') { // ⌘, — настройки, как в macOS
    e.preventDefault();
    if (view === 'settings') { setView('list'); render(); } else setView('settings');
    return;
  }

  if (e.metaKey && e.key === 'Backspace') { // ⌘⌫ — очистить завершённые
    e.preventDefault();
    window.jarvis.clearFinished();
    return;
  }

  if (e.metaKey && (e.key === 'p' || e.key === 'P')) { // ⌘P — закрепить/открепить
    e.preventDefault();
    const s = view === 'list' ? filtered()[sel]
      : view === 'chat' ? state.find((x) => x.id === chatSessionId)
      : null;
    if (s) window.jarvis.setPin(s.id, !s.pinned);
    return;
  }

  if (e.metaKey && (e.key === 'g' || e.key === 'G')) { // ⌘G — «где это?»: оверлей в терминале
    e.preventDefault();
    const s = view === 'list' ? filtered()[sel] : state.find((x) => x.id === chatSessionId);
    if (s) window.jarvis.pingTerminal(s.id).then((res) => {
      if (!res.ok) showToast(res.error || 'Не получилось');
    });
    return;
  }

  if (view === 'stats') { // ←→ период · 1-3 разрез · ↑↓ скролл
    if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
      e.preventDefault();
      statsPeriod = statsPeriod === 'today' ? 'week' : 'today';
      renderStats();
    } else if (/^[1-4]$/.test(e.key)) {
      e.preventDefault();
      statsDim = DIMS[Number(e.key) - 1][0];
      renderStats();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      statsEl.scrollBy({ top: e.key === 'ArrowDown' ? 80 : -80, behavior: 'smooth' });
    }
    return;
  }

  if (view === 'list') { // навигация по списку
    const list = filtered();
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      sel = Math.min(list.length - 1, sel + 1);
      render();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      sel = Math.max(0, sel - 1);
      render();
    } else if (e.key === 'Enter' && list.length) {
      e.preventDefault();
      if (e.metaKey) focusTerminal(list[sel].id, list[sel].project); // ⌘↵ — прыжок в терминал
      else openChat(list[sel].id, list[sel].project); // ↵ — чат сессии
    }
  }
}, true);
