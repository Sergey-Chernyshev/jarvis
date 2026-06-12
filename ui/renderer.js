/* Панель Jarvis: сессии, чат сессии, настройки. Все данные — через textContent, без innerHTML. */

const panelEl = document.getElementById('panel');
const listEl = document.getElementById('list');
const chatEl = document.getElementById('chat');
const chatlogEl = document.getElementById('chatlog');
const chatTitleEl = document.getElementById('chatTitle');
const chatChannelEl = document.getElementById('chatChannel');
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

// время старта сессии (createdAt): сегодня → ЧЧ:ММ, вчера → «вч ЧЧ:ММ», раньше → ДД.ММ
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
  if (sameDate(d, yest)) return `вч ${hm}`;
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
  else if (recording) { recording = false; hotkeyBtn.classList.remove('recording'); }
  if (next === 'list') queryEl.focus();
}

// Клик/Enter по сессии: есть вопрос → клавиатурный пикер, иначе чат
function openSession(s) {
  if (s.question && s.question.questions && s.question.questions.length) openQuestion(s);
  else openChat(s.id, s.project);
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
  return [...ordered.filter((s) => s.pinned), ...ordered.filter((s) => !s.pinned)];
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

  // если в одном проекте несколько сессий — различаем их tmux-именем
  const projCount = {};
  for (const s of list) projCount[s.project] = (projCount[s.project] || 0) + 1;

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

    const badge = document.createElement('span');
    badge.className = 'badge';
    badge.textContent = s.model || s.agent || 'claude'; // модель — из транскрипта, бесплатно

    const host = hostLabel(s);
    let hostBadge = null;
    if (host) {
      hostBadge = document.createElement('span');
      hostBadge.className = 'badge host';
      hostBadge.textContent = host;
    }

    let tmuxBadge = null;
    if (projCount[s.project] > 1 && s.tmuxName) {
      tmuxBadge = document.createElement('span');
      tmuxBadge.className = 'badge host';
      tmuxBadge.textContent = s.tmuxName;
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
    row.appendChild(badge);
    if (hostBadge) row.appendChild(hostBadge);
    if (tmuxBadge) row.appendChild(tmuxBadge);
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
      if (sel !== i) { sel = i; render(); }
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

function renderMarkdown(root, text) {
  const para = [];
  const code = [];
  let inCode = false;
  let ul = null;
  let callout = null; // открытый блок `★ Insight ───…`

  const target = () => callout || root;

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

    // `★ Insight ───` → открытие callout-карточки
    const co = line.match(/^\s*`?\s*[★✦☆]\s*([^─━`]*?)\s*[─━]{3,}\s*`?\s*$/);
    if (co) {
      flushPara();
      ul = null;
      callout = document.createElement('div');
      callout.className = 'callout';
      const title = document.createElement('div');
      title.className = 'callout-title';
      title.textContent = `★ ${co[1].trim() || 'Insight'}`;
      callout.appendChild(title);
      root.appendChild(callout);
      continue;
    }
    // линия из ─ : закрытие callout либо просто разделитель
    if (/^\s*`?\s*[─━]{3,}\s*`?\s*$/.test(line)) {
      flushPara();
      ul = null;
      if (callout) callout = null;
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
}

/* --- лента чата: подряд идущие тулзы группируются в чипы, повторы ×N --- */

let toolsGroup = null; // текущая группа чипов (обнуляется текстовой репликой)

function addToolChip(label) {
  if (!toolsGroup) {
    toolsGroup = document.createElement('div');
    toolsGroup.className = 'msg tools';
    chatlogEl.appendChild(toolsGroup);
  }
  const last = toolsGroup.lastElementChild;
  if (last && last.dataset.label === label) {
    const n = (Number(last.dataset.count) || 1) + 1;
    last.dataset.count = String(n);
    last.textContent = `${label} ×${n}`;
    return;
  }
  const chip = document.createElement('span');
  chip.className = 'chip';
  chip.dataset.label = label;
  chip.title = label;
  chip.textContent = label;
  toolsGroup.appendChild(chip);
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
    const msg = document.createElement('div');
    msg.className = `msg ${it.role === 'user' ? 'user' : 'assistant'}`;
    const bubble = document.createElement('div');
    bubble.className = 'bubble';
    if (it.role === 'user') bubble.textContent = it.text;
    else renderMarkdown(bubble, it.text);
    msg.appendChild(bubble);
    chatlogEl.appendChild(msg);
  }
  if (items.length && nearBottom) chatlogEl.scrollTop = chatlogEl.scrollHeight;
}

function updateChatChannelMark() {
  const s = state.find((x) => x.id === chatSessionId);
  // tmux-сессии — без пометки; вне tmux помечаем
  chatChannelEl.hidden = !s || !!s.tmuxPane;
  const sub = (s && (s.task || s.summary || s.title)) || '';
  const subEl = document.getElementById('chatSub');
  subEl.textContent = sub;
  // расход сессии — в той же строке, тихо
  if (s) {
    window.jarvis.getSessionUsage(s.id).then((us) => {
      if (!us || chatSessionId !== s.id) return;
      const money = (us.billing && us.billing !== 'plan') ? `$${us.cost.toFixed(2)}` : `~$${us.cost.toFixed(2)}`;
      subEl.textContent = `${sub ? sub + ' · ' : ''}${fmtTok(us.tok)} ткн · ${money}`;
    }).catch(() => {});
  }
  gateReply(s);
  updateChatStatus(s);
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

function paintQOptions() {
  for (const [i, btn] of [...qOptsEl.children].entries()) {
    btn.classList.toggle('sel', i === qSel);
    btn.classList.toggle('chosen', qChosen.has(i + 1));
  }
  qOptsEl.children[qSel]?.scrollIntoView({ block: 'nearest' });
}

function renderQuestion() {
  qHeaderEl.textContent = qData.header || '';
  qHeaderEl.hidden = !qData.header;
  qTitleEl.textContent = qData.question;

  qOptsEl.textContent = '';
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
    qOptsEl.appendChild(btn);
  });
  paintQOptions();

  qFootEl.textContent = '';
  const hint = (cap, text) => {
    const h = document.createElement('span');
    h.appendChild(keycap(cap));
    h.appendChild(document.createTextNode(text));
    return h;
  };
  qFootEl.appendChild(hint('↑↓', 'выбрать'));
  if (qData.multiSelect) {
    qFootEl.appendChild(hint('␣', 'отметить'));
    qFootEl.appendChild(hint('↵', 'отправить'));
  } else {
    qFootEl.appendChild(hint('↵', 'ответить'));
    qFootEl.appendChild(hint('1–9', 'быстрый выбор'));
  }
  qFootEl.appendChild(hint('esc', 'назад'));
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
  if (res.ok) { setView('list'); render(); }
  else showToast(res.error || 'Не удалось ответить');
}

document.getElementById('qBack').addEventListener('click', () => { setView('list'); render(); });

async function openChat(sessionId, project) {
  const res = await window.jarvis.openChat(sessionId);
  if (!res.ok) { showToast(res.error || 'Не удалось открыть чат'); return; }
  chatSessionId = sessionId;
  chatTitleEl.textContent = res.project || project || '';
  updateChatChannelMark();
  chatlogEl.textContent = '';
  toolsGroup = null;
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

document.getElementById('chatBack').addEventListener('click', () => { setView('list'); render(); });

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

/** строка «Сейчас: <статус>» с кнопкой действия — видно, работает режим или нет */
function statusRow(valueText, on, btnLabel, onClick) {
  const row = document.createElement('div');
  row.className = 'srow sub';
  const lab = document.createElement('span');
  lab.className = 'slabel';
  lab.style.opacity = '0.6';
  lab.textContent = 'Сейчас';
  row.appendChild(lab);
  const val = document.createElement('span');
  val.className = on ? 'sval on' : 'sval';
  val.textContent = valueText;
  row.appendChild(val);
  const sp = document.createElement('span');
  sp.className = 'spacer';
  row.appendChild(sp);
  if (btnLabel) {
    const btn = document.createElement('button');
    btn.className = 'keycap';
    btn.textContent = btnLabel;
    btn.addEventListener('click', onClick);
    row.appendChild(btn);
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

function renderPluginRows() {
  const box = document.getElementById('pluginRows');
  if (!box) return;
  box.textContent = '';

  const ka = pluginById('keep-awake');
  if (ka) {
    box.appendChild(srow('☕ Не спать', stoggle(ka.enabled, (v) => pluginCmd(ka.id, '_enable', { on: v })),
      { hint: 'плагин: вето на сон мака' }));
    if (ka.enabled && ka.status) {
      const st = ka.status;
      box.appendChild(statusRow(
        st.active ? (st.line || 'включено') : 'выключено — мак спит как обычно',
        st.active,
        st.manual ? 'Выключить' : 'Включить ∞',
        () => pluginCmd(ka.id, st.manual ? 'stop' : 'start-manual')));
      box.appendChild(srow('Авто: держать, пока агенты работают',
        stoggle(st.autoEnabled, (v) => pluginCmd(ka.id, 'set', { auto: v })), { dim: true, sub: true }));
      box.appendChild(srow('Не гасить экран',
        stoggle(st.keepDisplayOn, (v) => pluginCmd(ka.id, 'set', { keepDisplayOn: v })), { dim: true, sub: true }));
    }
  }

  const cs = pluginById('clamshell');
  if (cs) {
    box.appendChild(srow('⌒ Крышка', stoggle(cs.enabled, (v) => pluginCmd(cs.id, '_enable', { on: v })),
      { hint: 'плагин: работа с закрытой крышкой' }));
    if (cs.enabled && cs.status) {
      const st = cs.status;
      box.appendChild(statusRow(
        st.armed ? 'мак не уснёт даже с закрытой крышкой' : 'выключено — закроешь крышку, мак уснёт',
        st.armed,
        st.armed ? 'Выключить' : 'Включить',
        () => pluginCmd(cs.id, st.armed ? 'disarm' : 'arm')));
      box.appendChild(srow('Подсказывать после прерванного сна',
        stoggle(st.suggest, (v) => pluginCmd(cs.id, 'set', { suggest: v })), { dim: true, sub: true }));
      box.appendChild(srow('Авто: вместе с «Не спать»',
        stoggle(st.autoArm, (v) => pluginCmd(cs.id, 'set', { autoArm: v }), !st.sudoers),
        { dim: true, sub: true, hint: st.sudoers ? '' : 'нужен тихий режим' }));
      if (!st.sudoers) {
        const btn = document.createElement('button');
        btn.className = 'keycap';
        btn.textContent = 'Настроить…';
        btn.addEventListener('click', () => pluginCmd(cs.id, 'install-sudoers'));
        box.appendChild(srow('Тихое переключение без пароля (sudoers)', btn, { dim: true, sub: true }));
      }
    }
  }
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
  panelEl.classList.add('entering');
  requestAnimationFrame(() => requestAnimationFrame(() => {
    panelEl.classList.remove('entering');
  }));
  queryEl.value = '';
  sel = 0;
  rebuildOrder();
  setView('list');
  render();
});
requestAnimationFrame(() => requestAnimationFrame(() => {
  panelEl.classList.remove('entering');
}));

queryEl.addEventListener('input', () => {
  if (view === 'history') { renderHistory(); return; }
  sel = 0;
  render();
});

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

/* ---------- настройки ---------- */

const hotkeyBtn = document.getElementById('hotkey');
const hotkeyErr = document.getElementById('hotkeyError');
let recording = false;

function displayHotkey(acc) {
  return acc
    .replace('CommandOrControl', '⌘').replace('Command', '⌘')
    .replace('Control', '⌃').replace('Option', '⌥').replace('Alt', '⌥')
    .replace('Shift', '⇧').replaceAll('+', ' ');
}

async function loadSettings() {
  const s = await window.jarvis.getSettings();
  hotkeyBtn.textContent = displayHotkey(s.hotkey);
  document.getElementById('notifyDone').checked = s.notifyDone;
  document.getElementById('notifyWaiting').checked = s.notifyWaiting;
  document.getElementById('autoResume').checked = s.autoResume !== false;
  document.getElementById('openAtLogin').checked = s.openAtLogin;
  for (const b of document.querySelectorAll('.segbtn')) {
    b.classList.toggle('active', b.dataset.v === s.position);
  }
  try {
    plugins = await window.jarvis.getPlugins();
    renderPluginRows();
  } catch {}
}

for (const id of ['notifyDone', 'notifyWaiting', 'autoResume', 'openAtLogin']) {
  document.getElementById(id).addEventListener('change', (e) => {
    window.jarvis.setSettings({ [id]: e.target.checked });
  });
}
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

hotkeyBtn.addEventListener('click', () => {
  recording = true;
  hotkeyErr.hidden = true;
  hotkeyBtn.classList.add('recording');
  hotkeyBtn.textContent = 'нажми сочетание…';
});

/* ---------- клавиатура ---------- */

window.addEventListener('keydown', async (e) => {
  if (recording) {
    e.preventDefault();
    if (e.key === 'Escape') {
      recording = false;
      hotkeyBtn.classList.remove('recording');
      loadSettings();
      return;
    }
    const acc = accelFromEvent(e);
    if (!acc) return; // ждём полный аккорд
    recording = false;
    hotkeyBtn.classList.remove('recording');
    const res = await window.jarvis.setSettings({ hotkey: acc });
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
