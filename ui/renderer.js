/* Панель Jarvis: сессии, чат сессии, настройки. Все данные — через textContent, без innerHTML. */

// Каталог агентов: модели, усилия, умения — оттуда, а не тернарником по id.
const AGENTS = window.JarvisAgents;

const panelEl = document.getElementById('panel');
const listEl = document.getElementById('list');
const chatEl = document.getElementById('chat');
const chatlogEl = document.getElementById('chatlog');
const chatTitleEl = document.getElementById('chatTitle');
const chatChannelEl = document.getElementById('chatChannel');
const chatModelEl = document.getElementById('chatModel');
const chatRemoteEl = document.getElementById('chatRemote');
const chatDotEl = document.getElementById('chatDot');
const settingsEl = document.getElementById('settings');
const queryEl = document.getElementById('query');
const replyEl = document.getElementById('reply');
const chatAttachEl = document.getElementById('chatAttach');

// Вставленные в поле ответа картинки, ждущие отправки. Каждая — { id, ext,
// dataUrl } (base64 для IPC режется из dataUrl на отправке — не храним дважды).
// На отправке пишутся во временные файлы, пути уходят в промпт.
let pendingImages = [];
let attachSeq = 0;
const MAX_IMAGES = 8;
// Расширения, с которыми session_save_image (Rust) пишет файлы на диск, — единый
// список для чип-regex и extFromType; источник истины для записи — белый список в ipc.rs.
const IMG_EXTS = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'heic', 'tiff', 'tif'];
const PASTED_IMG_RE = new RegExp(String.raw`(^|\s)(\/[^\s]*\/jarvis-paste\/[^\s]+\.(?:${IMG_EXTS.join('|')}))\b`, 'gi');

// Фокус в редактируемом поле? Тогда нативные текст-комбо (⌘⌫ = удалить до начала
// строки) НЕ должны перехватываться глобальными хоткеями приложения — иначе ⌘⌫ при
// печати в чате стирал завершённые сессии (clearFinished). См. обработчик ниже.
function editingText(el = document.activeElement) {
  if (!el) return false;
  return window.jarvisKeys.editing(el);
}

/* Поле, которое разбирает простые клавиши само (правка имени чата, путь
 * «Нового проекта», «что сделать»). stopPropagation в таком поле бесполезен:
 * главный обработчик панели висит на window в фазе ПЕРЕХВАТА и отрабатывает
 * раньше — Esc успевал спрятать всю панель, ↵ провалиться в чат, ↑↓ перерисовать
 * список и убить поле под курсором. Помечаем поле, а обработчик его уступает. */
function ownsKeys(el) {
  return !!(el && el.dataset && el.dataset.ownkeys !== undefined);
}
// Пометить поле как разбирающее клавиши само. Гасить всплытие всё равно нужно
// отдельно — для локальных обработчиков разделов, слушающих window по-простому.
function claimKeys(el) {
  el.dataset.ownkeys = '';
  return el;
}
const footerLeftEl = document.getElementById('footerLeft');
const tabSessionsEl = document.getElementById('tabSessions');
const tabSettingsEl = document.getElementById('tabSettings');
const voicehistEl = document.getElementById('voicehist');
const loopsEl = document.getElementById('loops');
const tabLoopsEl = document.getElementById('tabLoops');
const bundlePaneEl = document.getElementById('bundlePane');
const tabBundleEl = document.getElementById('tabBundle');
const tabVoiceEl = document.getElementById('tabVoice');
const agentPaneEl = document.getElementById('agentPane');
const tabAgentEl = document.getElementById('tabAgent');
// Колонка чатов Джарвиса и её домашнее место внутри переписки (см. dockAgentSide)
const agChatsEl = document.getElementById('agChats');
const agWrapEl = agentPaneEl.querySelector('.agwrap');

const STATUS_LABEL = {
  working: 'работает',
  waiting: 'ждёт тебя',
  done: 'готово',
  idle: 'простаивает',
  limit: 'лимит — ждёт сброса',
};

/* Сессия не закончила, а умерла. Честного Status::Failed в модели нет: демон
 * ставит упавшей сессии Done с этой пометкой в detail, и в списке она выглядит
 * ровно как успешно доработавшая. Пока состояние не завели в Rust, различаем
 * по detail — врать «готово» про упавшую хуже, чем опереться на строку. */
const FAILED_RE = /останов|упал|оборв|не отвеча/i;
const failedSession = (s) => !!(s && s.status === 'done' && FAILED_RE.test(s.detail || ''));

// Состояние словами — для title, aria-label и вообще везде, где точки мало.
const statusWord = (s) =>
  (failedSession(s) ? 'упала' : STATUS_LABEL[s && s.status]) || '';

let state = [];
let sel = 0;
let view = 'home'; // root command list, then one module at a time
const navigation = window.createJarvisNavigation({ view: 'home' });
let chatSessionId = null;
let chatOpenSequence = 0;
let pendingChat = null;
let stateReceived = false;
let sessionsLoadState = 'loading';
let sessionsLoadError = '';
const deliveryStates = new Map();

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

const contentEl = document.getElementById('content');
const detailEmptyEl = document.getElementById('detailEmpty');

/** Оконный режим (14h) против накладки ⌘J. Источник правды — атрибут на <html>,
 *  который ставит theme.js из настроек; так режим виден и до ответа моста. */
const windowMode = () => document.documentElement.dataset.mode === 'window';

/** Разделы, при которых список слева что-то значит: только в них его и стоит
 *  пересобирать на пуш состояния (см. render). */
const LIST_VIEWS = new Set(['list', 'chat', 'question']);

/** Недописанные ответы по сессиям: id → текст поля. */
const chatDrafts = new Map();

/* Вкладка «Джарвис» — отдельный экран на всё окно, а не третья колонка внутри
 * переписки. В оконном режиме его чаты занимают ЛЕВУЮ колонку окна вместо
 * списка сессий: сетка кладёт в область `list` только своих детей, поэтому
 * колонку и переселяем. В накладке и в окне из трея левой колонки нет — там она
 * остаётся дома, внутри .agwrap. */
let agentTab = null; // рукоятка смонтированной вкладки (agent-chat.js)
function dockAgentSide(on) {
  const host = on ? panelEl : agWrapEl;
  if (agChatsEl.parentElement !== host) {
    // в переписке колонка идёт ПЕРЕД лентой, в сетке порядок решают области
    if (on) panelEl.insertBefore(agChatsEl, listEl.nextSibling);
    else agWrapEl.insertBefore(agChatsEl, agWrapEl.firstChild);
  }
  const was = agChatsEl.dataset.dock;
  agChatsEl.dataset.dock = on ? '1' : '0';
  // Класс ставим здесь же, а не ждём первой отрисовки колонки: неразмеченный
  // grid-элемент сетка разложила бы сама — куда попало.
  agChatsEl.classList.toggle('docked', on);
  if (was !== agChatsEl.dataset.dock && agentTab) agentTab.redock();
}

/* Поиск в шапке: в режиме Джарвиса он ищет по ЕГО чатам. Двух почти одинаковых
 * полей рядом не бывает — сессий на экране нет, и искать общему полю нечего, а
 * колонка своего поля больше не рисует (agent-chat.js, extFind). */
const QUERY_PLACEHOLDER = queryEl.placeholder;
let listQuery = ''; // поиск по сессиям — ждёт возврата таким, каким его оставили
let agentQuery = ''; // ...и поиск по чатам Джарвиса тоже: это два разных поиска

function routeSnapshot() {
  return { view, query: queryEl.value, sel, sessionId: chatSessionId,
    project: chatTitleEl.textContent, draft: view === 'chat' ? replyEl.value : undefined,
    moduleSelection: window.jarvisWorkspace?.selection(),
    projects: view === 'history' ? window.jarvisProjects?.snapshot() : undefined,
    history: view === 'history' ? { machine: histMachine, project: histProject, selected: histSel, trail: histTrail.map(route => ({ ...route })) } : undefined,
    focusId: document.activeElement?.id || null };
}

function goBack() {
  if (view === 'history' && window.jarvisProjects) {
    if (window.jarvisProjects.back()) return;
  } else
  if (view === 'history') {
    if (histNewOpen) { histNewOpen = false; renderHistory(); return; }
    if (histBack()) return;
  }
  if (window.jarvisModuleBack?.[view]?.()) return;
  if (view === 'home') {
    if (queryEl.value) { queryEl.value = ''; window.jarvisWorkspace.refresh(); queryEl.focus(); }
    else if (!windowMode()) window.jarvis.hidePanel();
    return;
  }
  const route = navigation.back();
  if (route.view === 'chat' && state.some(s => s.id === route.sessionId)) {
    openChat(route.sessionId, route.project, { restore: route });
  } else {
    setView(route.view === 'chat' || route.view === 'question' ? 'home' : route.view, { restore: route });
    render();
  }
}

function setView(next, options = {}) {
  const prev = view;
  const previousQuery = queryEl.value;
  const changed = view !== next;
  if (options.restore) navigation.replace({ ...options.restore, view: next });
  else navigation.go({ view: next, sessionId: next === 'chat' ? chatSessionId : undefined }, options.fromRoute || routeSnapshot());
  if (changed) queryEl.value = options.restore?.query || '';
  if (options.restore?.sel != null) sel = options.restore.sel;
  if (next !== 'chat') {
    if (pendingChat && view !== 'chat') window.jarvis.closeChat();
    chatOpenSequence++; pendingChat = null;
  }
  if (view === 'chat' && next !== 'chat') {
    window.jarvisSessionWorkspace?.saveDraft(chatSessionId, replyEl.value, pendingImages);
    window.jarvis.closeChat();
    chatSessionId = null;
  }
  if (view === 'question' && next !== 'question') qSessionId = null;
  // вкладка всегда открывается с верхнего уровня: выбор машины (или сразу
  // проекты локальной, если узлов не настроено)
  if (next === 'history' && changed) {
    window.jarvisProjects?.enter(options.restore?.projects, changed);
    histProject = options.restore?.history?.project || null;
    histMachine = options.restore?.history?.machine || null;
    histSel = options.restore?.history?.selected || 0;
    histTrail = options.restore?.history?.trail?.map(route => ({ ...route })) || [];
    histNewOpen = false;
  }
  view = next;
  window.jarvisWorkspace?.changed(next, options.restore);
  window.jarvisSessionWorkspace?.changed(next);
  closeActions();
  // Оконный режим (14h): список слева живёт всегда — кроме экрана Джарвиса, где
  // левая колонка отдана его чатам; поиск и вкладки на месте всегда.
  // В накладке остаётся прежний фокус-режим: чат и вопрос занимают панель целиком.
  const win = windowMode();
  document.querySelector('.cmdrow').hidden = !['home', 'list', 'history', 'agent'].includes(next);
  queryEl.placeholder = next === 'home' ? 'Найти команду или чат…' : next === 'history' ? 'Найти проект…' : 'Найти чат…';
  document.getElementById('pageNavigation').hidden = next === 'home';
  document.getElementById('launcher').hidden = next !== 'home';
  listEl.hidden = next !== 'list';
  contentEl.hidden = ['home', 'list'].includes(next);
  detailEmptyEl.hidden = true;
  // Уходим от Джарвиса — снимаем прокрутку его ленты: спрятанному узлу браузер
  // обнуляет scrollTop, и возврат кидал бы в конец переписки.
  if (prev === 'agent' && next !== 'agent' && agentTab) agentTab.park();
  // Поиск меняет адресата вместе с экраном (см. QUERY_PLACEHOLDER выше)
  if (next === 'agent' && prev !== 'agent') {
    listQuery = previousQuery;
    queryEl.value = agentQuery;
    queryEl.placeholder = 'Найти чат Джарвиса…';
  } else if (prev === 'agent' && next !== 'agent') {
    agentQuery = previousQuery;
    queryEl.value = listQuery;
    // The destination view already set its own placeholder above.
  }
  /* Где живёт колонка чатов Джарвиса, решает режим окна, а не вкладка: в сетке
   * она просто ждёт своего экрана спрятанной. Зато КОЛОНКА СЕТКИ отдаётся ему
   * только на его экране — по этой метке ширину тянут за границу сетки. */
  dockAgentSide(win);
  agChatsEl.hidden = win && next !== 'agent';
  panelEl.dataset.agent = win && next === 'agent' ? '1' : '0';
  chatEl.hidden = next !== 'chat';
  qviewEl.hidden = next !== 'question';
  settingsEl.hidden = next !== 'settings';
  document.getElementById('machines').hidden = next !== 'machines';
  statsEl.hidden = next !== 'stats';
  voicehistEl.hidden = next !== 'voicehist';
  document.getElementById('meetings').hidden = next !== 'meetings';
  loopsEl.hidden = next !== 'loops';
  bundlePaneEl.hidden = next !== 'bundle';
  agentPaneEl.hidden = next !== 'agent';
  historyEl.hidden = next !== 'history';
  // чат и вопрос несут собственные нижние бары — парящий футер только тут.
  // В окне полоска не парит, а стоит в сетке под обеими колонками — она нужна всегда.
  footerEl.hidden = !win && (next === 'chat' || next === 'question');
  if (next === 'home') { primaryLabelEl.textContent = 'Открыть'; primaryKeyEl.textContent = '↵'; }
  else if (next === 'list') { primaryLabelEl.textContent = 'Открыть чат'; primaryKeyEl.textContent = '↵'; }
  else if (next === 'history') { primaryLabelEl.textContent = 'Открыть проект'; primaryKeyEl.textContent = '↵'; }
  else { primaryLabelEl.textContent = 'Назад'; primaryKeyEl.textContent = 'esc'; }
  tabSettingsEl.classList.toggle('active', next === 'settings');
  document.getElementById('tabMachines').classList.toggle('active', next === 'machines');
  document.getElementById('tlSettings').classList.toggle('active', next === 'settings');
  tabStatsEl.classList.toggle('active', next === 'stats');
  tabHistoryEl.classList.toggle('active', next === 'history');
  tabVoiceEl.classList.toggle('active', next === 'voicehist');
  tabLoopsEl.classList.toggle('active', next === 'loops');
  tabBundleEl.classList.toggle('active', next === 'bundle');
  tabAgentEl.classList.toggle('active', next === 'agent');
  tabSessionsEl.classList.toggle('active', next === 'list' || next === 'chat');
  // Каждый раздел поднимается в своей обёртке: исключение в одном не должно
  // оставлять панель с белым экраном — раньше первая же ошибка обрывала
  // setView, и соседние разделы переставали показываться вместе с ним.
  const safely = (what, fn, host) => {
    const at = Date.now();
    const blame = (e) => {
      console.error(`[view:${what}]`, e);
      try { window.jarvis.reportError(`view:${what}`, (e && e.stack) || e); } catch (_) { /* лог не обязателен */ }
    };
    // Молчаливые беды не менее вредны, чем исключения: раздел может отрисоваться
    // пустым или отрисовываться десять секунд, и об этом не узнает никто, кроме
    // человека перед экраном. Поэтому меряем и считаем нарисованное.
    const audit = () => {
      const ms = Date.now() - at;
      if (!host || !host.isConnected || host.hidden || view !== next) return;
      const kids = host.childElementCount;
      // Высота нужна отдельно от числа детей: «белый экран» бывает и при
      // непустом DOM — когда раздел отрисовался, но схлопнут в нулевую высоту
      // или спрятан. Одно число этих двух случаев не различает, а чинятся они
      // в разных местах.
      const h = Math.round(host.getBoundingClientRect().height);
      const bad = kids === 0 || h === 0;
      // Только о беде: раздел пуст, схлопнут в ноль или рисовался слишком
      // долго. Строка на каждый переход была нужна, пока искали зависание, —
      // теперь это лишний шум в логе у всех.
      if (!bad && ms < 1500) return;
      try {
        window.jarvis.reportError(
          `view:${what}`,
          `дети=${kids} высота=${h}px за ${ms} мс${host.hidden ? ' (скрыт)' : ''}`,
        );
      } catch (_) { /* лог не обязателен */ }
    };
    try {
      const r = fn();
      // Разделы бывают асинхронными: у них ошибка приходит отказом обещания,
      // и обычный catch её не увидит.
      if (r && typeof r.then === 'function') r.then(audit, blame);
      else audit();
    } catch (e) { blame(e); }
  };
  if (next === 'settings') safely('settings', loadSettings);
  if (next === 'machines') safely('machines', () => window.initMachines(document.getElementById('machines')), document.getElementById('machines'));
  if (next === 'meetings') safely('meetings', () => window.initMeetings(document.getElementById('meetings')), document.getElementById('meetings'));
  if (next === 'stats') safely('stats', renderStats, statsEl);
  if (next === 'voicehist') {
    voicehistEl.style.cssText = 'padding:0;height:100%;overflow:hidden';
    safely('voicehist', () => window.initVoiceHistory(voicehistEl));
  }
  if (next === 'loops') {
    // Режим живёт своим модулем: панель только даёт ему место и уходит.
    safely('loops', () => window.initLoops(loopsEl), loopsEl);
  }
  if (next === 'bundle') {
    safely('bundle', () => window.initBundle(bundlePaneEl), bundlePaneEl);
  }
  // Разговор с главным агентом: разметка своя, вся логика — в agent-chat.js,
  // общем с окном из трея. Монтируется один раз, дальше только фокус.
  if (next === 'agent') safely('agent', () => { agentTab = window.initAgentChat(agentPaneEl); }, agentPaneEl);
  if (next === 'history') safely('history', renderHistory, historyEl);
  else if (recording) { recording = false; recordingBtn.classList.remove('recording'); }
  if (['home', 'list', 'history'].includes(next)) queryEl.focus();
  else document.getElementById('pageBack').focus();
  if (options.restore?.focusId) requestAnimationFrame(() => {
    const target = document.getElementById(options.restore.focusId);
    if (target && !target.closest('[hidden]')) target.focus();
  });
}

// Клик/Enter по сессии: всегда открываем чат; если у сессии есть вопрос —
// сразу поднимаем слайд-овер вариантов поверх чата (видно переписку И варианты).
function openSession(s) {
  openChat(s.id, s.project).then(() => {
    if (chatSessionId === s.id && view === 'chat' && questionOf(s)) openVarPanel();
  });
}

/* Открыть чат по id сессии — для чужих модулей (карточки связки). */
window.openSessionById = (id) => {
  const s = state.find((x) => x.id === id);
  if (s) openSession(s);
};

/* ---------- список сессий ---------- */

// TERM_PROGRAM сессии → человеческое имя. Значения маковских терминалов
// (iTerm.app, Apple_Terminal) остаются: сессия могла прийти и с мака.
const HOST_LABEL = {
  'iTerm.app': 'iTerm',
  Apple_Terminal: 'Terminal',
  vscode: 'VS Code',
  'JetBrains-JediTerm': 'JetBrains',
  WezTerm: 'WezTerm',
  ghostty: 'Ghostty',
  // Linux-эмуляторы
  'gnome-terminal': 'GNOME Terminal',
  konsole: 'Konsole',
  kitty: 'kitty',
  alacritty: 'Alacritty',
  foot: 'foot',
  tilix: 'Tilix',
  terminator: 'Terminator',
  'xfce4-terminal': 'Xfce Terminal',
  ptyxis: 'Ptyxis',
  xterm: 'xterm',
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
  // имя чата ищется наравне с проектом: его для того и дают, чтобы находить
  return ordered.filter((s) =>
    `${s.project || ''} ${s.name || ''} ${s.detail || ''} ${s.agent || ''} ${s.remote || ''}`.toLowerCase().includes(q));
}

function render() {
  window.jarvisSessionWorkspace?.refresh(state);
  window.jarvisProjects?.stateChanged();
  if (view === 'home') { window.jarvisWorkspace?.refresh(); footerLeftEl.textContent = footerText(); return; }
  // Гейт — видимость списка, а не имя вида. В оконном режиме список стоит
  // рядом с открытым чатом, и прежняя проверка «вид — не список» замораживала
  // его на всё время чата: статусы не обновлялись, а открытый чат никак не
  // выделялся среди строк.
  if (listEl.hidden) return;
  // …но в оконном режиме список прячется только на экране Джарвиса, а при всех
  // прочих разделах он виден — и пересобирается целиком до восьми раз в
  // секунду, включая время, когда человек в настройках или статистике и на
  // список даже не смотрит. Разделы, при
  // которых список неактуален, перерисовку не заказывают; вернётся человек —
  // setView позовёт render() сам.
  if (!LIST_VIEWS.has(view)) return;
  // пока правят имя — список не трогаем: пуш состояния прилетает несколько раз
  // в секунду и стёр бы поле вместе с курсором
  if (renaming) return;
  // hover-выбор разоружаем на каждую перерисовку: дальше его снова взведёт только
  // реальное mousemove (см. listEl.mousemove). Иначе фон-обновления (data push)
  // пересоздают строки под неподвижным курсором → mouseenter таскает выделение.
  palHoverEnabled = false;
  // палитра быстрых команд: «/» в главном поиске вместо списка сессий.
  // Только в виде списка: в оконном режиме рядом с чатом список остаётся
  // списком — палитре там рисоваться не на чем.
  if (view === 'list' && (argMode || queryEl.value.trim().startsWith('/'))) { renderCmdPalette(); return; }
  argMode = null;
  listEl.textContent = '';

  footerLeftEl.textContent = footerText();

  const list = filtered();
  sel = Math.min(sel, Math.max(0, list.length - 1));

  // Открытый чат и есть выделение: курсор списка ведём за ним, а не отдельно.
  // Заодно Esc из чата возвращает к той же строке, а не куда курсор укатился.
  const openId = view === 'chat' ? chatSessionId : view === 'question' ? qSessionId : null;
  if (openId) {
    const oi = list.findIndex((x) => x.id === openId);
    if (oi >= 0) sel = oi;
  }

  if (!list.length) {
    const empty = document.createElement('div');
    empty.className = 'empty';
    // перечисляем те CLI, что пульт действительно ведёт, а не один claude:
    // список берём из каталога агентов, чтобы он не разъезжался с ним
    empty.textContent = state.length
      ? 'Ничего не найдено'
      : `Нет активных сессий — запусти ${AGENTS.present().map((a) => a.id).join(', ')} в любом терминале, они появятся здесь сами.`;
    listEl.appendChild(empty);
    return;
  }

  list.forEach((s, i) => {
    const row = document.createElement('div');
    row.setAttribute('role', 'button');
    row.tabIndex = 0;
    row.addEventListener('keydown', e => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); openSession(s); } });
    row.className = `row ${s.status}${failedSession(s) ? ' failed' : ''}${i === sel ? ' selected' : ''}`;
    row.dataset.sid = s.id; // по нему правка имени находит свою строку
    row.title = [statusWord(s), s.remote ? `узел ${s.remote}` : null, s.cwd, s.title, ...(s.todoList || [])]
      .filter(Boolean).join('\n');

    const dot = document.createElement('span');
    dot.className = 'dot';
    // состояние словами: точка различает форму и краску, но пара «лимит» и
    // «ждёт» по яркости — 1.51:1, а дальтонику это одна и та же точка
    dot.title = statusWord(s);
    dot.setAttribute('aria-label', statusWord(s));

    const name = document.createElement('span');
    name.className = 'name';
    name.textContent = s.project || '?';

    let branch = null;
    if (s.branch) {
      branch = document.createElement('span');
      branch.className = 'gitbranch';
      branch.textContent = `⎇ ${s.branch}`;
    }

    // пилл у всех, кроме агента по умолчанию: для claude как раньше
    let agentBadge = null;
    if (s.agent && s.agent !== AGENTS.DEFAULT_ID) {
      agentBadge = document.createElement('span');
      agentBadge.className = 'badge agent';
      agentBadge.textContent = AGENTS.title(s.agent).toLowerCase();
    }

    // сессия живёт не на этой машине: бейдж с именем узла рядом с бейджем агента.
    // Контурный, а не залитый — как точка «закончила»: другая машина, не другой цвет.
    let remoteBadge = null;
    if (s.remote) {
      remoteBadge = document.createElement('span');
      remoteBadge.className = 'badge remote';
      remoteBadge.textContent = s.remote;
      remoteBadge.title = `Агент работает на узле «${s.remote}»`;
    }

    const badge = document.createElement('span');
    badge.className = 'badge';
    // модель ещё не названа: имя агента, а рядом с пиллом — многоточие, чтобы не дублировать
    badge.textContent = modelLabel(s.agent, s.model) || (agentBadge ? '…' : AGENTS.title(s.agent).toLowerCase());

    const host = hostLabel(s);
    let hostBadge = null;
    if (host) {
      hostBadge = document.createElement('span');
      hostBadge.className = 'badge host';
      hostBadge.textContent = host;
    }

    // имя чата, данное человеком: оно и есть заголовок этой строки
    let chatName = null;
    if (s.name) {
      chatName = document.createElement('span');
      chatName.className = 'badge chatname';
      chatName.textContent = s.name;
      chatName.title = `Своё имя чата${s.autoTitle ? ` (авто: ${s.autoTitle})` : ''} — клик, чтобы изменить`;
      chatName.addEventListener('click', (e) => { e.stopPropagation(); startRename(s); });
    }

    const summary = document.createElement('span');
    summary.className = 'summary';
    // контекст по убыванию точности: текущая задача → саммари последних задач → промпт → ai-title
    const live = s.detail || STATUS_LABEL[s.status] || '';
    // имя уже стоит чипом — вторым эхом в этой же строке оно не нужно
    const ctx = s.task
      ? `${s.taskProgress ? s.taskProgress + ' · ' : ''}${s.task}`
      : (s.summary || s.lastPrompt || (s.name ? '' : s.title) || '');
    // Состояние — первым: многоточие режет конец, и раньше отваливалось ровно
    // то, ради чего в список и смотрят. У «готово» и «лимита» контекст есть
    // почти всегда, поэтому слова состояния не показывались никогда.
    summary.textContent = ctx && ctx !== live ? `${live} — ${ctx}` : (live || ctx);
    // обрезанное саммари негде дочитать: в title строки ни task, ни summary нет
    summary.title = summary.textContent;

    const time = document.createElement('span');
    time.className = 'time';
    time.textContent = startLabel(s.createdAt);
    time.title = startTitle(s.createdAt);

    row.append(dot, name);
    if (chatName) row.appendChild(chatName);
    if (branch) row.appendChild(branch);
    if (agentBadge) row.appendChild(agentBadge);
    if (remoteBadge) row.appendChild(remoteBadge);
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
    row.addEventListener('click', () => { sel = i; openSession(s); });
    listEl.appendChild(row);
  });
}

/* ---------- чат сессии ---------- */

/* --- мини-маркдаун для реплик ассистента: абзацы, списки, код. Без innerHTML.
 * Сам рендер живёт в markdown.js: та же лента есть у вкладки «Джарвис» и у окна
 * из трея, а renderer.js там не загружен. Здесь — только местное имя. --- */

function renderMarkdown(root, text) { JarvisMarkdown.renderChat(root, text); }


/* --- лента чата: подряд идущие тулзы группируются в чипы, повторы ×N --- */

let toolsGroup = null; // текущая группа чипов (обнуляется текстовой репликой)

/* --- сводки ходов: лента группируется в .turn-блоки по юзер-репликам --- */
let curTurn = null; // { key, wrap, raw } — текущий ход агента
const turnFacts = new Map(); // key → {files, commands} из chat_open.spans
let chatLlmOk = false; // есть ли служебный LLM (кнопка «Сводка»)
const turnTarget = () => (curTurn ? curTurn.raw : chatlogEl);

function startTurn(key) {
  toolsGroup = null; // чипы прошлого хода не продолжаем в новом
  const wrap = document.createElement('div');
  wrap.className = 'turn';
  wrap.dataset.key = key;
  const raw = document.createElement('div');
  raw.className = 'turnraw';
  wrap.appendChild(raw);
  chatlogEl.appendChild(wrap);
  curTurn = { key, wrap, raw };
}

/* тумблер «Сводка/Лента» — запоминается локально, по умолчанию сводка.
 * Объявлен здесь (до openChat): const-стрелки не хойстятся. */
const sumToggleEl = document.getElementById('sumToggle');
const summaryModeOn = () => localStorage.getItem('chatSummary') !== '0';
function renderSumToggle() {
  const on = summaryModeOn();
  sumToggleEl.classList.toggle('on', on);
  sumToggleEl.textContent = on ? 'Сводка' : 'Лента';
  chatlogEl.classList.toggle('sum', on);
}
sumToggleEl.addEventListener('click', () => {
  localStorage.setItem('chatSummary', summaryModeOn() ? '0' : '1');
  renderSumToggle();
});
renderSumToggle();

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
    const disclosure = document.createElement('details');
    disclosure.className = 'tool-disclosure';
    const summary = document.createElement('summary');
    summary.textContent = 'Действия агента';
    disclosure.appendChild(summary);
    toolsGroup = document.createElement('div');
    toolsGroup.className = 'msg tools';
    disclosure.appendChild(toolsGroup);
    turnTarget().appendChild(disclosure);
  }
  const summary = toolsGroup.parentElement.querySelector('summary');
  const count = Number(toolsGroup.dataset.count || 0) + 1;
  toolsGroup.dataset.count = String(count);
  summary.textContent = `Действия агента · ${count}`;
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
  if (arg && ['read','write','edit','multiedit','notebookedit'].includes(tool.toLowerCase())) {
    const sessionId=chatSessionId, path=window.JarvisArtifacts?.filePath(arg.replace(/:\d+(?::\d+)?$/, ''));
    if(path){chip.tabIndex=0;chip.setAttribute('role','button');chip.setAttribute('aria-label','Открыть '+path);
      const preview=()=>window.JarvisArtifacts.open({sessionId,path,source:state.find(s=>s.id===sessionId)?.remote||'Этот компьютер'});
      chip.addEventListener('click',preview);chip.addEventListener('keydown',event=>{if(event.key==='Enter'||event.key===' '){event.preventDefault();preview();}});
    }
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
  // путь к картинке, вставленной из Jarvis (только наш temp-каталог jarvis-paste —
  // произвольные пути, набранные юзером руками, из текста не выдёргиваем)
  text = text.replace(PASTED_IMG_RE, (m, pre, p) => { chips.push({ type: 'image', label: p.split('/').pop(), path: p }); return pre; });
  // @file: только если выглядит как путь (есть точка или слэш) — не трогаем @everyone и т.п.
  text = text.replace(/(^|\s)@([\w./\-]*[./][\w./\-]*)/g, (m, pre, p) => { chips.push({ type: 'file', label: `@${p}`, path: p }); return pre; });
  text = text.replace(/Вложения \(пути к файлам на машине агента\):\n((?:"[^\n]+"(?:\n|$))+)/g, (_, paths) => {
    for (const line of paths.trim().split('\n')) { try { const path = JSON.parse(line); chips.push({ type: 'file', label: path.split('/').pop(), path }); } catch {} }
    return '';
  });
  return { chips, text: text.replace(/[ \t]{2,}/g, ' ').trim() };
}

// inline-SVG картинки, собранные через DOM (без innerHTML — политика файла).
// Живут в markdown.js — оттуда же их берёт Insight в реплике; здесь короткое
// местное имя, потому что иконок в файле два десятка.
function svgEl(tag, attrs) { return JarvisMarkdown.svgEl(tag, attrs); }
function makeImageIcon() {
  const svg = svgEl('svg', { width: '11', height: '11', viewBox: '0 0 12 12', fill: 'none' });
  svg.appendChild(svgEl('rect', { x: '1', y: '1.5', width: '10', height: '9', rx: '1.5', stroke: 'currentColor', 'stroke-width': '1.2' }));
  svg.appendChild(svgEl('circle', { cx: '4', cy: '4.6', r: '1', fill: 'currentColor' }));
  svg.appendChild(svgEl('path', { d: 'M1.5 9 L4.5 6.4 L7 8.6 L8.6 7.2 L10.5 9', stroke: 'currentColor', 'stroke-width': '1.2', 'stroke-linejoin': 'round' }));
  return svg;
}

function userBubble(rawText, sessionId = chatSessionId, files = []) {
  const { chips, text } = extractAttachments(rawText);
  const bubble = document.createElement('div');
  bubble.className = 'bubble';
  if (chips.length) {
    const wrap = document.createElement('div');
    wrap.className = 'attachs';
    for (const c of chips) {
      const chip = document.createElement(c.path ? 'button' : 'div');
      chip.className = 'attach';
      if (c.path) { chip.type = 'button'; chip.addEventListener('click', () => window.JarvisArtifacts.open({ sessionId, path: c.path, source: state.find(s => s.id === sessionId)?.remote || 'Этот компьютер' })); }
      if (c.type === 'image') chip.appendChild(makeImageIcon());
      const lbl = document.createElement('span');
      lbl.textContent = c.label;
      chip.appendChild(lbl);
      wrap.appendChild(chip);
    }
    bubble.appendChild(wrap);
  }
  window.JarvisArtifacts?.chips(bubble, files.length ? files : window.JarvisArtifacts.references(rawText).filter(ref => !chips.some(chip => chip.path === ref.path)), sessionId, state.find(s => s.id === sessionId)?.remote || 'Этот компьютер');
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
  const agent = state.find(s => s.id === chatSessionId)?.agent || 'claude';
  who.textContent = agent === 'claude' ? 'Claude' : agent === 'codex' ? 'Codex' : agent;
  head.appendChild(who);
  const tm = fmtClock(it.ts);
  if (tm) {
    const t = document.createElement('span');
    t.className = 'mtime';
    t.textContent = tm;
    head.appendChild(t);
  }
  const copy = document.createElement('button');
  copy.className = 'msg-copy'; copy.type = 'button'; copy.textContent = 'Копировать';
  copy.setAttribute('aria-label', 'Копировать ответ целиком');
  copy.addEventListener('click', async () => {
    try {
      const result = await window.jarvis.copyText(String(it.text || ''));
      if (result?.ok === false) throw new Error(result.error);
      showToast('Ответ скопирован');
    } catch (error) { showToast(error.message || String(error)); }
  });
  head.appendChild(copy);
  msg.appendChild(head);
  const bubble = document.createElement('div');
  bubble.className = 'bubble';
  renderMarkdown(bubble, it.text);
  window.JarvisArtifacts?.chips(bubble, window.JarvisArtifacts.references(it.text), chatSessionId, curSession()?.remote || 'Этот компьютер');
  for (const pre of bubble.querySelectorAll('pre')) {
    const code = pre.querySelector('code');
    const text = code?.textContent ?? pre.textContent;
    const button = document.createElement('button');
    button.className = 'code-copy'; button.type = 'button'; button.textContent = 'Копировать код';
    button.addEventListener('click', async () => {
      try {
        const result = await window.jarvis.copyText(text);
        if (result?.ok === false) throw new Error(result.error);
        showToast('Код скопирован');
      } catch (error) { showToast(error.message || String(error)); }
    });
    pre.prepend(button);
  }
  msg.appendChild(bubble);
  return msg;
}

/* Карточка сводки хода. card=null → детерминированная (факты + сжатый ответ). */
function buildCard(key, card) {
  const facts = turnFacts.get(key) || { files: [], commands: [] };
  const box = document.createElement('div');
  box.className = 'turnsum';

  // kind (created|edited) — детерминированный признак из фактов; у LLM-карточек
  // из старого кэша поля может не быть — фоллбэк на факты хода по пути
  const kindOf = (f) => f.kind || (facts.files.find((x) => x.path === f.path) || {}).kind || '';
  let files = card ? card.files : facts.files.map((f) => ({ path: f.path, note: '', kind: f.kind }));
  // doc-файлы (docs/** или *.md) — первыми: главный артефакт хода (§3.3)
  files = [...files.filter((f) => JarvisMarkdown.isDocPath(f.path)), ...files.filter((f) => !JarvisMarkdown.isDocPath(f.path))];
  const sumText = card ? card.summary : detSummary(key);
  if (sumText) {
    const s = document.createElement('div');
    renderMarkdown(s, sumText);
    box.appendChild(s);
  }
  if (files.length) {
    const fl = document.createElement('div');
    fl.className = 'tsum-files';
    for (const f of files) {
      const isDoc = JarvisMarkdown.isDocPath(f.path);
      const chip = document.createElement('span');
      chip.className = 'fchip';
      chip.title = `${f.path} — клик: посмотреть, ${window.jarvisKeys.ALT}+клик: показать в ${window.jarvisKeys.NOUNS.fileManager}`;
      const p = document.createElement('span');
      p.textContent = (isDoc ? '📄 ' : '') + f.path.split('/').pop();
      chip.appendChild(p);
      if (isDoc && kindOf(f)) { // бейдж «создан/изменён» — только на доках
        const b = document.createElement('span');
        b.className = 'fbadge';
        b.textContent = kindOf(f) === 'created' ? 'создан' : 'изменён';
        chip.appendChild(b);
      }
      if (f.note) {
        const n = document.createElement('span');
        n.className = 'fnote';
        n.textContent = '· ' + f.note;
        chip.appendChild(n);
      }
      // клик — вьюер в панели; Alt/⌥ — файловый менеджер (редактор — из вьюера)
      chip.addEventListener('click', async (ev) => {
        if (!ev.altKey) { openDocViewer(f.path, kindOf(f)); return; }
        const res = await window.jarvis.openFile(chatSessionId, f.path, true);
        if (res && res.error) showToast(res.error);
      });
      fl.appendChild(chip);
    }
    box.appendChild(fl);
  }
  if (card && card.docs_digest) {
    const det = document.createElement('details');
    const sm = document.createElement('summary');
    sm.textContent = 'Дока';
    det.appendChild(sm);
    const body = document.createElement('div');
    renderMarkdown(body, card.docs_digest);
    det.appendChild(body);
    box.appendChild(det);
  }
  const cmds = card ? card.commands : facts.commands.slice(0, 3).join(' · ');
  if (cmds) {
    const c = document.createElement('div');
    c.className = 'tsum-cmds';
    c.textContent = cmds;
    box.appendChild(c);
  }

  const foot = document.createElement('div');
  foot.className = 'tsum-foot';
  // ход трогал док → CTA «Открыть документ» (первый док) — сценарий
  // «отревьюить, что агент написал» (§3.3)
  const firstDoc = files.find((f) => JarvisMarkdown.isDocPath(f.path));
  if (firstDoc) {
    const docBtn = document.createElement('button');
    docBtn.className = 'tsum-btn';
    docBtn.textContent = 'Открыть документ';
    docBtn.addEventListener('click', () => openDocViewer(firstDoc.path, kindOf(firstDoc)));
    foot.appendChild(docBtn);
  }
  const exp = document.createElement('button');
  exp.className = 'tsum-btn';
  const wrapOf = () => box.closest('.turn');
  const relabel = () => { exp.textContent = wrapOf()?.classList.contains('expanded') ? 'свернуть' : 'развернуть'; };
  exp.addEventListener('click', () => { wrapOf()?.classList.toggle('expanded'); relabel(); });
  foot.appendChild(exp);
  if (!card && chatLlmOk) {
    const gen = document.createElement('button');
    gen.className = 'tsum-btn';
    gen.textContent = 'Сводка';
    gen.addEventListener('click', () => {
      gen.textContent = 'готовлю…';
      gen.disabled = true;
      window.jarvis.summarizeTurn(chatSessionId, key);
      // сбой/таймаут LLM события не даёт — возвращаем кнопку через 100с
      // (2 попытки × 45с + запас); гейт по isConnected: если карточка успела
      // прийти, applyCard заменил .turnsum целиком и кнопка вне DOM — не оживёт
      setTimeout(() => {
        if (gen.isConnected) { gen.textContent = 'Сводка'; gen.disabled = false; }
      }, 100000);
    });
    foot.appendChild(gen);
  }
  box.appendChild(foot);
  queueMicrotask(relabel);
  return box;
}

// детерминированное саммари: сжатый хвост последней реплики агента в ходе
function detSummary(key) {
  const wrap = chatlogEl.querySelector(`.turn[data-key="${CSS.escape(key)}"]`);
  const bubbles = wrap ? wrap.querySelectorAll('.msg.assistant .bubble') : [];
  const last = bubbles.length ? bubbles[bubbles.length - 1].textContent.trim() : '';
  return last.length > 220 ? last.slice(0, 220) + '…' : last;
}

/* Вставить/заменить карточку хода; card=null — детерминированная. */
function applyCard(key, card) {
  const wrap = chatlogEl.querySelector(`.turn[data-key="${CSS.escape(key)}"]`);
  if (!wrap) return;
  // Класс done схлопывает всё сырьё хода, а событие приходит асинхронно — ровно
  // когда человек дочитывает. WebKit не якорит скролл, scrollTop клампится, и
  // читателя швыряет вниз. Держим место так же, как appendChatItems: у нижнего
  // края идём за лентой, выше — остаёмся на прочитанном.
  const nearBottom =
    chatlogEl.scrollHeight - chatlogEl.scrollTop - chatlogEl.clientHeight < 60;
  const keepTop = chatlogEl.scrollTop;
  const keepHeight = chatlogEl.scrollHeight;
  // ход целиком выше окна — на столько же надо подтянуть скролл вверх
  const above = wrap.offsetTop < keepTop;
  wrap.querySelector('.turnsum')?.remove();
  wrap.insertBefore(buildCard(key, card), wrap.firstChild);
  wrap.classList.add('done');
  if (nearBottom) chatlogEl.scrollTop = chatlogEl.scrollHeight;
  else chatlogEl.scrollTop = Math.max(0, keepTop - (above ? keepHeight - chatlogEl.scrollHeight : 0));
}

// оптимистично показанные ответы юзера, ждут «эха» из транскрипта (для дедупа)
const pendingRepliesBySession = new Map();
const observedUserItems = new Map();
const trackedLaunchMessages = new Set();
const userItemKey = it => JSON.stringify([it.ts, it.text.trim()]);
function pendingFor(sessionId) {
  if (!pendingRepliesBySession.has(sessionId)) pendingRepliesBySession.set(sessionId, []);
  return pendingRepliesBySession.get(sessionId);
}
function removePending(pending) {
  pending.el.remove();
  const list = pendingFor(pending.sessionId), index = list.indexOf(pending);
  if (index >= 0) list.splice(index, 1);
}
function restorePendingReplies() {
  const list = pendingFor(chatSessionId);
  if (list.length) chatlogEl.querySelector('.chatempty')?.remove();
  for (const pending of list) chatlogEl.append(pending.el);
}

// сразу показать отправленную реплику в ленте — иначе при занятой сессии она
// уходит в очередь Claude и в чате до обработки не видна («ничего не происходит»)
function appendPendingReply(text, queued, sessionId = chatSessionId, matchText = text, files = []) {
  if (view === 'chat' && sessionId === chatSessionId) {
    chatlogEl.querySelector('.chatempty')?.remove();
    toolsGroup = null;
  }
  const msg = document.createElement('div');
  msg.className = 'msg user pending';
  msg.appendChild(userBubble(text, sessionId, files));
  const st = document.createElement('div');
  st.className = 'msg-status';
  st.textContent = queued ? 'в очереди — доставлю, как освободится' : 'отправлено';
  msg.appendChild(st);
  if (view === 'chat' && sessionId === chatSessionId) {
    chatlogEl.appendChild(msg);
    chatlogEl.scrollTop = chatlogEl.scrollHeight;
  }
  const pending = { sessionId, text: matchText.trim(), el: msg, seen: new Set(observedUserItems.get(sessionId) || []) };
  pendingFor(sessionId).push(pending);
  return pending;
}


// Keep the first task visible while the remote transcript catches up. Repeated
// launch events update delivery status, even if the user has opened another chat.
function trackLaunchMessage(sessionId, message) {
  let pending = pendingFor(sessionId).find(p => p.launchKey === message.key);
  if (!trackedLaunchMessages.has(message.key) && message.displayText != null) {
    trackedLaunchMessages.add(message.key);
    pending = appendPendingReply(message.displayText, false, sessionId, message.text, message.files);
    pending.launchKey = message.key;
  }
  if (!pending) return; // The transcript has already acknowledged this task.
  window.JarvisAsyncState.status(pending.el.querySelector('.msg-status'), message.status, message.kind);
}

/* Сколько верхних блоков держим в ленте чата.
 *
 * Лента росла всю сессию: у агента, работающего часами, в ней накапливались
 * десятки тысяч узлов, и каждое следующее добавление пересчитывало вёрстку по
 * всей этой куче. Отсюда и «чат подтормаживает, а потом встаёт»: чем дольше
 * смотришь, тем медленнее. Хвоста хватает с запасом — выше него всё равно
 * листают до начала не глазами, а поиском. */
const CHATLOG_MAX_BLOCKS = 400;

function trimChatlog() {
  let extra = chatlogEl.childElementCount - CHATLOG_MAX_BLOCKS;
  while (extra-- > 0) {
    const first = chatlogEl.firstElementChild;
    // Текущий ход не трогаем ни при каких условиях: в него ещё пишет стрим, и
    // унесённый из документа узел проглотил бы все следующие реплики молча.
    if (!first || (curTurn && first === curTurn.wrap)) break;
    first.remove();
  }
  // Потолок верхних блоков в главном сценарии не действует вовсе: новый .turn
  // заводит только реплика ЧЕЛОВЕКА, а всё, что агент делает дальше сам, растёт
  // внутри ОДНОГО хода. «Дал задачу — агент работает сам» за полчаса — это
  // тысячи узлов в одном блоке, и каждый тик хвоста меряет вёрстку по ним всем.
  const raw = curTurn && curTurn.raw;
  if (!raw) return;
  let inner = raw.childElementCount - CHATLOG_MAX_BLOCKS;
  while (inner-- > 0) {
    const head = raw.firstElementChild;
    if (!head) break;
    head.remove();
  }
}

function appendChatItems(items) {
  const nearBottom =
    chatlogEl.scrollHeight - chatlogEl.scrollTop - chatlogEl.clientHeight < 60;
  chatlogEl.querySelector('.chatempty')?.remove();
  for (const it of items) {
    if (it.kind === 'tool' || it.kind === 'progress') {
      addToolChip(it.kind === 'progress' ? 'Ход работы · ' + it.text : it.text);
      continue;
    }
    // Ignore future runtime events instead of interpreting them as assistant text.
    if ((it.kind && it.kind !== 'text') || !['user', 'assistant'].includes(it.role)) continue;
    toolsGroup = null;
    if (it.role === 'user') {
      // реальная реплика из транскрипта пришла — снимаем оптимистичный дубль
      const key = userItemKey(it);
      const pending = pendingFor(chatSessionId).find(p => p.text === it.text.trim() && !p.seen.has(key));
      if (pending) removePending(pending);
      if (!observedUserItems.has(chatSessionId)) observedUserItems.set(chatSessionId, new Set());
      observedUserItems.get(chatSessionId).add(key);
      const msg = document.createElement('div');
      msg.className = 'msg user';
      msg.appendChild(userBubble(it.text));
      chatlogEl.appendChild(msg);
      startTurn(String(it.ts)); // ответ агента на эту реплику — новый ход
    } else {
      turnTarget().appendChild(assistantMsg(it));
    }
  }
  if (items.length) trimChatlog();
  if (items.length && nearBottom) chatlogEl.scrollTop = chatlogEl.scrollHeight;
}

function updateChatChannelMark() {
  const s = state.find((x) => x.id === chatSessionId);
  // модель — бейдж рядом с именем проекта (как в строке списка)
  const model = s && (modelLabel(s.agent, s.model) || s.agent);
  chatModelEl.textContent = model || '';
  chatModelEl.hidden = !model;
  // рука связки: ветка в шапке + пометка конфликта. Человек, открывший чат из
  // пульта, должен видеть, ГДЕ он, — обычный чат и рука выглядят одинаково.
  const chatBundleEl = document.getElementById('chatBundle');
  if (chatBundleEl) {
    const hand = window.bundleHandOf ? window.bundleHandOf(chatSessionId) : null;
    chatBundleEl.textContent = hand
      ? `связка · ${hand.branch}${hand.state === 'conflict' ? ' · конфликт' : ''}`
      : '';
    chatBundleEl.hidden = !hand;
    chatBundleEl.title = hand ? `worktree ${hand.worktree}` : '';
  }
  // сессия с удалённого узла — имя узла в шапке, чтобы не спутать с локальной:
  // ответы и пульт уходят туда по SSH, а не в терминал на этой машине
  if (chatRemoteEl) {
    const rem = s && s.remote ? String(s.remote) : '';
    chatRemoteEl.textContent = rem;
    chatRemoteEl.hidden = !rem;
    chatRemoteEl.title = rem ? `Агент работает на узле «${rem}» — ответы уходят туда по SSH` : '';
  }
  // «Изменения» — только когда есть где их считать: без рабочего каталога
  // git спрашивать не о чем, и кнопка вела бы в тупик
  if (changesBtn) {
    changesBtn.hidden = !s || !s.cwd;
    if (changesBtn.hidden && chgOpen) closeChanges();
  }
  if (searchBtn) {
    searchBtn.hidden = !s || !s.cwd;
    if (searchBtn.hidden && srchOpen) closeSearch();
  }
  // Превью — только у местной задачи: адрес узла на этом компьютере не
  // откроется, а обещать превью и не показать его хуже, чем не обещать.
  if (previewBtn) previewBtn.hidden = !s || !s.cwd || !!s.remote;
  // tmux-сессии — без пометки; вне tmux помечаем
  chatChannelEl.hidden = !s || !!s.tmuxPane || s.controlMode === 'external';
  // статус-точка справа — форма и краска по состоянию, движение если ждёт
  chatDotEl.className = `chatdot ${s ? s.status : ''}${failedSession(s) ? ' failed' : ''}`;
  chatDotEl.title = statusWord(s);
  // правый край: расход сессии (или ветка, пока usage грузится) — тихо, моно
  const subEl = document.getElementById('chatSub');
  subEl.textContent = s && s.branch ? `⎇ ${s.branch}` : '';
  if (s && !window.jarvisSessionWorkspace) {
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

// Пары [id, подпись] для `/model <id>`: чужие модели в сессии предлагать нельзя (#10).
function modelsFor(agent) {
  return AGENTS.models(agent).map((m) => [m.id, m.name]);
}

// Короткое имя из каталога: id вроде «kimi-code/k3-256k» в узкую строку не влезает.
function modelLabel(agent, model) {
  if (!model) return '';
  const hit = AGENTS.models(agent).find((m) => m.id === model);
  return hit ? hit.name : model;
}

function visibleModel(value) {
  if (window.JarvisSessionState?.modelName) return window.JarvisSessionState.modelName(value);
  // Legacy/standalone surfaces may not load the workspace module.
  if (typeof value !== 'string' || value.length > 256 || /[\x00-\x1f\x7f<>]/.test(value)) return '';
  return /^synthetic$/i.test(value.trim()) ? '' : value.trim();
}

// базовые уровни приезжают из `claude --help` через демона (не отстаём от CLI);
// ultracode в help не публикуется — это знание с живого слайдера Fable/Opus
let effortBase = ['low', 'medium', 'high', 'xhigh', 'max'];
window.jarvis.getMeta().then((m) => {
  if (!m) return;
  if (Array.isArray(m.effortLevels) && m.effortLevels.length) effortBase = m.effortLevels;
  AGENTS.setAll(m.agents);
}).catch(() => {});

const EFFORT_SHORT = { medium: 'med' };

// Уровни у каждого агента свои; общий список из app_meta — запасной.
function effortsFor(agent, model) {
  const m = (model || '').toLowerCase();
  const own = AGENTS.efforts(agent);
  const list = [['auto', 'auto'], ...(own.length ? own : effortBase).map((l) => [l, EFFORT_SHORT[l] || l])];
  if (m === 'fable' || m === 'opus') list.push(['ultracode', 'ultracode']);
  return list;
}

// вне tmux ответ недоступен — гасим поле и показываем, что запустить
function gateReply(s) {
  const isTmux = !!(s && s.tmuxPane);
  const external = s?.controlMode === 'external';
  tmuxHintEl.hidden = !s || isTmux || external;
  replyEl.disabled = !isTmux || external || sendingSessions.has(s?.id) || pendingChat?.sessionId === s?.id;
  replyEl.placeholder = external ? 'Только просмотр' : isTmux ? 'Ответить агенту…  ( / — команды )' : 'Ответ недоступен';
  if (!s || isTmux || external) return;
  tmuxHintEl.textContent = '';
  const details = document.createElement('details');
  const summary = document.createElement('summary'); summary.textContent = 'Подключить ответы из Jarvis'; details.append(summary);
  const hint = document.createElement('div'); details.append(hint); tmuxHintEl.append(details);
  const where = s.remote ? `Сессия на узле «${s.remote}» не в tmux — управлять из Jarvis нельзя. Запусти ТАМ: `
    : 'Сессия не в tmux — управлять из Jarvis нельзя. Запусти в терминале: ';
  hint.appendChild(document.createTextNode(where));
  const code = document.createElement('code');
  code.className = 'tmuxcmd';
  // id, под которым сессию знает САМ агент: у сессии с узла ключ реестра
  // выглядит как «<узел>:<id>», и `--resume` с ним не найдёт ничего
  const sid = s.providerSessionId || (s.remote && s.id.startsWith(s.remote + ':') ? s.id.slice(s.remote.length + 1) : s.id);
  // команда возобновления зависит от агента: codex resume <id> vs claude --resume <id>.
  // Раньше было захардкожено «claude --resume» — для codex-сессий это вело не туда.
  const resumeCmd = resumeBase(s.agent, sid);
  code.textContent = resumeCmd;
  code.title = 'Скопировать';
  code.addEventListener('click', () => {
    navigator.clipboard?.writeText(resumeCmd);
    showToast('Скопировано');
  });
  hint.appendChild(code);
  hint.appendChild(document.createTextNode(s.remote
    ? ' — под tmux -L jarvis, иначе Jarvis до неё не дотянется.'
    : ' — shim подхватит её в tmux.'));
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
    // не «ты нужен»: в списке эта же сессия в тот же момент самая тусклая, и
    // акцентная полоса в чате читалась ровно наоборот
    chatStatusEl.className = 'chatstatus limit';
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

/* ---------- вопросы агента: черновик привязан к requestId + revision ---------- */
const qviewEl = document.getElementById('qview');
const qOptsEl = document.getElementById('qOpts');
const qHeaderEl = document.getElementById('qHeader');
const qTitleEl = document.getElementById('qTitle');
const qFootEl = document.getElementById('qFoot');
const qCustomRowEl = document.getElementById('qCustomRow');
const qCustomEl = document.getElementById('qCustom');
let qSessionId = null, qData = null, qRequest = null, qDraft = null;
let qSel = 0, qChosen = new Set(), qItems = [], qIdx = 0, qAnswers = [], qTexts = [];
let qPending = false, qReview = false, qMessage = '', qUnknown = false;
let activeQOpts = qOptsEl, activeQCustom = null;
const qDrafts = JarvisQuestionAnswer.createDraftStore();
function keycap(text) { const k = document.createElement('span'); k.className = 'keycap'; k.textContent = text; return k; }
function saveQDraft() {
  if (!qDraft) return;
  qDraft.answers = qAnswers; qDraft.texts = qTexts; qDraft.index = qIdx;
  qDraft.selections[qIdx] = qSel; qDraft.review = qReview;
  qDraft.message = qMessage; qDraft.unknown = qUnknown;
}
function loadQ() {
  qData = qItems[qIdx] || null;
  qSel = qDraft?.selections[qIdx] ?? 0;
  qChosen = new Set(qAnswers[qIdx] || []);
}
function beginQ(s) {
  if (!s?.question?.questions?.length) return false;
  saveQDraft();
  qSessionId = s.id; qRequest = s.question; qItems = qRequest.questions;
  qDraft = qDrafts.get(s.id, qRequest); qIdx = Math.min(qDraft.index, qItems.length - 1);
  qAnswers = qDraft.answers; qTexts = qDraft.texts; qReview = qDraft.review;
  qMessage = qDraft.message; qUnknown = qDraft.unknown; qPending = !!qDraft.pending; loadQ();
  return true;
}
function openQuestion(s) {
  if (!beginQ(s)) return;
  setView('question'); render(); renderQuestion();
  qOptsEl.focus();
}
function questionWritable() { return !['external', 'codex-rpc'].includes(qRequest?.transport); }
function paintQOptions() {
  for (const [i, btn] of [...activeQOpts.querySelectorAll('.qopt')].entries()) {
    const custom = qData?.customMode !== 'notes' && JarvisQuestionAnswer.normalizeText(qTexts[qIdx]);
    const chosen = qData?.multiSelect ? qChosen.has(i + 1) : !custom && i === qSel;
    btn.classList.toggle('sel', i === qSel); btn.classList.toggle('chosen', chosen);
    btn.setAttribute('aria-checked', String(chosen));
    btn.tabIndex = i === qSel ? 0 : -1;
  }
  activeQOpts.querySelectorAll('.qopt')[qSel]?.scrollIntoView({ block: 'nearest' });
}
function questionError(text) { qMessage = text; saveQDraft(); paintQStatus(); }
function paintQStatus() {
  const foot = varOpen ? qpFootEl : qFootEl;
  const status = foot.querySelector('.q-status');
  if (status) status.textContent = qPending ? 'Отправляю · жду подтверждения агента…' : qMessage;
  foot.querySelectorAll('button[data-q-submit]').forEach(b => { b.disabled = qPending || qUnknown || !questionWritable(); });
  activeQOpts.querySelectorAll('button').forEach(b => { b.disabled = qPending || qUnknown; });
  if (activeQCustom) activeQCustom.disabled = qPending || qUnknown;
}
function renderQOpts(optsEl, footEl) {
  optsEl.replaceChildren(); optsEl.tabIndex = 0;
  optsEl.setAttribute('role', qReview ? 'group' : qData.multiSelect ? 'group' : 'radiogroup');
  optsEl.setAttribute('aria-label', qReview ? 'Проверка ответов' : qData.question);
  if (qReview) {
    qItems.forEach((item, index) => {
      const row = document.createElement('div'); row.className = 'q-review-row';
      const title = document.createElement('strong'); title.textContent = item.header || item.question;
      const answer = document.createElement('div'); answer.className = 'q-review-answer';
      const labels = (qAnswers[index] || []).map(n => item.options[n - 1]?.label).filter(Boolean);
      if (qTexts[index]?.trim()) labels.push(item.isSecret ? '••••••••' : qTexts[index].trim());
      answer.textContent = labels.join('\n');
      const edit = document.createElement('button'); edit.className = 'q-edit'; edit.textContent = 'Изменить'; edit.disabled = qPending;
      edit.addEventListener('click', () => { qReview = false; qIdx = index; loadQ(); saveQDraft(); redrawQ(); });
      row.append(title, answer, edit); optsEl.append(row);
    });
  } else {
    qData.options.forEach((o, i) => {
      const btn = document.createElement('button'); btn.type = 'button'; btn.className = 'qopt';
      btn.setAttribute('role', qData.multiSelect ? 'checkbox' : 'radio');
      const num = document.createElement('span'); num.className = 'qnum'; num.textContent = String(i + 1);
      const body = document.createElement('span'); body.className = 'qbody';
      const label = document.createElement('span'); label.className = 'qlabel'; label.textContent = o.label; body.append(label);
      if (o.description) { const desc = document.createElement('span'); desc.className = 'qdesc'; desc.textContent = o.description; body.append(desc); }
      btn.append(num, body);
      if (qData.multiSelect) { const ck = document.createElement('span'); ck.className = 'qcheck'; ck.textContent = '✓'; btn.append(ck); }
      btn.addEventListener('focus', () => { qSel = i; paintQOptions(); });
      btn.addEventListener('click', () => { if (qPending || qUnknown) return; qSel = i; activateQ(); });
      optsEl.append(btn);
    });
    paintQOptions();
  }
  footEl.replaceChildren();
  const status = document.createElement('div'); status.className = 'q-status'; status.setAttribute('role', 'status'); status.setAttribute('aria-live', 'polite');
  const controls = document.createElement('div'); controls.className = 'q-controls';
  if (qReview || qIdx > 0) {
    const back = document.createElement('button'); back.className = 'q-secondary'; back.textContent = 'Назад'; back.disabled = qPending;
    back.addEventListener('click', backQ); controls.append(back);
  }
  const hint = document.createElement('span'); hint.className = 'q-key-hint';
  hint.textContent = qReview ? '⌘/Ctrl + Enter — отправить' : '↑↓ — вариант · пробел — выбрать · Esc — назад';
  controls.append(hint);
  const send = document.createElement('button'); send.className = 'q-primary'; send.dataset.qSubmit = 'true';
  send.textContent = qReview ? 'Отправить ответы' : qIdx + 1 < qItems.length ? 'Далее' : 'Проверить ответы';
  send.addEventListener('click', submitQ); controls.append(send);
  footEl.append(status, controls);
  if (!questionWritable()) {
    qMessage = 'Вопрос открыт в Codex. Ответь в приложении агента; твой черновик остаётся здесь.';
    const open = document.createElement('button'); open.className = 'q-secondary'; open.textContent = 'Открыть в Codex';
    open.addEventListener('click', () => focusTerminal(qSessionId, state.find(s => s.id === qSessionId)?.project)); controls.append(open);
  }
}
function renderQCustom(rowEl, inputEl) {
  const s = state.find(x => x.id === qSessionId);
  rowEl.hidden = qReview || !JarvisQuestionAnswer.customAllowed(s?.agent, qData, qRequest);
  if (inputEl.value !== (qTexts[qIdx] || '')) inputEl.value = qTexts[qIdx] || '';
  inputEl.placeholder = qData?.customMode === 'notes' ? 'Пояснение к выбранному варианту…' : 'Свой ответ…';
  inputEl.setAttribute('aria-label', inputEl.placeholder);
  inputEl.classList.toggle('q-secret', !!qData?.isSecret);
  inputEl.setAttribute('autocomplete', 'off');
  activeQCustom = rowEl.hidden ? null : inputEl;
  paintQStatus();
}
function renderQuestion() {
  if (!qData) return;
  qHeaderEl.textContent = qData.header || ''; qHeaderEl.hidden = !qData.header;
  qTitleEl.textContent = qReview ? 'Проверь ответы перед отправкой' : qData.question;
  const prog = document.getElementById('qProgress'); prog.textContent = qReview ? 'Проверка' : `${qIdx + 1} / ${qItems.length}`; prog.hidden = false;
  activeQOpts = qOptsEl; renderQOpts(qOptsEl, qFootEl); renderQCustom(qCustomRowEl, qCustomEl);
}
function redrawQ() { if (varOpen) renderVarPanel(curSession()); else renderQuestion(); }
function toggleQ(i) {
  if (qPending || qUnknown || qReview || !qData.options[i]) return;
  const n = i + 1; if (qChosen.has(n)) qChosen.delete(n); else qChosen.add(n);
  qAnswers[qIdx] = [...qChosen]; saveQDraft(); paintQOptions();
}
function activateQ() {
  if (!qData?.options[qSel]) return;
  if (qData.multiSelect) toggleQ(qSel);
  else {
    // Selecting a predefined answer intentionally switches away from Other.
    if (qData.customMode !== 'notes') { qTexts[qIdx] = ''; if (activeQCustom) activeQCustom.value = ''; }
    qAnswers[qIdx] = [qSel + 1]; saveQDraft(); paintQOptions();
  }
}
function commitCurrentQ() {
  const res = JarvisQuestionAnswer.commitRow({ multiSelect: qData.multiSelect, chosen: qChosen, sel: qSel,
    text: activeQCustom ? activeQCustom.value : qTexts[qIdx], customMode: qData.customMode, optionCount: qData.options.length });
  if (!res) { questionError('Выбери вариант или напиши ответ.'); return false; }
  qAnswers[qIdx] = res.row; qTexts[qIdx] = res.text || ''; qMessage = ''; saveQDraft(); return true;
}
function advanceQ() {
  if (qIdx + 1 < qItems.length) { qIdx++; loadQ(); } else qReview = true;
  saveQDraft(); redrawQ();
}
function backQ() {
  if (qPending) return;
  saveQDraft();
  if (qReview) qReview = false;
  else if (qIdx > 0) { qIdx--; loadQ(); }
  else { if (varOpen) closeVarPanel(); else goBack(); return; }
  saveQDraft(); redrawQ();
}
async function finalizeQ() {
  if (qPending || qUnknown || !qRequest || !questionWritable()) return;
  const sid = qSessionId, request = qRequest, draft = qDraft;
  const payload = JarvisQuestionAnswer.buildPayload(qAnswers, qTexts, request, draft.submissionId);
  qPending = true; draft.pending = true; qMessage = ''; paintQStatus();
  try {
    const res = await window.jarvis.answerQuestion(sid, payload);
    draft.pending = false;
    draft.unknown = res.delivery === 'unknown' || res.delivery === 'sending' || (res.ok && res.delivery !== 'confirmed');
    draft.message = res.error || (draft.unknown ? 'Агент ещё не подтвердил ответ. Проверь терминал.' : '');
    if (res.ok && res.delivery === 'confirmed') qDrafts.delete(sid, request);
    if (qRequest !== request || qSessionId !== sid) return;
    if (res.ok && res.delivery === 'confirmed') {
      qDrafts.delete(sid, request); qDraft = null;
      if (varOpen) closeVarPanel(); else { setView('list'); render(); }
    } else {
      qUnknown = res.delivery === 'unknown' || res.delivery === 'sending' || res.ok;
      questionError(res.error || 'Агент ещё не подтвердил ответ. Проверь терминал.');
    }
  } catch (error) {
    draft.unknown = true; draft.message = 'Связь прервалась. Ответ мог дойти; проверь терминал. Черновик сохранён.';
    if (qRequest === request) { qUnknown = true; questionError(draft.message); }
  } finally { draft.pending = false; if (qRequest === request) { qPending = false; saveQDraft(); paintQStatus(); } }
}
function submitQ() {
  if (qPending || qUnknown) return;
  if (qReview) finalizeQ(); else if (commitCurrentQ()) advanceQ();
}
document.getElementById('qBack').addEventListener('click', backQ);

async function openChat(sessionId, project, navigationOptions = {}) {
  if (view === 'chat') window.jarvisSessionWorkspace?.saveDraft(chatSessionId, replyEl.value, pendingImages);
  const fromRoute = routeSnapshot();
  const request = ++chatOpenSequence;
  pendingChat = { sessionId, items: [] };
  chatSessionId = sessionId;
  const openedSession = state.find(s => s.id === sessionId);
  chatTitleEl.textContent = window.JarvisSessionState?.titleOf(openedSession || { project }) || openedSession?.title || project || 'Чат';
  chatTitleEl.title = chatTitleEl.textContent;
  boardExpanded = 0;
  closeBoard(); // доска прошлого чата не должна оставаться открытой
  closeVarPanel(); // и слайд-овер вариантов прошлого чата
  closeDocViewer(); // и вьюер документов
  updateChatChannelMark();
  chatlogEl.textContent = '';
  toolsGroup = null;
  curTurn = null;
  turnFacts.clear();
  chatLlmOk = false;
  chatlogEl.classList.toggle('sum', summaryModeOn());
  restorePendingReplies(); // исходящие сообщения принадлежат сессии, а не открытому экрану
  const savedDraft = window.jarvisSessionWorkspace?.draft(sessionId);
  replyEl.value = savedDraft?.text || '';
  autoGrowReply();
  pendingImages = savedDraft?.images?.slice() || [];
  renderAttachments();
  hidePalette();
  loadCommands();
  setView('chat', { fromRoute, ...navigationOptions });
  if (navigationOptions.restore?.draft) { replyEl.value = navigationOptions.restore.draft; autoGrowReply(); }
  // В оконном режиме список стоит рядом: выделение должно переехать на
  // открытый чат сейчас, а не со следующим пушем состояния.
  render();
  replyEl.focus();
  const loading = window.JarvisAsyncState.skeleton('history', 'Загружаем историю…');
  chatlogEl.setAttribute('aria-busy', 'true'); chatlogEl.append(loading);
  const slow = setTimeout(() => { if (request === chatOpenSequence && pendingChat) loading.append(window.JarvisAsyncState.message({ title: 'История загружается дольше обычного', detail: 'Можно открыть другой чат — загрузка не блокирует навигацию.' })); }, 8000);
  let res;
  try {
    for (let attempt = 0; attempt < 30; attempt++) {
      if (request !== chatOpenSequence) { clearTimeout(slow); return; }
      res = await window.jarvis.openChat(sessionId);
      if (!res.retryable) break;
      loading.setAttribute('aria-label', res.error || 'Ждём историю новой сессии…');
      await new Promise(resolve => setTimeout(resolve, 1000));
    }
  } catch (error) { res = { ok: false, error: String(error) }; }
  clearTimeout(slow);
  if (request !== chatOpenSequence) return;
  chatlogEl.setAttribute('aria-busy', 'false');
  const earlyItems = pendingChat?.items || []; pendingChat = null;
  gateReply(curSession()); window.jarvisSessionWorkspace?.composerChanged();
  if (!res.ok) {
    loading.replaceWith(window.JarvisAsyncState.message({ title: 'Не удалось загрузить историю', detail: res.error || 'Проверь подключение и повтори попытку.', kind: 'error', action: 'Повторить', onAction: () => openChat(sessionId, project) }));
    return;
  }
  loading.remove(); chatLlmOk = !!res.llm;
  if (res.items.length) {
    // Мост отдаёт хвост переписки. Без пометки верхняя реплика выглядит началом
    // разговора, хотя до неё были часы. Признак честный: у хода, чью юзер-реплику
    // хвост отрезал, complete=false — значит выше него что-то было.
    if ((res.spans || []).some((sp) => sp.key !== 'pre' && !sp.complete)) {
      const cut = document.createElement('div');
      cut.className = 'chatcut';
      cut.textContent = 'Показан хвост переписки — начало осталось в терминале';
      chatlogEl.appendChild(cut);
    }
    appendChatItems(res.items);
    const sess = state.find((x) => x.id === chatSessionId);
    const lastCompleteKey = (res.spans || []).filter((s) => s.complete).map((s) => s.key).pop();
    for (const sp of res.spans || []) {
      if (sp.key === 'pre') continue; // частичный головной ход — только сырьё
      turnFacts.set(sp.key, { files: sp.files || [], commands: sp.commands || [] });
      if (!sp.complete) continue;
      const card = (res.cards || {})[sp.key] || null;
      // живой ход не схлопываем дет-карточкой: агент ещё пишет, стрим должен быть
      // виден; карточка придёт событием chat:summary на Stop. Кэшированная
      // LLM-карточка означает, что Stop по этому ходу уже был — её применяем.
      if (!card && sp.key === lastCompleteKey && sess && sess.status === 'working') continue;
      applyCard(sp.key, card);
    }
    chatlogEl.scrollTop = chatlogEl.scrollHeight;
  } else if (!pendingFor(sessionId).length && !earlyItems.length) {
    const waiting = curSession()?.status === 'working';
    const empty = window.JarvisAsyncState.message({ title: waiting ? 'Синхронизируем сообщения…' : 'В этом чате пока нет сообщений', detail: waiting ? 'Агент работает. История появится после синхронизации.' : curSession()?.controlMode === 'external' ? 'История появится после синхронизации с агентом.' : 'Напиши задачу или прикрепи файл, чтобы начать.', action: !waiting && curSession()?.tmuxPane ? 'Написать сообщение' : null, onAction: () => replyEl.focus() });
    empty.classList.add('chatempty');
    chatlogEl.appendChild(empty);
  }
  if (earlyItems.length) appendChatItems(earlyItems);
  restorePendingReplies();
}

window.jarvis.onChatAppend(({ sessionId, items }) => {
  if (pendingChat?.sessionId === sessionId) { pendingChat.items.push(...items); return; }
  if (view === 'chat' && sessionId === chatSessionId) appendChatItems(items);
});

window.jarvis.onChatSummary(({ sessionId, turnKey, card }) => {
  if (view === 'chat' && sessionId === chatSessionId) applyCard(turnKey, card);
});

document.getElementById('chatBack').addEventListener('click', () => { closeBoard(); goBack(); });

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
  tasksRingFg.setAttribute('stroke', b.stopped ? 'var(--warn)' : 'var(--accent)'); // мёртвая доска — янтарь
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
  if (window.jarvisShortcutRecording || !document.getElementById('commandDialog').hidden) return;
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
const qpCustomRowEl = document.getElementById('qpCustomRow');
const qpCustomEl = document.getElementById('qpCustom');
let varOpen = false;

const questionOf = (s) =>
  s && s.question && s.question.questions && s.question.questions.length ? s.question.questions[0] : null;

function renderVarBtn(s) {
  const q = questionOf(s);
  if (!q) { varBtn.hidden = true; if (varOpen) closeVarPanel(); return; }
  varBtn.hidden = false;
  const n = q.options.length;
  const count = s.question.questions.length;
  varBtnLabel.textContent = count > 1 ? `${count} ${plural(count, 'вопрос', 'вопроса', 'вопросов')}` : n ? `${n} ${plural(n, 'вариант', 'варианта', 'вариантов')}` : 'Написать ответ';
  varBtn.classList.toggle('open', varOpen);
  if (varOpen) renderVarPanel(s);
}

function renderVarPanel(s) {
  if (!qData || !qRequest) { closeVarPanel(); return; }
  const current = s?.question;
  if (current && JarvisQuestionAnswer.draftKey(s.id, current) !== JarvisQuestionAnswer.draftKey(qSessionId, qRequest)) {
    if (qPending) return;
    beginQ(s); qMessage = 'Агент задал новый вопрос. Предыдущий черновик сохранён.';
  }
  qpHeaderEl.textContent = qReview ? 'Проверка' : `${qIdx + 1} / ${qItems.length}`; qpHeaderEl.hidden = false;
  qpTitleEl.textContent = qReview ? 'Проверь ответы перед отправкой' : qData.question;
  activeQOpts = qpOptsEl;
  // State heartbeats must not rebuild the focused textarea or option controls.
  const key = JSON.stringify([JarvisQuestionAnswer.draftKey(qSessionId, qRequest), qIdx, qReview]);
  if (qpOptsEl.dataset.questionRender !== key) {
    qpOptsEl.dataset.questionRender = key;
    renderQOpts(qpOptsEl, qpFootEl); renderQCustom(qpCustomRowEl, qpCustomEl);
  } else paintQStatus();
}
function openVarPanel() {
  if (!beginQ(curSession())) return;
  varOpen = true; qWrap.hidden = false; varBtn.classList.add('open'); replyEl.blur();
  delete qpOptsEl.dataset.questionRender; renderVarPanel(curSession()); qpOptsEl.focus();
}

function closeVarPanel() {
  saveQDraft();
  const was = varOpen;
  varOpen = false;
  qWrap.hidden = true;
  varBtn.classList.remove('open');
  activeQOpts = qOptsEl;
  // Клавиатуру возвращаем в поле ответа: openVarPanel сам его разфокусировал,
  // и после ответа (или Esc) дописать реплику можно было только мышью — ровно
  // посреди главного сценария «хоткей → ↵ по сессии → ↵ по варианту».
  if (was && view === 'chat') replyEl.focus?.();
}

varBtn.addEventListener('click', () => (varOpen ? closeVarPanel() : openVarPanel()));
// Ради этой кнопки панель чата и открывают — она обязана иметь клавишу.
varBtn.title = `Выбрать вариант ответа · ${window.jarvisKeys.k('O')}`;
document.getElementById('qpClose').addEventListener('click', closeVarPanel);
document.getElementById('qScrim').addEventListener('click', closeVarPanel);

// Поле «Свой ответ…» обоих контейнеров: печать не должна дёргать навигацию
// пикера (stopPropagation режет window-обработчики); Enter — отправить,
// Esc — вернуть клавиши пикеру.
for (const el of [qCustomEl, qpCustomEl]) {
  claimKeys(el);
  el.addEventListener('input', () => { qTexts[qIdx] = el.value; saveQDraft(); paintQOptions(); });
  el.addEventListener('keydown', (e) => {
    e.stopPropagation();
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && !e.isComposing) { e.preventDefault(); submitQ(); }
    else if (e.key === 'Escape') { e.preventDefault(); el.blur(); activeQOpts.focus(); }
  });
}

// Клавиатура слайд-овера — capture-фаза, чтобы перехватить раньше обработчиков чата
window.addEventListener('keydown', (e) => {
  if (window.jarvisShortcutRecording || !document.getElementById('commandDialog').hidden) return;
  if (!varOpen) return;
  if (e.target === qpCustomEl || e.target === qCustomEl) return; // печать в «Свой ответ» — клавиши полю
  if (e.key === 'Escape') { e.preventDefault(); e.stopImmediatePropagation(); backQ(); return; }
  if (!qData || qPending || qUnknown) return;
  const stop = () => { e.preventDefault(); e.stopImmediatePropagation(); };
  if (qReview) { if (e.key === 'Enter') { stop(); submitQ(); } return; }
  if (e.key === 'ArrowDown') { stop(); qSel = Math.min(qData.options.length - 1, qSel + 1); paintQOptions(); return; }
  if (e.key === 'ArrowUp') { stop(); qSel = Math.max(0, qSel - 1); paintQOptions(); return; }
  if (e.key === ' ') { stop(); activateQ(); return; }
  if (e.key === 'Enter') { stop(); submitQ(); return; }
  if (/^[1-9]$/.test(e.key)) {
    const n = Number(e.key);
    if (n <= qData.options.length) { stop(); qSel = n - 1; activateQ(); }
  }
}, true);

/* ---------- вьюер документов (спека 2026-07-18 §3.1) ----------
 * Слайд-овер поверх чата по образцу доски задач. Тело: .md/.markdown —
 * рендер markdown.js (экранированная HTML-строка), остальное — <pre>.
 * Открывается кликом по файл-чипу карточки сводки и кнопкой «Открыть документ».
 * Инкремент 3 добавит сюда табы «Изменения/Документ» — держим тело отдельным
 * контейнером, чтобы табы легли рядом без переделки. */

const docWrap = document.getElementById('docWrap');
const docTitleEl = document.getElementById('docTitle');
const docBodyEl = document.getElementById('docBody');
const docTruncEl = document.getElementById('docTrunc');
const docTabsEl = document.getElementById('docTabs');
const docDiffEl = document.getElementById('docDiff');
const docDiffLabelEl = document.getElementById('docDiffLabel');
const docTabDiffEl = document.getElementById('docTabDiff');
const docTabDocEl = document.getElementById('docTabDoc');
let docOpen = false;
let docPath = null; // путь открытого файла — для «Редактор»/«Папка»
let docHasDiff = false;

// Переключить активный таб вьюера. «Документ» — рендер файла (docBody);
// «Изменения» — git-дифф (docDiff). Без диффа виден только «Документ».
function docSelectTab(tab) {
  const diff = tab === 'diff' && docHasDiff;
  docBodyEl.hidden = diff;
  docDiffEl.hidden = !diff;
  docTruncEl.hidden = !docTruncEl.dataset.trunc || diff;
  docTabDiffEl.classList.toggle('active', diff);
  docTabDocEl.classList.toggle('active', !diff);
  docDiffLabelEl.textContent = diff ? docDiffLabelEl.dataset.label || '' : '';
}

function openDocViewer(path, kind) {
  const sessionId=chatSessionId;
  window.JarvisArtifacts.open({sessionId,path,source:curSession()?.remote || 'Этот компьютер',
    changes: !curSession()?.remote && kind === 'edited' ? async () => { await openChat(sessionId); openLegacyDocViewer(path,kind); } : null });
}
async function openLegacyDocViewer(path, kind) {
  const res = await window.jarvis.readFile(chatSessionId, path);
  if (!res || !res.ok) { showToast((res && res.error) || 'Не удалось открыть файл'); return; }
  docPath = path;
  docTitleEl.textContent = res.name || path.split('/').pop();
  docTitleEl.title = path;
  docTruncEl.dataset.trunc = res.truncated ? '1' : '';
  docBodyEl.textContent = '';
  if (JarvisMarkdown.isMarkdownPath(path)) {
    // единственный innerHTML здесь: markdown.js полностью экранирует
    // недоверенное содержимое (см. ui/markdown.test.mjs), сырой HTML не пройдёт
    docBodyEl.innerHTML = JarvisMarkdown.render(res.content);
  } else {
    const pre = document.createElement('pre');
    pre.textContent = res.content;
    docBodyEl.appendChild(pre);
  }
  docBodyEl.scrollTop = 0;
  docOpen = true;
  docWrap.hidden = false;

  // дифф — асинхронно; таб «Изменения» по умолчанию, если ход правил док
  // (kind edited) и дифф есть, иначе сразу показываем «Документ»
  docHasDiff = false;
  docTabsEl.hidden = true;
  docSelectTab('doc');
  const diff = await window.jarvis.diffFile(chatSessionId, path);
  if (docPath !== path) return; // вьюер уже переключили на другой файл
  if (diff && diff.ok && diff.mode !== 'none' && (diff.hunks || []).length) {
    docHasDiff = true;
    docDiffLabelEl.dataset.label = diff.label || '';
    // diffview.js строит узлы через textContent (без innerHTML) — см. diffview.test.mjs
    JarvisDiffView.renderTo(docDiffEl, diff.hunks);
    docTabsEl.hidden = false;
    docSelectTab(kind === 'edited' ? 'diff' : 'doc');
  }
}

/* Свод правок задачи. Панель поверх чата: смотреть изменения человек уходит
 * из разговора, но не из задачи — возвращаться должно одним Esc. */
const changesBtn = document.getElementById('changesBtn');
const chgWrap = document.getElementById('chgWrap');
let chgOpen = false;
let chgMounted = false;

function openChanges() {
  if (!chatSessionId || !chgWrap) return;
  // Модуль экрана грузится отдельным скриптом: если он не доехал, панель
  // обязана сказать это словами, а не упасть на ReferenceError и утащить с
  // собой весь интерфейс (урок «Циклов», где ошибка отрисовки гасила раздел).
  if (typeof JarvisChanges === 'undefined') { showToast('Экран изменений не загрузился'); return; }
  if (!chgMounted) {
    JarvisChanges.mount(document.getElementById('chgBody'), window.jarvis, showToast);
    chgMounted = true;
  }
  chgOpen = true;
  chgWrap.hidden = false;
  changesBtn.classList.add('open');
  JarvisChanges.open(chatSessionId);
}

function closeChanges() {
  chgOpen = false;
  if (chgWrap) chgWrap.hidden = true;
  if (changesBtn) changesBtn.classList.remove('open');
}

if (changesBtn) changesBtn.addEventListener('click', () => (chgOpen ? closeChanges() : openChanges()));
document.getElementById('chgClose')?.addEventListener('click', closeChanges);
document.getElementById('chgScrim')?.addEventListener('click', closeChanges);
window.addEventListener('keydown', (e) => {
  if (window.jarvisShortcutRecording || !document.getElementById('commandDialog').hidden) return;
  if (e.key === 'Escape' && chgOpen) { e.preventDefault(); e.stopImmediatePropagation(); closeChanges(); }
}, true);

/* Превью: окно с локальным адресом того, что подняла задача. Адрес спрашиваем
 * и запоминаем — у каждого проекта свой порт, и набирать его каждый раз глупо. */
const previewBtn = document.getElementById('previewBtn');
let previewUrl = '';

if (previewBtn) previewBtn.addEventListener('click', async () => {
  const url = window.prompt('Адрес превью (только этот компьютер)', previewUrl || 'localhost:3000');
  if (!url) return;
  previewUrl = url;
  const res = await window.jarvis.previewOpen(url);
  if (!res || !res.ok) showToast((res && res.error) || 'Превью не открылось');
});

/* Поиск по проекту: та же панель поверх чата, что и изменения. */
const searchBtn = document.getElementById('searchBtn');
const srchWrap = document.getElementById('srchWrap');
let srchOpen = false;
let srchMounted = false;

function openSearch() {
  if (!chatSessionId || !srchWrap) return;
  if (typeof JarvisSearch === 'undefined') { showToast('Экран поиска не загрузился'); return; }
  if (!srchMounted) {
    JarvisSearch.mount(document.getElementById('srchBody'), window.jarvis, async (path) => {
      // Открываем системным редактором: панель — надзиратель, а не IDE.
      const res = await window.jarvis.openFile(chatSessionId, path, false);
      if (res && res.error) showToast(res.error);
    });
    srchMounted = true;
  }
  srchOpen = true;
  srchWrap.hidden = false;
  searchBtn.classList.add('open');
  JarvisSearch.open(chatSessionId);
}

function closeSearch() {
  srchOpen = false;
  if (srchWrap) srchWrap.hidden = true;
  if (searchBtn) searchBtn.classList.remove('open');
}

if (searchBtn) searchBtn.addEventListener('click', () => (srchOpen ? closeSearch() : openSearch()));
document.getElementById('srchClose')?.addEventListener('click', closeSearch);
document.getElementById('srchScrim')?.addEventListener('click', closeSearch);
window.addEventListener('keydown', (e) => {
  if (window.jarvisShortcutRecording || !document.getElementById('commandDialog').hidden) return;
  if (e.key === 'Escape' && srchOpen) { e.preventDefault(); e.stopImmediatePropagation(); closeSearch(); }
}, true);

function closeDocViewer() {
  docOpen = false;
  docWrap.hidden = true;
  docPath = null;
}

docTabDiffEl.addEventListener('click', () => docSelectTab('diff'));
docTabDocEl.addEventListener('click', () => docSelectTab('doc'));

document.getElementById('docClose').addEventListener('click', closeDocViewer);
document.getElementById('docScrim').addEventListener('click', closeDocViewer);
document.getElementById('docEdit').addEventListener('click', async () => {
  if (!docPath) return;
  const res = await window.jarvis.openFile(chatSessionId, docPath, false);
  if (res && res.error) showToast(res.error);
});
document.getElementById('docFinder').addEventListener('click', async () => {
  if (!docPath) return;
  const res = await window.jarvis.openFile(chatSessionId, docPath, true);
  if (res && res.error) showToast(res.error);
});
// Esc закрывает вьюер раньше «назад» чата (capture, как доска/варианты)
window.addEventListener('keydown', (e) => {
  if (window.jarvisShortcutRecording || !document.getElementById('commandDialog').hidden) return;
  if (e.key === 'Escape' && docOpen) { e.preventDefault(); e.stopImmediatePropagation(); closeDocViewer(); }
}, true);
// внешние http(s)-ссылки дока: настоящих href нет (навигация вебвью запрещена),
// клик по .md-link уходит в системный браузер через ipc
docBodyEl.addEventListener('click', (e) => {
  const a = e.target.closest('a.md-link[data-href]');
  if (a) { e.preventDefault(); window.jarvis.openUrl(a.dataset.href); }
});

// иконка статуса задачи (через DOM — без innerHTML)
function tpStatusIcon(status) {
  if (status === 'in_progress') {
    const sp = document.createElement('span');
    sp.className = 'tp-pulse';
    return sp;
  }
  const svg = svgEl('svg', { width: '15', height: '15', viewBox: '0 0 16 16', fill: 'none' });
  if (status === 'completed') {
    svg.appendChild(svgEl('circle', { cx: '8', cy: '8', r: '6.6', stroke: 'var(--accent)', 'stroke-width': '1.4' }));
    svg.appendChild(svgEl('path', { d: 'M5 8.2 L7.1 10.3 L11 5.8', stroke: 'var(--accent)', 'stroke-width': '1.5', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }));
  } else if (status === 'interrupted') {
    svg.appendChild(svgEl('circle', { cx: '8', cy: '8', r: '6.4', stroke: 'var(--warn)', 'stroke-width': '1.4' }));
    svg.appendChild(svgEl('path', { d: 'M5.4 8 H10.6', stroke: 'var(--warn)', 'stroke-width': '1.5', 'stroke-linecap': 'round' }));
  } else {
    // pending / очередь — пунктирное кольцо
    svg.appendChild(svgEl('circle', { cx: '8', cy: '8', r: '6.4', stroke: 'var(--ink-faint)', 'stroke-width': '1.4', 'stroke-dasharray': '2.5 2.5' }));
  }
  return svg;
}

// правый текст строки: модель · время / статус
function tpRight(t, stopped) {
  const parts = [];
  if (visibleModel(t.model)) parts.push(visibleModel(t.model));
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
      if (visibleModel(sa.model)) seg.push(visibleModel(sa.model));
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
  autoGrowReply();
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
  return { builtin: 'встр', project: 'проект', user: 'мои', plugin: 'плагин', codex: 'codex', kimi: 'kimi' }[src] || '';
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
  if (!window.jarvisSessionWorkspace?.capabilities(s).canConfigure) { hidePalette(); return; }
  if (kind === 'model') {
    const cur = visibleModel(s?.model).toLowerCase();
    paletteItems = modelsFor(s && s.agent).map(([val, label]) => ({
      name: label, desc: 'модель сессии', active: cur === val.toLowerCase() || cur === label.toLowerCase(),
      apply: () => applyValue('setModel', val),
    }));
  } else {
    // у codex reasoning живёт в /model-пикере — спрашиваем каталог, а не id
    if (!AGENTS.hasSeparateEffort(s && s.agent)) { hidePalette(); return; }
    paletteItems = effortsFor(s && s.agent, s && s.model).map(([val, label]) => ({
      name: label, desc: 'усилие', active: !!(s && s.effort === val),
      apply: () => applyValue('setEffort', val),
    }));
  }
  cmdSel = Math.max(0, paletteItems.findIndex((i) => i.active));
  paintPalette();
}

async function applyValue(method, val) {
  if (!chatSessionId || !window.jarvisSessionWorkspace?.capabilities(curSession()).canConfigure) return;
  const res = await window.jarvis[method](chatSessionId, val);
  replyEl.value = '';
  autoGrowReply();
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
    autoGrowReply();
    refreshPalette();
    replyEl.focus();
  } else {
    replyEl.value = '/' + c.name;
    hidePalette();
    sendReplyNow();
  }
}

/* ---------- отправка ответа: tmux-вставка или claude -p --resume ---------- */

const sendingSessions = new Set();
async function sendReplyNow() {
  const text = replyEl.value.trim(), imgs = pendingImages.slice();
  const targetId = chatSessionId, target = curSession();
  if (pendingImages.some(file => file.loading) || (!text && !imgs.length) || sendingSessions.has(targetId) || !targetId || !target?.tmuxPane || target.controlMode === 'external' || replyEl.disabled) return;
  if (window.jarvisSessionWorkspace && !window.jarvisSessionWorkspace.connectionReady()) { showToast('Нет связи с машиной. Черновик сохранён; повтори отправку после подключения.'); return; }
  sendingSessions.add(targetId);
  const pending = appendPendingReply(text, false, targetId, text, imgs);
  const status = pending.el.querySelector('.msg-status');
  const showDelivery = (text, kind = 'loading') => { window.JarvisAsyncState.status(status, text, kind); deliveryStates.set(targetId, { text, kind }); window.jarvisSessionWorkspace?.composerChanged(); };
  showDelivery(imgs.length ? 'Загружаем вложения…' : 'Отправляем…');
  replyEl.value = ''; pendingImages = []; autoGrowReply(); renderAttachments();
  window.jarvisSessionWorkspace?.saveDraft(targetId, '', []);
  gateReply(target); window.jarvisSessionWorkspace?.composerChanged();
  try {
    const paths = await window.JarvisAttachments.save(imgs, target.remote, (done, total) => showDelivery(`Загружено файлов: ${done} из ${total}`));
    const finalText = window.JarvisAttachments.prompt(text, paths);
    pending.text = finalText.trim();
    showDelivery('Отправляем…');
    const res = await window.jarvis.sendReply(targetId, finalText);
    if (!res.ok) throw new Error(res.error || (res.needsTmux ? 'Сессия вне tmux' : 'Не удалось отправить'));
    showDelivery(res.queued ? 'В очереди — агент прочитает после текущей задачи' : 'Отправлено', 'success');
  } catch (error) {
    removePending(pending);
    const draft = window.jarvisSessionWorkspace?.draft(targetId);
    window.jarvisSessionWorkspace?.saveDraft(targetId, [text, draft?.text].filter(Boolean).join('\n'), [...imgs, ...(draft?.images || [])]);
    if (chatSessionId === targetId) {
      replyEl.value = [text, replyEl.value].filter(Boolean).join('\n'); pendingImages = [...imgs, ...pendingImages];
      autoGrowReply(); renderAttachments();
    }
    deliveryStates.set(targetId, { kind: 'error', text: String(error?.message || error) });
    showToast('Не удалось отправить: ' + String(error?.message || error));
  } finally {
    sendingSessions.delete(targetId);
    gateReply(curSession()); window.jarvisSessionWorkspace?.composerChanged();
    if (chatSessionId === targetId) replyEl.focus();
  }
}

// Авто-рост поля под многострочный текст: от одной строки до max-height,
// дальше включается внутренний скролл (max-height задан в CSS).
function autoGrowReply() {
  replyEl.style.height = 'auto';
  // scrollHeight === 0, когда поле ещё скрыто (чат не показан) — не схлопываем его в 0px.
  const h = replyEl.scrollHeight;
  replyEl.style.height = (h > 0 ? h : 18) + 'px';
}

// Вставка переноса строки в позицию курсора (для Shift/Alt+Enter).
function insertNewlineAtReply() {
  const start = replyEl.selectionStart ?? replyEl.value.length;
  const end = replyEl.selectionEnd ?? replyEl.value.length;
  replyEl.value = replyEl.value.slice(0, start) + '\n' + replyEl.value.slice(end);
  const pos = start + 1;
  replyEl.setSelectionRange(pos, pos);
  autoGrowReply();
}

// ---------- вставка картинок в поле ответа ----------

function extFromType(type) {
  const m = { 'image/png': 'png', 'image/jpeg': 'jpg', 'image/gif': 'gif', 'image/webp': 'webp', 'image/bmp': 'bmp', 'image/heic': 'heic', 'image/tiff': 'tiff' };
  if (m[type]) return m[type];
  const sub = String(type || '').split('/')[1]?.toLowerCase();
  return IMG_EXTS.includes(sub) ? sub : 'png'; // незнакомый тип Rust всё равно нормализует в png
}

function renderAttachments() {
  window.JarvisAttachments.render(chatAttachEl, pendingImages, id => {
    pendingImages = pendingImages.filter(im => im.id !== id); renderAttachments();
  });
  window.jarvisSessionWorkspace?.composerChanged();
}
async function addPendingImage(file) {
  const targetId = chatSessionId;
  if (replyEl.disabled || pendingImages.length >= MAX_IMAGES) { showToast(`Не больше ${MAX_IMAGES} файлов`); return; }
  const placeholder = { id: 'reading-' + attachSeq++, name: file.name || 'Изображение', loading: true };
  pendingImages.push(placeholder); renderAttachments();
  function replaceFile(attachment) {
    const draft = chatSessionId === targetId ? { text: replyEl.value, images: pendingImages } : window.jarvisSessionWorkspace?.draft(targetId);
    if (!draft) return;
    const images = draft.images.flatMap(image => image.id === placeholder.id ? (attachment ? [attachment] : []) : [image]);
    if (chatSessionId === targetId) { pendingImages = images; renderAttachments(); }
    window.jarvisSessionWorkspace?.saveDraft(targetId, draft.text, images);
  }
  try {
    const attachment = await window.JarvisAttachments.read(file);
    replaceFile(attachment);
  } catch (error) { replaceFile(null); deliveryStates.set(targetId, { kind: 'error', title: 'Не удалось прочитать файл', text: error.message + '. Прикрепи файл ещё раз' }); showToast(error.message); }
  finally { window.jarvisSessionWorkspace?.composerChanged(); }
}
window.JarvisAttachments.bind(chatEl.querySelector('.chatinput'), addPendingImage);

replyEl.addEventListener('input', () => { if (deliveryStates.get(chatSessionId)?.kind !== 'loading') deliveryStates.delete(chatSessionId); autoGrowReply(); refreshPalette(); window.jarvisSessionWorkspace?.composerChanged(); });

replyEl.addEventListener('keydown', (e) => {
  if (e.isComposing || e.keyCode === 229) return;
  if (isMod(e)) return; // ⌘↵ — в терминал, обрабатывается глобально
  if (paletteOpen()) {
    if (e.key === 'ArrowDown') { e.preventDefault(); e.stopPropagation(); cmdSel = Math.min(paletteItems.length - 1, cmdSel + 1); paintPalette(); return; }
    if (e.key === 'ArrowUp') { e.preventDefault(); e.stopPropagation(); cmdSel = Math.max(0, cmdSel - 1); paintPalette(); return; }
    // Tab/Enter применяют команду из палитры; но Shift/Alt+Enter — это перенос строки,
    // его пропускаем дальше, к обработке ниже.
    if (e.key === 'Tab' || (e.key === 'Enter' && !e.shiftKey && !e.altKey)) { e.preventDefault(); e.stopPropagation(); paletteItems[cmdSel] && paletteItems[cmdSel].apply(); return; }
    if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); hidePalette(); return; }
  }
  if (e.key === 'Enter') {
    // Shift+Enter (везде) или Alt/Option+Enter (Win/Linux/Mac) — перенос строки.
    if (e.shiftKey || e.altKey) {
      e.preventDefault();
      e.stopPropagation();
      insertNewlineAtReply();
      return;
    }
    // Чистый Enter — отправка.
    e.preventDefault();
    e.stopPropagation();
    sendReplyNow();
  }
});

/* ---------- переход к терминалу ---------- */

async function focusTerminal(sessionId, project) {
  const res = await window.jarvis.focusTerminal(sessionId);
  // Накладка своё дело сделала — уходит с глаз. Окно так себя не ведёт:
  // из него ушли в терминал, а не закрыли его.
  if (res.ok) { if (!windowMode()) window.jarvis.hidePanel(); return; }
  // нижняя ступень лесенки — не ошибка, а чат сессии прямо в панели
  if (res.fallbackChat && view !== 'chat') openChat(sessionId, project);
  else showToast(res.error || 'Не нашёл терминал');
}

/* ---------- состояние от демона ---------- */

window.jarvis.onState((list) => {
  stateReceived = true; sessionsLoadState = 'ready'; sessionsLoadError = '';
  state = list;
  render();
  if (view === 'chat') updateChatChannelMark();
  if (view === 'question') {
    const s = state.find((x) => x.id === qSessionId);
    if (!s || !s.question) { saveQDraft(); setView('list'); render(); }
    else if (!qPending && JarvisQuestionAnswer.draftKey(s.id, s.question) !== JarvisQuestionAnswer.draftKey(qSessionId, qRequest)) {
      beginQ(s); qMessage = 'Агент задал новый вопрос. Предыдущий черновик сохранён.'; renderQuestion();
    }
  }
});
async function loadInitialSessions() {
  sessionsLoadState = 'loading'; sessionsLoadError = ''; window.jarvisSessionWorkspace?.refresh(state);
  try {
    const list = await window.jarvis.getState();
    if (!Array.isArray(list)) throw new Error('Не удалось прочитать список чатов');
    if (!stateReceived) state = list;
    sessionsLoadState = 'ready'; rebuildOrder(); render();
  } catch (error) {
    if (!stateReceived) { sessionsLoadState = 'error'; sessionsLoadError = String(error?.message || error); render(); }
  }
}
const initialStateReady = loadInitialSessions();

/* Свои агенты (qwen/opencode/внутренние CLI): нужны кнопкам «Нового проекта»
 * и командам возобновления. Старый бэкенд без реестра — просто пустой список. */
let customAgents = [];
if (typeof window.jarvis.agentsList === 'function') {
  window.jarvis.agentsList().then((r) => { if (r && r.ok) customAgents = r.agents || []; }).catch(() => {});
}

/* Один список кнопок на все экраны: раньше их было два, и они разъехались.
 * Свои агенты живут шимом на этой машине — на узле их нет. */
function launchAgents(remote) {
  const list = AGENTS.present().map((a) => ({ id: a.id, name: a.title }));
  if (!remote) for (const a of customAgents) list.push({ id: a.id, name: a.name || a.id });
  return list;
}

/* Флаг возобновления свой у каждого CLI — таблицей, а не лесенкой if-ов.
 * Свой агент без шаблона получает новую сессию, а не выдуманный флаг. */
const BUILTIN_RESUME = {
  claude: (sid) => `claude --resume ${sid}`,
  codex: (sid) => `codex resume ${sid}`,
  kimi: (sid) => `kimi -S ${sid}`,
};
function resumeBase(agent, sid) {
  const id = agent || AGENTS.DEFAULT_ID;
  const builtin = BUILTIN_RESUME[id];
  if (builtin) return builtin(sid);
  const a = customAgents.find((x) => x.id === id);
  if (a && a.resume) return a.resume.replaceAll('{sid}', sid);
  return id; // новая сессия через шим — resume у агента не описан
}

/* ---------- лимит-баннер ---------- */

const limitBannerEl = document.getElementById('limitBanner');
let limitInfo = null;

function paintLimitBanner() {
  if (!limitInfo || !limitInfo.active) { limitBannerEl.hidden = true; return; }
  const min = Math.max(0, Math.round((limitInfo.resetAt - Date.now()) / 60000));
  const t = min < 60 ? `${min}м` : `${Math.floor(min / 60)}ч ${min % 60}м`;
  const reset = limitInfo.resetAt > 0 ? `сброс через ${t}` : 'время сброса неизвестно';
  const more = limitInfo.profileCount > 1 ? ` · ещё профилей: ${limitInfo.profileCount - 1}` : '';
  limitBannerEl.textContent = `${limitInfo.sourceLabel || 'Агент'}${limitInfo.plan ? ` ${limitInfo.plan}` : ''} · лимит использования · ${reset}${more}${limitInfo.autoResume ? ' — автопродолжение включено' : ''}`;
  limitBannerEl.hidden = false;
}

/* limit_get отдаёт баннер И бюджет одним ответом; событие limit-state — только
 * баннер, поэтому бюджет берём именно отсюда и не даём событию его затереть. */
window.jarvis.onLimitState((l) => { limitInfo = l; paintLimitBanner(); });
window.jarvis.getLimit().then((l) => { limitInfo = l; paintLimitBanner(); takeBudget(l); }).catch(() => {});
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

/* ---------- нижняя полоска панели (дизайн 14a) ----------
 * Слева — волна и подпись: если будящее слово включено, панель прямо говорит,
 * как её позвать, вместо невнятного «слушаю». Справа — полоска лимита подписки
 * с окном до сброса; в настройках «Внизу панели» переключается на расход. */

const footerWaveEl = document.getElementById('footerWave');
const footerLimitEl = document.getElementById('footerLimit');
const footerMeterEl = document.getElementById('footerMeter');
const footerLimitTextEl = document.getElementById('footerLimitText');

let wakeStatus = null;   // { enabled, running, listening, muted }
let footerBottom = 'limit'; // 'limit' | 'spend' — настройка «Внизу панели»
let footerUsage = null;  // последний usage_summary (для процента и расхода)

function footerText() {
  // будящее слово слышит — говорим, как позвать (14a)
  if (wakeStatus?.running && !wakeStatus.muted) return 'скажи «джарвис» — я услышу' + powerSuffix();
  const base = state.length
    ? `${state.length} ${plural(state.length, 'сессия', 'сессии', 'сессий')} · демон активен`
    : 'демон активен';
  return base + powerSuffix();
}

/** Волна «горит» только когда детектор реально слушает микрофон. */
function paintFooterWave() {
  if (!footerWaveEl) return;
  footerWaveEl.classList.toggle('is-live', !!(wakeStatus?.listening && !wakeStatus.muted));
}

/**
 * Полоска лимитов: «5ч 62% · нед 94% · до 21:59».
 *
 * Планка — по УЗКОМУ месту: сессия может быть полупустой, когда неделя уже
 * упирается в стену, и полоска только с сессией врала бы спокойствием. Время —
 * сброс того окна, что узкое.
 */
function limitStrip() {
  const o = footerUsage?.official;
  const sess = o?.session;
  const week = o?.week;
  if (!sess && !week) return null;
  const worst = (week?.pct ?? -1) > (sess?.pct ?? -1) ? week : sess;
  const pct = Math.max(0, Math.min(100, Math.round(worst.pct)));
  const parts = [];
  if (sess && typeof sess.pct === 'number') parts.push(`5ч ${Math.round(sess.pct)}%`);
  if (week && typeof week.pct === 'number') parts.push(`нед ${Math.round(week.pct)}%`);
  if (worst.resetAt > Date.now()) {
    const d = new Date(worst.resetAt);
    parts.push(`до ${pad2(d.getHours())}:${pad2(d.getMinutes())}`);
  }
  return { pct, text: parts.join(' · ') };
}

/** «5ч 62% · нед 94%» — или «$14.20 за день», если выбран расход. */
function paintFooterLimit() {
  if (!footerLimitEl) return;
  const bar = footerMeterEl?.firstElementChild;

  if (footerBottom === 'spend') {
    const t = footerUsage?.total;
    if (!t) { footerLimitEl.hidden = true; return; }
    const cost = (t.api || 0) + (t.plan || 0);
    footerMeterEl.hidden = true;
    footerLimitTextEl.textContent = cost > 0.005 ? `$${cost.toFixed(2)} за день` : '—';
    footerLimitEl.hidden = false;
    return;
  }

  const strip = limitStrip();
  if (!strip) {
    // Данных нет — но пустота неотличима от «всё выключено». Если добытчик
    // назвал причину, показываем факт и держим причину в подсказке: человек,
    // авторизованный только на узле, увидит здесь ровно свою ситуацию.
    const why = footerUsage?.officialError;
    if (why) {
      footerMeterEl.hidden = true;
      footerLimitTextEl.textContent = 'лимиты недоступны';
      footerLimitEl.title = why;
      footerLimitEl.hidden = false;
    } else {
      footerLimitEl.hidden = true;
    }
    return;
  }
  footerMeterEl.hidden = false;
  if (bar) bar.style.width = `${strip.pct}%`;
  footerMeterEl.classList.toggle('is-crit', strip.pct > 90);
  footerMeterEl.classList.toggle('is-warn', strip.pct > 75 && strip.pct <= 90);
  footerLimitTextEl.textContent = strip.text;
  // Чьи это проценты — важно, когда авторизаций несколько: подсказка называет
  // источник («local» или имя узла).
  const src = footerUsage?.official?.source;
  footerLimitEl.title = src && src !== 'local' ? `лимиты с узла «${src}»` : '';
  footerLimitEl.hidden = false;
}

function refreshFooterUsage() {
  window.jarvis.getUsage('day')
    .then((u) => { footerUsage = u; paintFooterLimit(); paintTitlebarLimit(); })
    .catch(() => {});
}

/* ---------- бюджет подписок в футере ----------
 * Процент отвечает на «сколько осталось», а человеку нужно «до когда хватит»:
 * 44% при спокойном темпе — это запас, при рывке — завтрашняя стена. Поэтому
 * словами идёт ЗАПАС ХОДА, а процент стоит рядом мелким.
 *
 * Провайдеров два, и складывать их нельзя: две подписки, две шкалы, у каждой
 * свой день сброса (kimi — вторник, claude — среда). Отсюда две отдельные
 * строки, а не одна общая цифра. */

const WEEKDAY = ['вс', 'пн', 'вт', 'ср', 'чт', 'пт', 'сб'];
const BUDGET_DAY_MS = 86400000;

const footerBudgetEl = document.getElementById('footerBudget');
let budgetInfo = null;

/** Одна шкала словами: `{ name, text, pct, rung, title }`. */
function budgetLine(name, p, now) {
  const day = (ms) => WEEKDAY[new Date(ms).getDay()];
  const pct = typeof p?.weekLeftPct === 'number' ? Math.round(p.weekLeftPct) : null;
  // Чисел нет — говорим это словами бюджета, а не прячем строку: молчание
  // добытчика неотличимо от «всё хорошо».
  if (!p || p.rung === 'unknown' || pct === null) {
    return { name, text: 'чисел нет', pct: null, rung: 'unknown',
             title: p?.reason || 'опросчик ещё не ходил за числами' };
  }
  /* Число в футере — то, на котором принимаются решения.
   *
   * Раньше показывался голый остаток недели, а отказывал бюджет по другому
   * числу: остаток минус резерв на последний день минус уже занятое под
   * разрешённую работу. Человек видел «осталось 30%» и получал отказ — цифра,
   * по которой нельзя предсказать поведение, хуже отсутствия цифры. Поэтому
   * крупно идёт свободное, а из чего оно сложилось — в подсказке. */
  const num = (v) => (typeof v === 'number' ? Math.round(v) : null);
  const avail = num(p.availablePct);
  const held = num(p.reservedPct) || 0;
  const keep = num(p.reservePct) || 0;
  const split =
    avail === null || (held === 0 && keep === 0)
      ? null
      : `остаток ${pct}% = свободно ${avail}%` +
        (keep ? ` + резерв на последний день ${keep}%` : '') +
        (held ? ` + занято под уже разрешённое ${held}%` : '');
  const title = [p.reason, split, p.staleNote, p.normNote].filter(Boolean).join(' · ');
  const reset = p.weekResetAt > now ? day(p.weekResetAt) : null;
  const runway = typeof p.runwayDays === 'number' ? p.runwayDays : null;
  if (runway === null) {
    // Прогноза нет (мёртвая зона, мало точек) — обещать день нельзя.
    return { name, text: reset ? `темпа пока нет, сброс ${reset}` : 'темпа пока нет',
             pct, free: avail, rung: p.rung, title };
  }
  const enough = runway >= (p.daysToReset || 0);
  const until = day(now + runway * BUDGET_DAY_MS);
  const text = `хватит до ${until}, до сброса${reset ? ` (${reset})` : ''} ${enough ? 'дотянет' : 'не дотянет'}`;
  return { name, text, pct, free: avail, rung: p.rung, title };
}

function takeBudget(l) {
  budgetInfo = l;
  paintFooterBudget();
}

function paintFooterBudget() {
  if (!footerBudgetEl) return;
  const provs = budgetInfo?.providers;
  // В режиме «расход» футер занят деньгами — двум шкалам там места нет.
  if (!provs || footerBottom !== 'limit') { footerBudgetEl.hidden = true; return; }
  const now = Date.now();
  const lines = ['claude', 'kimi'].filter((n) => provs[n]).map((n) => budgetLine(n, provs[n], now));
  footerBudgetEl.textContent = '';
  for (const l of lines) {
    const span = document.createElement('span');
    span.className = 'fbud';
    if (l.rung === 'stop' || l.rung === 'queue') span.classList.add('is-crit');
    else if (l.rung === 'routine' || l.rung === 'warn') span.classList.add('is-warn');
    span.title = l.title ? `${l.name}: ${l.title}` : '';
    span.append(`${l.name} ${l.text}`);
    // Свободное, а не голый остаток: по нему бюджет и отказывает. Разбивка —
    // в подсказке, чтобы отказ при непустом остатке не выглядел произволом.
    const shown = typeof l.free === 'number' ? l.free : l.pct;
    if (shown !== null) {
      const small = document.createElement('small');
      small.textContent = `${shown}%`;
      span.append(' ', small);
    }
    footerBudgetEl.append(span);
  }
  footerBudgetEl.hidden = lines.length === 0;
}

function refreshBudget() {
  window.jarvis.getLimit().then(takeBudget).catch(() => {});
}

window.jarvis.wakeGet?.().then((w) => { wakeStatus = w; paintFooterWave(); footerLeftEl.textContent = footerText(); }).catch(() => {});
window.jarvis.onWake?.((p) => {
  if (!p) return;
  if (p.state || p.listening !== undefined) { wakeStatus = { ...wakeStatus, ...p }; paintFooterWave(); }
});
window.jarvis.onAudioState?.((s) => {
  wakeStatus = { ...wakeStatus, listening: s?.state === 'listening', muted: !!s?.muted };
  paintFooterWave();
  footerLeftEl.textContent = footerText();
});
refreshFooterUsage();
setInterval(refreshFooterUsage, 60000);
// Бюджет ходит в сеть по своему расписанию (budget.rs) — панель только читает
// готовый кэш, поэтому минуты хватает.
setInterval(refreshBudget, 60000);

const initialSettingsReady = window.jarvis.getSettings().then((s) => {
  footerBottom = s?.footerBottom === 'spend' ? 'spend' : 'limit';
  paintFooterLimit();
  paintFooterBudget();
}).catch(() => {});
// «Внизу панели» переключили в настройках — полоска меняется без перезапуска
window.addEventListener('jarvis:footer-bottom', (e) => {
  footerBottom = e.detail === 'spend' ? 'spend' : 'limit';
  paintFooterLimit();
  paintFooterBudget();
});

/* ---------- титульная полоса оконного режима (14h) ---------- */

const tlLimitEl = document.getElementById('tlLimit');
const tlMeterEl = document.getElementById('tlMeter');
const tlLimitTextEl = document.getElementById('tlLimitText');

// светофор: декораций у окна нет, поэтому кнопки наши. «Закрыть» = спрятать
// (CloseRequested перехвачен в Rust — демон продолжает жить).
// шестерёнка титульной полосы — тот же переход, что вкладка настроек
const tlSettingsEl = document.getElementById('tlSettings');
tlSettingsEl.addEventListener('click', () => {
  if (view === 'settings') goBack(); else setView('settings');
});

document.getElementById('winClose').addEventListener('click', () => window.jarvis.winClose());
document.getElementById('winMin').addEventListener('click', () => window.jarvis.winMinimize());
// зелёная кнопка — фуллскрин, с Alt — зум по содержимому (соглашение macOS)
document.getElementById('winZoom').addEventListener('click', (e) => {
  if (e.altKey) window.jarvis.winZoom();
  else window.jarvis.winToggleFullscreen().then(syncFullscreen).catch(() => {});
});
// двойной клик по титульной полосе — зум, системная привычка
document.getElementById('titlebar').addEventListener('dblclick', (e) => {
  if (e.target.closest('button')) return;
  window.jarvis.winZoom();
});

/** Фуллскрин: убрать скругление и тень, иначе по углам экрана видны прорези. */
function syncFullscreen() {
  window.jarvis.winIsFullscreen?.()
    .then((on) => { document.documentElement.dataset.fullscreen = on ? '1' : '0'; })
    .catch(() => {});
}
// ⌃⌘F и зелёная кнопка меняют размер окна — ловим оба пути через resize
let fsTimer = null;
window.addEventListener('resize', () => {
  clearTimeout(fsTimer);
  fsTimer = setTimeout(syncFullscreen, 120);
});
syncFullscreen();

// светофор горит только у активного окна
const paintWinFocus = (on) => { document.documentElement.dataset.winFocus = on ? '1' : '0'; };
paintWinFocus(document.hasFocus());
window.jarvis.onWinFocus?.(paintWinFocus);
window.addEventListener('focus', () => paintWinFocus(true));
window.addEventListener('blur', () => paintWinFocus(false));

/** Лимит в титульной полосе — тот же расчёт, что внизу панели. */
function paintTitlebarLimit() {
  const strip = limitStrip();
  if (!strip) {
    const why = footerUsage?.officialError;
    if (why) {
      tlMeterEl.hidden = true;
      tlLimitTextEl.textContent = 'лимиты недоступны';
      tlLimitEl.title = why;
      tlLimitEl.hidden = false;
    } else {
      tlLimitEl.hidden = true;
    }
    return;
  }
  tlMeterEl.hidden = false;
  const bar = tlMeterEl.firstElementChild;
  if (bar) bar.style.width = `${strip.pct}%`;
  tlMeterEl.classList.toggle('is-crit', strip.pct > 90);
  tlMeterEl.classList.toggle('is-warn', strip.pct > 75 && strip.pct <= 90);
  tlLimitTextEl.textContent = strip.text;
  tlLimitEl.hidden = false;
}

// Режим переключили (здесь или в другом окне) — пересобрать раскладку под
// новый data-mode: список из сайдбара в стопку видов и обратно.
window.addEventListener('jarvis:appearance', () => {
  // CSS updates the layout in place. Reinitializing a module here destroyed
  // text fields and the live appearance slider on every input event.
  footerEl.hidden = !windowMode() && (view === 'chat' || view === 'question');
  if (view === 'list') render();
});

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
  const path = (d, w) => svg.appendChild(svgEl('path', { d, stroke: 'currentColor', 'stroke-width': String(w || 1.3), 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }));
  if (kind === 'coffee') {
    path('M3 6.5 H10.5 V9.5 A2.5 2.5 0 0 1 8 12 H5.5 A2.5 2.5 0 0 1 3 9.5 Z');
    path('M10.5 7 H12 A1.5 1.5 0 0 1 12 10 H10.5');
    svg.appendChild(svgEl('path', { d: 'M5.2 2.6 V4', stroke: 'var(--ink-mute)', 'stroke-width': '1.2', 'stroke-linecap': 'round' }));
    svg.appendChild(svgEl('path', { d: 'M8 2.6 V4', stroke: 'var(--ink-mute)', 'stroke-width': '1.2', 'stroke-linecap': 'round' }));
  } else if (kind === 'lid') {
    path('M2.5 10.5 Q8 4 13.5 10.5');
    svg.appendChild(svgEl('path', { d: 'M1.5 12 H14.5', stroke: 'var(--ink-mute)', 'stroke-width': '1.3', 'stroke-linecap': 'round' }));
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
  hint.append('Быстрее: набери ');
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

  if (view === 'home' || view === 'list' || stale) {
    queryEl.value = ''; // список открывается свежим: чистый поиск + фокус
    sel = 0;
    rebuildOrder();
    setView(stale ? 'home' : view);
    render();
  }
});

queryEl.addEventListener('input', () => {
  // Экран Джарвиса: то же поле фильтрует его чаты — список сессий не показан
  if (view === 'agent') { if (agentTab) agentTab.search(queryEl.value); return; }
  if (view === 'home') { window.jarvisWorkspace.refresh(); return; }
  if (view === 'history') { if (window.jarvisProjects) window.jarvisProjects.search(); else { histSel = 0; renderHistory(); } return; }
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
  const p = (d, w) => svg.appendChild(svgEl('path', { d, stroke: 'currentColor', 'stroke-width': String(w || 1.4), 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }));
  if (kind === 'pos') {
    p('M2.5 3.5 H13.5 V12.5 H2.5 Z');
    svg.appendChild(svgEl('circle', { cx: '11', cy: '6', r: '1.3', fill: 'currentColor' }));
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
  ic.appendChild((() => { const s = svgEl('svg', { width: '15', height: '15', viewBox: '0 0 16 16', fill: 'none' }); s.appendChild(svgEl('path', { d: 'M3 6.5 H10.5 V9.5 A2.5 2.5 0 0 1 8 12 H5.5 A2.5 2.5 0 0 1 3 9.5 Z M10.5 7 H12 A1.5 1.5 0 0 1 12 10 H10.5', stroke: 'currentColor', 'stroke-width': '1.3', 'stroke-linejoin': 'round' })); return s; })());
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

/* ---------- своё имя чата ---------- */

// id сессии, чьё имя правят прямо сейчас. Пока он стоит, render() не
// перерисовывает список (см. render): иначе поле умрёт от первого же пуша.
let renaming = null;

function startRename(s) {
  if (!s) return;
  // прошлую правку бросаем ДО создания новой: иначе blur старого поля прилетит
  // уже на новое и погасит его
  if (renaming) { renaming = null; render(); }
  const row = [...listEl.children].find((r) => r.dataset && r.dataset.sid === s.id);
  // Говорим вслух: молча ничего не открыть — худший вид отказа.
  if (!row) { showToast('Строка чата не видна — открой список'); return; }
  renaming = s.id;

  const inp = claimKeys(document.createElement('input'));
  inp.className = 'rename';
  inp.value = s.name || '';
  inp.placeholder = s.autoTitle || 'имя чата';
  inp.maxLength = 60; // тот же потолок, что у демона
  inp.addEventListener('keydown', (e) => {
    e.stopPropagation(); // хоткеи панели не должны мешать печатать
    if (e.key === 'Enter') { e.preventDefault(); commitRename(s.id, inp.value); }
    else if (e.key === 'Escape') { e.preventDefault(); cancelRename(); }
  });
  inp.addEventListener('click', (e) => e.stopPropagation()); // клик в поле — не открытие чата
  inp.addEventListener('blur', () => cancelRename());
  const chip = row.querySelector('.badge.chatname');
  if (chip) row.replaceChild(inp, chip); // правим на месте самого имени
  else row.insertBefore(inp, row.querySelector('.summary'));
  inp.focus();
  inp.select?.();
}

function cancelRename() {
  if (!renaming) return;
  renaming = null;
  render();
}

// Пустое значение снимает имя — так же, как в капабилити sessions.rename.
function commitRename(id, value) {
  renaming = null;
  window.jarvis.renameSession(id, value).then((res) => {
    if (!res || res.ok !== true) showToast((res && res.error) || 'Не получилось переименовать');
    else if (!res.name) showToast('Имя снято — снова автозаголовок');
    render();
  });
}

// Сессия под курсором списка либо открытая в чате — цель ⌘R и меню действий.
function currentSession() {
  return view === 'list' ? filtered()[sel]
    : view === 'chat' ? state.find((x) => x.id === chatSessionId)
    : null;
}

/* ---------- завершение сессии ---------- */

// Подтверждение вторым нажатием, как у остальных разрушительных кнопок панели:
// живого агента закрывают насовсем, и промах по пункту меню не должен этого
// стоить. Хранится id: подтверждение относится к КОНКРЕТНОЙ сессии, иначе
// «ещё раз» после смены выбора убило бы соседнюю.
let killArmed = { id: null, at: 0 };
const KILL_ARM_MS = 4000;

function killArmedFor(id) {
  return killArmed.id === id && Date.now() - killArmed.at < KILL_ARM_MS;
}

function killSession(s) {
  if (!s) return;
  if (!killArmedFor(s.id)) {
    killArmed = { id: s.id, at: Date.now() };
    showToast('Завершить сессию? Нажми ещё раз');
    return;
  }
  killArmed = { id: null, at: 0 };
  window.jarvis.killSession(s.id).then((res) => {
    if (!res || res.ok !== true) { showToast((res && res.error) || 'Не получилось завершить'); return; }
    // Говорим, что именно случилось: закрыли живого агента или убрали
    // строку от давно умершего. Разница человеку важна.
    showToast(res.killed ? 'Сессия завершена' : 'Сессия убрана из списка');
    if (res.note) showToast(res.note);
  });
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
    items.push({ label: 'Перейти в терминал', key: K(KN('enter')), run: () => focusTerminal(s.id, s.project) });
    items.push({ label: s.pinned ? 'Открепить' : 'Закрепить', key: K('P'), run: () => window.jarvis.setPin(s.id, !s.pinned) });
    items.push({ label: s.name ? 'Переименовать чат' : 'Дать чату имя', key: K('R'), run: () => startRename(s) });
    if (s.name) items.push({ label: 'Вернуть автозаголовок', key: K('R', { shift: true }), run: () => commitRename(s.id, '') });
    if (s.tmuxPane) items.push({ label: 'Где этот терминал?', key: K('G'), run: () => window.jarvis.pingTerminal(s.id) });
    items.push({
      label: killArmedFor(s.id) ? 'Точно завершить?' : 'Завершить сессию',
      key: K(KN('del'), { shift: true }),
      run: () => killSession(s),
    });
  }
  if (view !== 'chat') items.push({ label: 'Очистить завершённые', key: K(KN('del')), run: () => window.jarvis.clearFinished() });
  items.push({ label: 'Проекты и история', key: K('2'), run: () => setView('history') });
  items.push({ label: 'Машины', key: K('8'), run: () => setView('machines') });
  items.push({ label: 'Аналитика ИИ и расход', key: K('3'), run: () => setView('stats') });
  items.push({ label: 'История голоса', key: K('4'), run: () => setView('voicehist') });
  items.push({ label: 'Разговор с Джарвисом', key: K('9'), run: () => setView('agent') });
  items.push({ label: 'Настройки', key: K(','), run: () => setView('settings') });
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
  if (view === 'home') window.jarvisWorkspace.runSelected();
  else if (view === 'list') { const s = filtered()[sel]; if (s) openSession(s); }
  else if (view === 'history' && window.jarvisProjects) window.jarvisProjects.primary();
  else goBack();
});

tabSessionsEl.addEventListener('click', () => { setView('list'); render(); });

/* ---------- вкладка «Проекты» (история чатов по проектам) ---------- */

const historyEl = document.getElementById('history');
const tabHistoryEl = document.getElementById('tabHistory');
tabHistoryEl.addEventListener('click', () => setView('history'));

let historyData = [];
let histRows = []; // плоский список выбираемых строк: машины, проекты или чаты (для ↑↓/Enter)
let histSel = 0;
let historyRenderSequence = 0;
let histTrail = [];
const histRoute = () => ({ machine: histMachine, project: histProject, selected: histSel, query: queryEl.value });
let histProject = null; // ключ открытого проекта (cwd) — null = список проектов
// Уровень 0: где работать. Проекты и чаты живут ВНУТРИ выбранной машины —
// история локальной машины и история узла это разные списки.
let histMachines = [];
let histMachine = null; // id машины ('local' | имя узла); null = список машин
let histError = ''; // отказ getHistory (узел не ответил) — показываем текстом
let histNewOpen = false; // раскрыта форма «Новый проект»
let histNewPath = ''; // введённый путь переживает перерисовку списка
let histNewFocus = false; // форму только что раскрыли — увести в неё курсор ровно один раз

// Одна машина (узлов не настроено) — уровень выбора бессмысленен: это был бы
// экран из одной строки на каждом заходе. Сразу показываем её проекты.
const histMultiMachine = () => histMachines.length > 1;
const histMachineOf = (id) => histMachines.find((m) => m.id === id) || null;
const histMachineName = (id) => (histMachineOf(id)?.name) || id || 'Эта машина';
const histIsRemote = (id) => !!histMachineOf(id) && histMachineOf(id).kind === 'remote';

function histTime(ts) {
  const d = new Date(ts);
  const now = new Date();
  const same = d.toDateString() === now.toDateString();
  return same ? `${pad2(d.getHours())}:${pad2(d.getMinutes())}` : `${pad2(d.getDate())}.${pad2(d.getMonth() + 1)}`;
}

function resumeCommand(s, cwd) {
  // Подсказка для tooltip. Реальная команда собирается на бэкенде из настроек
  // «Запуска» — честно предупреждаем, что она может отличаться (прокси, dangerous-флаги).
  // У сессии с узла id несёт префикс узла — resume ждёт «голый» agentId.
  const id = s.providerSessionId || s.agentId || s.id;
  const quote = value => "'" + String(value).replaceAll("'", "'\\''") + "'";
  const env = s.providerHome ? `${s.agent === 'codex' ? 'CODEX_HOME' : 'CLAUDE_CONFIG_DIR'}=${quote(s.providerHome)} ` : '';
  const base = env + resumeBase(s.agent, quote(id));
  return (cwd ? `cd ${quote(cwd)} && ${base}` : base) + '\n(+ параметры из настроек «Запуск»)';
}

// sessionId=null — новая сессия
// сессия, иначе продолжение; cwd — директория проекта; machine — где запускать
// ('local' | имя узла). Реальную команду (терминал, прокси, флаги «опасного
// режима») собирает бэкенд session_launch, он же создаёт каталог.
/* Как запускать задачу: песочница и режим разрешений. Свойства задачи, не
 * настройки на всё разом — но выбор липкий, потому что человек обычно работает
 * пачкой однотипных задач. Продолжение сессии песочницу игнорирует: она живёт
 * там, где начиналась. */
let taskOpts = { isolate: false, mode: 'ask', task: '', container: false };
const pendingLaunches = new Set();
const TASK_MODES = [
  ['ask', 'спросит', 'Агент спрашивает перед действиями'],
  ['plan', 'план', 'Только разведка и план — файлов не тронет'],
  ['yolo', 'без спроса', 'Ничего не спрашивает: для песочницы и рутины'],
];

function renderTaskOpts(host) {
  const row = document.createElement('div');
  row.className = 'taskopts';
  // Задача сразу: иначе «поставить агенту работу» — это два шага (подними,
  // потом найди чат и напиши), и именно на втором дело откладывается.
  const task = claimKeys(Object.assign(document.createElement('input'), {
    className: 'taskinput', type: 'text', spellcheck: false,
    placeholder: 'что сделать (необязательно)', value: taskOpts.task,
  }));
  task.title = 'Уедет агенту, как только он встанет';
  task.addEventListener('input', () => { taskOpts.task = task.value; });
  task.addEventListener('keydown', (e) => { if (!e.metaKey && !e.ctrlKey) e.stopPropagation(); });
  row.appendChild(task);
  const box = Object.assign(document.createElement('button'), {
    className: 'taskopt' + (taskOpts.isolate ? ' on' : ''),
    textContent: 'песочница',
  });
  box.title = 'Отдельный worktree и ветка рядом с проектом — правки не смешаются с твоими';
  box.addEventListener('click', (e) => { e.stopPropagation(); taskOpts.isolate = !taskOpts.isolate; renderHistory(); });
  row.appendChild(box);
  // Вторая изоляция: worktree разводит файлы, контейнер — инструменты.
  const cont = Object.assign(document.createElement('button'), {
    className: 'taskopt' + (taskOpts.container ? ' on' : ''),
    textContent: 'контейнер',
  });
  cont.title = 'Запустить агента в docker: свои зависимости, не общие (нужен образ в настройках)';
  cont.addEventListener('click', (e) => { e.stopPropagation(); taskOpts.container = !taskOpts.container; renderHistory(); });
  row.appendChild(cont);
  for (const [id, label, hint] of TASK_MODES) {
    const b = Object.assign(document.createElement('button'), {
      className: 'taskopt' + (taskOpts.mode === id ? ' on' : ''),
      textContent: label,
    });
    b.title = hint;
    b.addEventListener('click', (e) => { e.stopPropagation(); taskOpts.mode = id; renderHistory(); });
    row.appendChild(b);
  }
  host.appendChild(row);
}

async function launchSession(agent, sessionId, cwd, machine) {
  const key = JSON.stringify([machine || 'local', agent, sessionId, cwd]);
  if (pendingLaunches.has(key)) return false;
  pendingLaunches.add(key);
  try {
    const opts = sessionId ? null : { ...taskOpts };
    const r = await window.jarvis.launchSession(cwd, agent, sessionId, machine || 'local', opts);
    if (!r || !r.ok) { showToast((r && r.error) || 'Не удалось запустить'); return false; }
    // На узле терминала нет и быть не может: сессия поднимается отсоединённой в
    // tmux на той стороне и приезжает к нам в список сама — так и говорим.
    // Текст задачи одноразовый: он про эту работу, а не про следующую.
    const hadTask = !sessionId && !!opts.task.trim();
    if (!sessionId && taskOpts.task === opts.task) taskOpts.task = '';
    const tail = hadTask ? ' · задача уедет, как только агент встанет' : '';
    if (r.channel === 'node') showToast(`Поднял на узле «${r.machine || machine}»${tail || ' — сессия появится в списке'}`);
    else showToast(`Запускаю в терминале…${tail}`);
    return true;
  } catch (error) { showToast('Не удалось запустить: ' + String(error)); return false; }
  finally { pendingLaunches.delete(key); }
}

function openHistMachine(id) {
  histTrail.push(histRoute()); histSel = 0;
  histMachine = id;
  histProject = null;
  histNewOpen = false;
  queryEl.value = ''; // фильтр машин к проектам не относится
  renderHistory();
}

function openHistProject(key) {
  histTrail.push(histRoute()); histSel = 0;
  histProject = key;
  queryEl.value = ''; // фильтр списка проектов внутри проекта не нужен
  renderHistory();
}

// Шаг назад: чаты → проекты → машины. Ниже машин (или когда их нет) — вкладка «Чаты».
function histBack() {
  const previous = histTrail.pop();
  if (previous) {
    histMachine = previous.machine; histProject = previous.project;
    histSel = previous.selected; queryEl.value = previous.query; histNewOpen = false;
    renderHistory(); return true;
  }
  if (histProject != null) { histProject = null; renderHistory(); return true; }
  if (histMachine != null && histMultiMachine()) { histMachine = null; histNewOpen = false; renderHistory(); return true; }
  return false;
}

async function loadHistMachines() {
  const local = [{ id: 'local', name: 'Эта машина', kind: 'local', online: true }];
  try {
    const r = typeof window.jarvis.machinesList === 'function' ? await window.jarvis.machinesList() : null;
    histMachines = Array.isArray(r) && r.length ? r : local;
  } catch { histMachines = local; }
}

async function renderHistory() {
  if (window.jarvisProjects) return window.jarvisProjects.show();
  const request = ++historyRenderSequence;
  await loadHistMachines();
  if (view !== 'history' || request !== historyRenderSequence) return;
  if (!histMultiMachine()) histMachine = histMachines[0].id;
  // узел убрали из настроек, пока вкладка была открыта — возвращаемся к выбору
  if (histMachine != null && !histMachineOf(histMachine)) { histMachine = null; histProject = null; }

  histError = '';
  if (histMachine != null) {
    let data;
    try { data = await window.jarvis.getHistory(histMachine); } catch { data = { error: 'Не удалось получить историю' }; }
    if (view !== 'history' || request !== historyRenderSequence) return;
    // узел мог не ответить: бэкенд отдаёт {error} вместо массива
    if (Array.isArray(data)) historyData = data;
    else { historyData = []; histError = (data && data.error) ? String(data.error) : 'История недоступна'; }
  } else historyData = [];

  historyEl.textContent = '';
  histRows = [];
  const selection = histSel;

  const q = queryEl.value.trim().toLowerCase();

  if (histMachine == null) { renderHistMachines(q); paintHistSel(); return; }

  let g = null;
  if (histProject != null) {
    g = historyData.find((x) => (x.cwd || x.project) === histProject);
    if (!g) histProject = null; // проект исчез с диска — назад к списку
  }

  if (!g) renderHistProjects(q);
  else renderHistChats(g, q);
  histSel = Math.max(0, Math.min(selection, histRows.length - 1));
  paintHistSel();
}

/* уровень 0: где работать — эта машина или один из узлов */
function renderHistMachines(q) {
  primaryLabelEl.textContent = 'Выбрать машину';
  historyEl.appendChild(Object.assign(document.createElement('div'), {
    className: 'hhint', textContent: 'Проекты и чаты хранятся на той машине, где работает агент — выбери, где смотреть.',
  }));

  const list = q ? histMachines.filter((m) => `${m.name} ${m.sshHost || ''}`.toLowerCase().includes(q)) : histMachines;
  if (!list.length) {
    historyEl.appendChild(Object.assign(document.createElement('div'), { className: 'empty', textContent: 'Ничего не найдено' }));
    return;
  }

  for (const m of list) {
    const idx = histRows.length;
    histRows.push({ type: 'machine', key: m.id });
    const row = document.createElement('div');
    row.className = 'hrow' + (m.online ? '' : ' off');
    row.dataset.idx = idx;
    // не на связи — всё равно выбираем: причину назовёт сама попытка запуска
    row.title = [m.sshHost || null, m.online ? null : 'не на связи', m.error || null].filter(Boolean).join('\n') || m.name;

    row.appendChild(Object.assign(document.createElement('span'), { className: 'hdot' }));
    row.appendChild(Object.assign(document.createElement('span'), { className: 'htitle', textContent: m.name }));

    const meta = [];
    if (m.kind === 'remote' && m.sshHost) meta.push(m.sshHost);
    if (!m.online) meta.push(m.error ? `не на связи · ${m.error}` : 'не на связи');
    row.appendChild(Object.assign(document.createElement('span'), { className: 'hmeta hmeta-wide', textContent: meta.join(' · ') }));
    row.appendChild(Object.assign(document.createElement('span'), { className: 'hchev', textContent: '›' }));

    row.addEventListener('mouseenter', () => { histSel = idx; paintHistSel(); });
    row.addEventListener('click', () => openHistMachine(m.id));
    historyEl.appendChild(row);
  }
}

/* уровень 1: проекты выбранной машины */
function renderHistProjects(q) {
  primaryLabelEl.textContent = 'Открыть проект';
  const remote = histIsRemote(histMachine);

  // крошка есть только когда есть куда возвращаться (узлы настроены)
  if (histMultiMachine()) {
    const head = document.createElement('div');
    head.className = 'hgroup';
    const back = Object.assign(document.createElement('span'), { className: 'hback', textContent: '‹ Машины' });
    back.addEventListener('click', histBack);
    head.appendChild(back);
    head.appendChild(Object.assign(document.createElement('span'), { textContent: histMachineName(histMachine) }));
    if (remote) head.appendChild(Object.assign(document.createElement('span'), { className: 'hcount', textContent: 'узел' }));
    historyEl.appendChild(head);
  }

  renderHistNew(remote);

  if (histError) {
    historyEl.appendChild(Object.assign(document.createElement('div'), { className: 'empty', textContent: histError }));
    return;
  }

  const groups = q
    ? historyData.filter((x) => (x.project || '').toLowerCase().includes(q) || x.sessions.some((s) => (s.title || '').toLowerCase().includes(q)))
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

  // по умолчанию под курсором первый ПРОЕКТ, а не строка создания: Enter сразу
  // после открытия вкладки должен работать как раньше
  const first = histRows.findIndex((r) => r.type === 'project');
  if (first > 0) histSel = first;
}

/* строка «Новый проект» и её форма: путь на выбранной машине + выбор агента.
   Каталог создаст бэкенд (рекурсивно, на нужной машине) — от UI нужен только путь. */
function renderHistNew(remote) {
  const idx = histRows.length;
  histRows.push({ type: 'new' });
  const row = document.createElement('div');
  row.className = 'hrow hnew' + (histNewOpen ? ' open' : '');
  row.dataset.idx = idx;
  row.title = 'Запустить агента в новой директории';
  row.appendChild(Object.assign(document.createElement('span'), { className: 'htitle', textContent: 'Новый проект' }));
  row.appendChild(Object.assign(document.createElement('span'), {
    className: 'hmeta',
    textContent: remote ? `путь на узле «${histMachineName(histMachine)}»` : 'путь на этой машине',
  }));
  row.appendChild(Object.assign(document.createElement('span'), { className: 'hchev', textContent: histNewOpen ? '−' : '+' }));
  row.addEventListener('mouseenter', () => { histSel = idx; paintHistSel(); });
  row.addEventListener('click', () => { histNewOpen = !histNewOpen; histNewFocus = histNewOpen; renderHistory(); });
  historyEl.appendChild(row);
  if (!histNewOpen) return;

  // ↵ не должен звать отсутствующий CLI
  const agents = launchAgents(remote);
  const defaultAgent = agents.find((a) => a.id === AGENTS.DEFAULT_ID) || agents[0]
    || { id: AGENTS.DEFAULT_ID, name: AGENTS.title(AGENTS.DEFAULT_ID) };

  const form = document.createElement('div');
  form.className = 'hnewform';
  const input = claimKeys(Object.assign(document.createElement('input'), {
    type: 'text', placeholder: '~/projects/my-app', value: histNewPath, spellcheck: false,
  }));
  input.addEventListener('input', () => { histNewPath = input.value; });
  // поле живёт внутри вкладки с ↑↓/↵/esc на window — гасим всплытие, иначе
  // стрелки будут двигать выбор строки вместо каретки
  input.addEventListener('keydown', (e) => {
    if (e.metaKey || e.ctrlKey) return;
    e.stopPropagation();
    if (e.key === 'Enter') { e.preventDefault(); start(defaultAgent.id); }
    else if (e.key === 'Escape') { e.preventDefault(); histNewOpen = false; renderHistory(); }
  });

  async function start(agent) {
    const path = input.value.trim();
    if (!path) { input.focus(); showToast('Укажи путь к проекту'); return; }
    const buttons = [...form.querySelectorAll('button')];
    for (const button of buttons) button.disabled = true;
    if (await launchSession(agent, null, path, histMachine)) {
      if (histNewPath.trim() === path) { histNewOpen = false; histNewPath = ''; }
      renderHistory();
    }
    for (const button of buttons) button.disabled = false;
  }

  const btn = (agent, label) => {
    const b = Object.assign(document.createElement('button'), { className: 'abtn small', textContent: label });
    b.title = `Новая сессия ${label} по этому пути`;
    b.addEventListener('click', (e) => { e.stopPropagation(); start(agent); });
    return b;
  };
  form.append(input);
  for (const a of agents) form.append(btn(a.id, a.name));
  renderTaskOpts(form);
  historyEl.appendChild(form);
  historyEl.appendChild(Object.assign(document.createElement('div'), {
    className: 'hhint',
    textContent: remote
      ? 'Каталог создастся сам. На узле сессия поднимется в tmux — терминал не откроется, чат появится в списке.'
      : `Каталог создастся сам, если его ещё нет. ↵ — ${defaultAgent.name}, кнопка — выбрать агента.`,
  }));
  // курсор уводим только при раскрытии: список перерисовывается и на каждую
  // букву в поиске — иначе фокус улетал бы из строки поиска в путь
  if (histNewFocus) { histNewFocus = false; setTimeout(() => input.focus(), 0); }
}

/* уровень 2: чаты проекта */
function renderHistChats(g, q) {
  // на узле терминала нет: сессия уходит в tmux на той стороне
  const remote = histIsRemote(histMachine);
  primaryLabelEl.textContent = remote ? 'Поднять на узле' : 'Запустить в терминале';
  const head = document.createElement('div');
  head.className = 'hgroup';
  const back = Object.assign(document.createElement('span'), { className: 'hback', textContent: '‹ Проекты' });
  back.addEventListener('click', histBack);
  head.appendChild(back);
  head.appendChild(Object.assign(document.createElement('span'), { textContent: g.project }));
  head.appendChild(Object.assign(document.createElement('span'), { className: 'hcount', textContent: `${g.count} ${plural(g.count, 'чат', 'чата', 'чатов')}` }));
  // без известной директории (g.cwd == null) новая сессия бессмысленна
  if (g.cwd) {
    for (const a of launchAgents(remote)) {
      const b = Object.assign(document.createElement('button'), { className: 'abtn small', textContent: `+ ${a.name}` });
      b.title = `Новая сессия ${a.name} в этой директории`;
      b.addEventListener('click', (e) => { e.stopPropagation(); launchSession(a.id, null, g.cwd, histMachine); });
      head.appendChild(b);
    }
  }
  historyEl.appendChild(head);
  if (g.cwd) renderTaskOpts(historyEl);

  historyEl.appendChild(Object.assign(document.createElement('div'), {
    className: 'hhint',
    textContent: remote
      ? `↵ — поднять продолжение на узле «${histMachineName(histMachine)}» (терминал не откроется) · «+ агент» — новая сессия · esc — к проектам`
      : '↵ — запустить продолжение в терминале · «+ агент» — новая сессия · esc — к проектам',
  }));

  const sessions = q ? g.sessions.filter((s) => (s.title || '').toLowerCase().includes(q)) : g.sessions;
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
    // id сессии с узла несёт префикс «узел:» — человеку показываем «голый» agentId
    const sid = String(s.agentId || s.id || '');
    row.title = [s.title || `сессия ${sid}`, resumeCommand(s, g.cwd)].join('\n');

    const title = document.createElement('span');
    title.className = 'htitle' + (s.title ? '' : ' dim');
    // заголовков с узла нет и взяться им неоткуда: показываем id (время уже
    // справа, в метаданных — дублировать его в заголовке незачем)
    title.textContent = s.title || `сессия ${sid.slice(0, 8)}`;
    row.appendChild(title);

    const meta = document.createElement('span');
    meta.className = 'hmeta';
    const parts = [];
    if (visibleModel(s.model)) parts.push(visibleModel(s.model));
    if (s.tokens) parts.push(fmtTok(s.tokens));
    parts.push(histTime(s.lastAt));
    meta.textContent = parts.join(' · ');
    row.appendChild(meta);

    row.appendChild(Object.assign(document.createElement('span'), { className: 'hcopy', textContent: remote ? 'поднять ↵' : 'запустить ↵' }));

    row.addEventListener('mouseenter', () => { histSel = idx; paintHistSel(); });
    row.addEventListener('click', () => launchSession(s.agent, s.id, g.cwd, histMachine));
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
tabVoiceEl.addEventListener('click', () => setView('voicehist'));
tabLoopsEl.addEventListener('click', () => setView('loops'));
tabBundleEl.addEventListener('click', () => setView('bundle'));
tabAgentEl.addEventListener('click', () => setView('agent'));

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
let statsMode = 'analytics';
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

let statsRenderSequence = 0;
async function renderStats() {
  const request = ++statsRenderSequence;
  if (statsMode === 'analytics' && window.jarvisAiAnalytics) {
    return window.jarvisAiAnalytics.render(statsEl, {
      onUsage: () => { statsMode = 'usage'; renderStats(); },
    });
  }
  statsEl.classList.remove('ai-analytics-host');
  let u;
  try {
    u = await window.jarvis.getUsage(statsPeriod);
    if (!u?.total || !u.window || !Array.isArray(u.series)) throw new Error(u?.error || 'Данные использования недоступны.');
  } catch (error) {
    if (view !== 'stats' || request !== statsRenderSequence) return;
    statsEl.replaceChildren();
    const note = el('div', 'meeting-status error', 'Не удалось загрузить использование: ' + String(error)); note.setAttribute('role', 'alert');
    const retry = el('button', 'j-btn', 'Повторить'); retry.addEventListener('click', renderStats);
    statsEl.append(note, retry);
    if (window.jarvisAiAnalytics) {
      const analytics = el('button', 'j-btn ai-usage-back', 'Аналитика ИИ');
      analytics.addEventListener('click', () => { statsMode = 'analytics'; renderStats(); });
      statsEl.appendChild(analytics);
    }
    return;
  }
  if (view !== 'stats' || request !== statsRenderSequence) return;
  statsEl.textContent = '';

  if (window.jarvisAiAnalytics) {
    const analytics = el('button', 'j-btn ai-usage-back', 'Аналитика ИИ');
    analytics.addEventListener('click', () => { statsMode = 'analytics'; renderStats(); });
    statsEl.appendChild(analytics);
  }

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

    if (o.source) {
      statsEl.appendChild(el('div', 'uhover', `Claude · ${o.source === 'local' ? 'этот компьютер' : `узел «${o.source}»`}${o.providerHome ? ` · ${o.providerHome}` : ''}`));
    }
    if (o.session) limitRow('Сессия', o.session.pct, `${resetText(o.session.resetAt)}${o.windowTokens ? ` · ${fmtTok(o.windowTokens)} ткн` : ''}`);
    if (o.week) limitRow('Неделя', o.week.pct, resetText(o.week.resetAt));
    if (o.weekModel) limitRow(o.weekModel.model, o.weekModel.pct, resetText(o.weekModel.resetAt));
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
window.addEventListener('jarvis:open-machines', () => { setView('machines'); render(); });
document.getElementById('tabMachines').addEventListener('click', () => { setView('machines'); render(); });
tabSettingsEl.addEventListener('click', () => {
  if (view === 'settings') goBack(); else setView('settings');
});

// кнопка «Открыть настройки» из окна онбординга
window.jarvis.onGotoSettings(() => setView('settings'));
window.jarvis.onGotoVoicehist(() => setView('voicehist'));

// Wake-word (инкр. 10): живой индикатор «слушаю»/срабатывание + рефреш после установки
window.jarvis.onAudioState((p) => {
  const pill = document.getElementById('wake-status-pill');
  if (!pill || !p) return;
  let txt = 'выключено', cls = '';
  if (p.muted || p.state === 'muted') txt = 'заглушено';
  else if (p.state === 'permission-pending') txt = 'ожидаем разрешения';
  else if (p.state === 'starting') txt = 'подключаем микрофон';
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

// подписи клавиш живут в keys.js — там же ветвление macOS/Linux
const displayHotkey = (acc) => window.jarvisKeys.displayHotkey(acc);
const K = (key, opts) => window.jarvisKeys.k(key, opts);
const KN = (name) => window.jarvisKeys.NAMES[name] || name;

/** Нажат ли главный модификатор приложения. На маке это ⌘ (metaKey), на Linux —
 *  Ctrl: Super там принадлежит окружению рабочего стола (в GNOME Super+1..4
 *  переключает приложения дока, и панель бы с ним дралась). */
const isMod = (e) => (window.jarvisKeys.isMac ? e.metaKey : e.ctrlKey);

let settingsLoadSequence = 0;
async function loadSettings() {
  const sequence = ++settingsLoadSequence;
  // Новая страница настроек (settings2.js): сайдбар + детальные панели в дизайне
  // редизайна, проводка к тем же IPC. initSettings2 сам чистит и строит host.
  // Старая разметка #settings затирается; её top-level обработчики остаются
  // привязанными к detached-узлам (безопасно), карточки no-op (нет их DOM).
  try { plugins = await window.jarvis.getPlugins(); } catch {}
  if (view !== 'settings' || sequence !== settingsLoadSequence) return;
  settingsEl.style.cssText = 'padding:0;height:100%;overflow:hidden';
  try {
    window.initSettings2(settingsEl);
  } catch (e) {
    console.error('[settings2] init:', e);
  }
}

/* ── карточка «История диктовки»: что я говорил + копирование/очистка ── */
async function renderTranscriptsCard() {
  const box = document.getElementById('transcriptsCard');
  if (!box) return;
  box.textContent = '';
  let items = [];
  try { const r = await window.jarvis.transcriptsGet(); items = (r && r.items) || []; } catch {}

  const head = document.createElement('div');
  head.className = 'awakehead';
  head.appendChild(Object.assign(document.createElement('span'), { className: 'atitle', textContent: 'История диктовки' }));
  head.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
  if (items.length) {
    const copyAll = document.createElement('button');
    copyAll.className = 'abtn small';
    copyAll.textContent = 'Копировать всё';
    copyAll.addEventListener('click', () => {
      try { navigator.clipboard.writeText(items.map((i) => i.text).join('\n')); } catch {}
      copyAll.textContent = 'Скопировано'; setTimeout(() => { copyAll.textContent = 'Копировать всё'; }, 1500);
    });
    head.appendChild(copyAll);
    const clr = document.createElement('button');
    clr.className = 'abtn danger small'; clr.style.marginLeft = '8px'; clr.textContent = 'Очистить';
    let armed = false;
    clr.addEventListener('click', async () => {
      if (!armed) { armed = true; clr.textContent = 'Точно?'; setTimeout(() => { armed = false; clr.textContent = 'Очистить'; }, 3000); return; }
      try { await window.jarvis.transcriptsClear(); } catch {}
      renderTranscriptsCard();
    });
    head.appendChild(clr);
  }
  box.appendChild(head);

  if (!items.length) {
    const hint = document.createElement('div');
    hint.className = 'ahint';
    hint.textContent = 'Пока пусто. Скажи что-нибудь через диктовку (F8) или «Hey Jarvis».';
    box.appendChild(hint);
    return;
  }

  for (const it of items) {
    const wrap = document.createElement('div');
    const r = document.createElement('div');
    r.className = 'istat hairtop on';
    r.appendChild(Object.assign(document.createElement('span'), { className: 'dot' }));
    const txt = document.createElement('span');
    txt.textContent = it.text;
    txt.style.cssText = 'flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;';
    r.appendChild(txt);
    r.appendChild(Object.assign(document.createElement('span'), { className: 'spacer' }));
    const meta = document.createElement('span');
    meta.className = 'sz';
    const d = new Date((it.ts || 0) * 1000);
    const hh = String(d.getHours()).padStart(2, '0'), mm = String(d.getMinutes()).padStart(2, '0');
    meta.textContent = `${it.source === 'wake' ? '🎙' : '⌨'} ${hh}:${mm}`;
    r.appendChild(meta);
    // → Промпт: преобразовать надиктованное через LLM (ишку)
    const enh = document.createElement('button');
    enh.className = 'abtn small primary'; enh.style.marginLeft = '10px'; enh.textContent = '→ Промпт';
    enh.addEventListener('click', async () => {
      enh.disabled = true; enh.textContent = 'Думаю…';
      let res = null;
      try { res = await window.jarvis.transcriptEnhance(it.text, 'prompt'); } catch (e) {}
      enh.disabled = false; enh.textContent = '→ Промпт';
      if (res && res.ok && res.result) showEnhanceResult(wrap, res.result);
      else showToast('Не удалось: ' + ((res && res.error) || 'быстрая модель недоступна'));
    });
    r.appendChild(enh);
    const cp = document.createElement('button');
    cp.className = 'abtn small'; cp.style.marginLeft = '8px'; cp.textContent = 'Копировать';
    cp.addEventListener('click', () => {
      try { navigator.clipboard.writeText(it.text); } catch {}
      cp.textContent = 'OK'; setTimeout(() => { cp.textContent = 'Копировать'; }, 1200);
    });
    r.appendChild(cp);
    wrap.appendChild(r);
    box.appendChild(wrap);
  }
}

// показать результат преобразования (промпт) под репликой + кнопка копирования
function showEnhanceResult(wrap, text) {
  const old = wrap.querySelector('.enh-result');
  if (old) old.remove();
  const res = document.createElement('div');
  res.className = 'enh-result';
  res.style.cssText = 'margin:6px 0 10px 22px;padding:10px;border-radius:8px;background:rgba(108,160,255,.10);';
  const t = document.createElement('div');
  t.textContent = text;
  t.style.cssText = 'white-space:pre-wrap;font-size:13px;line-height:1.45;';
  res.appendChild(t);
  const cp = document.createElement('button');
  cp.className = 'abtn small primary'; cp.style.marginTop = '8px'; cp.textContent = 'Копировать промпт';
  cp.addEventListener('click', () => {
    try { navigator.clipboard.writeText(text); } catch {}
    cp.textContent = 'Скопировано'; setTimeout(() => { cp.textContent = 'Копировать промпт'; }, 1500);
  });
  res.appendChild(cp);
  wrap.appendChild(res);
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
    h.textContent = `При удалении сохранятся ${info.foreign_hooks} ${plural(info.foreign_hooks, 'чужой хук', 'чужих хука', 'чужих хуков')} — трогаем только свои.`;
    box.appendChild(h);
  }

  // тумблер тихого режима (разработчик)
  box.appendChild(arow('Тихий режим (разработчик)',
    stoggle(!!info.quiet, (v) => window.jarvis.quietSet(v)), { hairtop: true }));
  const qhint = document.createElement('div');
  qhint.className = 'ahint';
  qhint.textContent = `Фон копит статистику с хуков, но без тостов/голоса/показа. Тумблер — ${K('J', { alt: true })}.`;
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
  if (v.audio_state === 'permission-pending') return ['ожидаем разрешения', ''];
  if (v.audio_state === 'starting') return ['подключаем микрофон', ''];
  if (v.audio_state === 'no-device') return ['микрофон не найден', ''];
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
  sel.style.cssText = 'background:transparent;border:1px solid var(--line-strong);border-radius:6px;color:var(--text);font:inherit;font-size:12px;padding:3px 7px;outline:none;';
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

// Автозапуск — отдельно: система может отказать (LaunchAgent на macOS,
// autostart-каталог на Linux), поэтому после
// переключения перечитываем РЕАЛЬНОЕ состояние из системы и честно говорим,
// если не сработало. Иначе галка «врёт», что включила.
document.getElementById('openAtLogin').addEventListener('change', async (e) => {
  const want = e.target.checked;
  await window.jarvis.setSettings({ openAtLogin: want });
  const s = await window.jarvis.getSettings();
  e.target.checked = !!s.openAtLogin; // отражаем то, что реально записалось в систему
  if (!!s.openAtLogin !== want) {
    showToast(want ? 'Система не дала включить автозапуск' : 'Не вышло выключить автозапуск');
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
  if (e.metaKey) mods.push(window.jarvisKeys.isMac ? 'Command' : 'Super');
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
  if (window.jarvisShortcutRecording || !document.getElementById('commandDialog').hidden) return;
  if (e.defaultPrevented || e.isComposing || e.keyCode === 229) return;
  if (e.key === 'Escape' && window.jarvisSessionWorkspace?.dismissMenu()) {
    e.preventDefault(); e.stopImmediatePropagation(); return;
  }
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

  // Поле само разбирает простые клавиши (см. ownsKeys): без этой уступки Esc в
  // правке имени прятал панель, а ↵ проваливался в чат — до коммита имени.
  // Комбинации с модификатором остаются за панелью: ⌘W, ⌘⌫ и прочее.
  if (!isMod(e) && ownsKeys(e.target)) return;

  if (view === 'chat' && isMod(e) && e.key === 'Enter') { // ⌘↵ из чата — в терминал
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
    else if (e.key === 'Escape' || (isMod(e) && window.jarvisKeys.matches(e, 'k'))) { e.preventDefault(); closeActions(); }
    return;
  }

  // Оконные сочетания. Своего меню у приложения нет, поэтому системные
  // «закрыть»/«свернуть»/«фуллскрин» не привязываются сами — вешаем руками,
  // чтобы окно вело себя как окно. Клавиши берём по факту нажатия (metaKey —
  // ⌘ на маке, Super на Linux), а не по зашитой раскладке.
  if (windowMode() && isMod(e) && window.jarvisKeys.matches(e, 'w')) { // закрыть (спрятать)
    e.preventDefault();
    window.jarvis.winClose();
    return;
  }
  if (windowMode() && isMod(e) && !e.shiftKey && window.jarvisKeys.matches(e, 'm')) { // свернуть
    e.preventDefault();
    window.jarvis.winMinimize();
    return;
  }
  if (windowMode() && isMod(e) && e.shiftKey && window.jarvisKeys.matches(e, 'f')) { // фуллскрин
    e.preventDefault();
    window.jarvis.winToggleFullscreen().then(syncFullscreen).catch(() => {});
    return;
  }

  if (isMod(e) && e.shiftKey && window.jarvisKeys.matches(e, 'k')) { e.preventDefault(); toggleActions(); return; }
  if (isMod(e) && window.jarvisKeys.matches(e, 'k')) { // Searchable workspace commands
    e.preventDefault();
    closeActions();
    e.stopImmediatePropagation();
    window.jarvisWorkspace?.toggleCommands();
    return;
  }

  if (isMod(e) && e.key === '9') { e.preventDefault(); setView('agent'); return; }
  if (isMod(e) && e.key === '0') { e.preventDefault(); setView('home'); render(); return; }
  if (isMod(e) && e.key === '1') { // ⌘1 — Чаты
    e.preventDefault();
    setView('list');
    render();
    return;
  }
  if (isMod(e) && e.key === '2') { // ⌘2 — История
    e.preventDefault();
    setView('history');
    return;
  }
  if (isMod(e) && e.key === '3') { // ⌘3 — Статистика
    e.preventDefault();
    setView('stats');
    return;
  }
  if (isMod(e) && e.key === '4') { // ⌘4 — История голоса
    e.preventDefault();
    setView('voicehist');
    return;
  }
  if (isMod(e) && e.key === '5') { // ⌘5 — Циклы
    e.preventDefault();
    setView('loops');
    return;
  }
  if (isMod(e) && e.key === '6') { // ⌘6 — Связка
    e.preventDefault();
    setView('bundle');
    return;
  }

  if (isMod(e) && e.key === '7') { e.preventDefault(); setView('meetings'); return; }
  if (isMod(e) && e.key === '8') { e.preventDefault(); setView('machines'); return; }

  // Focused editors own arrows, Enter and text editing, except the main search.
  if (e.target !== queryEl && !isMod(e) && e.key !== 'Escape' &&
      (editingText(e.target) || e.target?.closest?.('button, a, [role="button"]'))) return;
  if (e.key === 'Escape' && e.target?.id === 'settingsSearch' && e.target.value) return;
  // An open settings editor/menu dismisses before the surrounding page.
  if (e.key === 'Escape' && e.target?.closest?.('[data-escape-owner], #settings2 .cselect.open')) return;
  if (e.key === 'Escape' && e.target?.tagName === 'SELECT') { e.target.blur(); return; }

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

  if (view === 'history') { // ↑↓ выбор · ↵ выбрать машину / открыть проект / запустить · esc — на уровень вверх
    if (window.jarvisProjects) { window.jarvisProjects.key(e); return; }
    if (e.key === 'ArrowDown') { e.preventDefault(); histSel = Math.min(histRows.length - 1, histSel + 1); paintHistSel(); return; }
    if (e.key === 'ArrowUp') { e.preventDefault(); histSel = Math.max(0, histSel - 1); paintHistSel(); return; }
    if (e.key === 'Enter' && histRows[histSel]) {
      e.preventDefault();
      const r = histRows[histSel];
      if (r.type === 'machine') openHistMachine(r.key);
      else if (r.type === 'project') openHistProject(r.key);
      else if (r.type === 'new') { histNewOpen = true; histNewFocus = true; renderHistory(); }
      else launchSession(r.s.agent, r.s.id, r.cwd, r.s.remote || histMachine || 'local'); // машина сессии важнее текущего уровня
      return;
    }
    if (e.key === 'Escape') {
      e.preventDefault();
      goBack();
      return;
    }
    return; // прочее (печать в поиск) — пусть идёт в инпут
  }

  if (e.key === 'Escape' && view === 'question') { e.preventDefault(); backQ(); return; }
  if (e.key === 'Escape') { // raycast: Esc — назад / закрыть
    if (view === 'chat' && paletteOpen()) return; // палитру закроет обработчик поля
    e.preventDefault();
    goBack();
    return;
  }

  // экран вопроса — только клавиатура. «Свой ответ…» помечен ownkeys (см.
  // выше): без этого в него нельзя было напечатать ни пробела, ни цифры —
  // ветка глотала их, не глядя на e.target, в отличие от близнеца-слайд-овера
  if (view === 'question' && qData) {
    if (qPending || qUnknown) return;
    if (qReview) { if (e.key === 'Enter') { e.preventDefault(); submitQ(); } return; }
    if (e.key === 'ArrowDown') { e.preventDefault(); qSel = Math.min(qData.options.length - 1, qSel + 1); paintQOptions(); return; }
    if (e.key === 'ArrowUp') { e.preventDefault(); qSel = Math.max(0, qSel - 1); paintQOptions(); return; }
    if (e.key === ' ') { e.preventDefault(); activateQ(); return; }
    if (e.key === 'Enter') { e.preventDefault(); submitQ(); return; }
    if (/^[1-9]$/.test(e.key)) {
      const n = Number(e.key);
      if (n <= qData.options.length) { e.preventDefault(); qSel = n - 1; activateQ(); }
      return;
    }
    return; // прочие клавиши экран вопроса проглатывает
  }

  if (isMod(e) && e.key === ',') { // модификатор + «,» — настройки, как принято в системе
    e.preventDefault();
    if (view === 'settings') goBack(); else setView('settings');
    return;
  }

  if (isMod(e) && e.shiftKey && e.key === 'Backspace') { // завершить сессию
    if (editingText()) return;
    e.preventDefault();
    killSession(view === 'list' ? filtered()[sel] : state.find((x) => x.id === chatSessionId));
    return;
  }

  if (isMod(e) && e.key === 'Backspace') { // очистить завершённые
    // НЕ в поле ввода: иначе ⌘⌫ (удалить до начала строки) при печати в чате
    // молча сносил все Done/Idle сессии. В поле — отдаём комбо нативному редактору.
    if (editingText()) return;
    e.preventDefault();
    window.jarvis.clearFinished();
    return;
  }

  if (isMod(e) && window.jarvisKeys.matches(e, 'p')) { // ⌘P — закрепить/открепить
    e.preventDefault();
    const s = view === 'list' ? filtered()[sel]
      : view === 'chat' ? state.find((x) => x.id === chatSessionId)
      : null;
    if (s) window.jarvis.setPin(s.id, !s.pinned);
    return;
  }

  // ⌘R — имя чата, ⇧⌘R — вернуть авто. Поле поиска не помеха (как и у ⌘P):
  // само поле правки имени свои нажатия не пропускает наверх.
  if (isMod(e) && (e.key === 'r' || e.key === 'R')) {
    e.preventDefault();
    const s = currentSession();
    if (!s) return;
    if (e.shiftKey) { if (s.name) commitRename(s.id, ''); }
    else startRename(s);
    return;
  }

  // ⌘O — варианты ответа. Закрыв слайд-овер по Esc, открыть его снова было
  // нечем: ни у одной кнопки шапки чата сочетания не было, а эту открывают чаще
  // всех остальных вместе.
  if (isMod(e) && (e.key === 'o' || e.key === 'O')) {
    if (view !== 'chat' || varBtn.hidden) return;
    e.preventDefault();
    if (varOpen) closeVarPanel(); else openVarPanel();
    return;
  }

  if (isMod(e) && window.jarvisKeys.matches(e, 'g')) { // ⌘G — «где это?»: оверлей в терминале
    e.preventDefault();
    const s = view === 'list' ? filtered()[sel] : state.find((x) => x.id === chatSessionId);
    if (s) window.jarvis.pingTerminal(s.id).then((res) => {
      if (!res.ok) showToast(res.error || 'Не получилось');
    });
    return;
  }

  if (view === 'stats') { // ←→ период · 1-3 разрез · ↑↓ скролл
    if (statsMode === 'analytics' && window.jarvisAiAnalytics) return;
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
      if (isMod(e)) focusTerminal(list[sel].id, list[sel].project); // ⌘↵ — прыжок в терминал
      else openChat(list[sel].id, list[sel].project); // ↵ — чат сессии
    }
  }
  if (view === 'home' && window.jarvisWorkspace.rootKey(e)) e.preventDefault();
}, true);

window.JarvisArtifacts?.configure({ comment: async (sessionId, text) => { if (chatSessionId !== sessionId || view !== 'chat') await openChat(sessionId); replyEl.value = [replyEl.value,text].filter(Boolean).join('\n\n'); autoGrowReply(); window.jarvisSessionWorkspace?.saveDraft(sessionId,replyEl.value,pendingImages); replyEl.focus(); } });
window.initWorkspace?.({ navigate: next => { setView(next); render(); }, back: goBack, sessions: () => state, openSession, toast: showToast });
window.initSessionWorkspace?.({
  navigate: next => { setView(next); render(); }, sessions: () => state,
  currentId: () => chatSessionId, openSession, toast: showToast, trackLaunchMessage,
  send: sendReplyNow, attach: addPendingImage, hasAttachments: () => pendingImages.some(file => !file.loading),
  sessionLoadState: () => sessionsLoadState, sessionLoadError: () => sessionsLoadError, retrySessions: loadInitialSessions,
  deliveryState: () => deliveryStates.get(chatSessionId), readingFiles: () => pendingImages.filter(file => file.loading).length,
  models: modelsFor, efforts: effortsFor,
  terminal: s => focusTerminal(s.id, s.project),
  settingsPane: pane => window.jarvisOpenSettingsPane?.(pane),
});
window.initProjects?.({
  visible: () => view === 'history', sessions: () => state,
  query: () => queryEl.value, setQuery: text => { queryEl.value = text; },
  back: goBack, openSession,
  newChat: project => window.jarvisSessionWorkspace.newChat(project),
  settings: () => { setView('machines'); },
});

setView(view);
render();

// Each native workspace has its own chat subscription and route. Wait for the
// first snapshot so a newly opened window can resolve the requested session.
let workspaceRouteSequence = 0;
async function applyWorkspaceRoute(route = {}, initial = false) {
  if (!initial && !route.sessionId && !route.project) return;
  const request = ++workspaceRouteSequence;
  await Promise.all([initialStateReady, initialSettingsReady]);
  if (request !== workspaceRouteSequence) return;
  if (route.sessionId) {
    await openChat(route.sessionId, route.project);
  } else if (route.project && window.jarvisSessionWorkspace) {
    await window.jarvisSessionWorkspace.focusProject(route.project, route.remote);
  } else { setView('list'); render(); }
}
window.jarvis.onWorkspaceRoute?.(route => applyWorkspaceRoute(route));
if (window.__JARVIS_WORKSPACE__) applyWorkspaceRoute(window.__JARVIS_WORKSPACE__, true);
