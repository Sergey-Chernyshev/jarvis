/* Панель целиком в настоящем DOM: разметка, скрипты и клики.
 *
 * Остальные тесты проверяют модули на подставном DOM — это ловит логику, но не
 * ловит того, чем панель ломается на живом маке: разъехавшиеся id, порядок
 * скриптов, кнопка, которая никуда не подключена. Здесь грузится НАСТОЯЩИЙ
 * index.html со всеми скриптами (кроме моста к Tauri — его подменяем), и по
 * кнопкам действительно кликают.
 *
 * Это не замена запуску приложения, но именно то, что можно проверить в CI на
 * macOS: панель поднимается, чат открывается, новые экраны открываются и
 * спрашивают у бэкенда ровно то, что должны.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const HERE = new URL('./', import.meta.url);
const read = (name) => readFileSync(new URL(name, HERE), 'utf8');

/* Скрипты панели в том же порядке, что в index.html. bridge.js пропускаем: он
 * зовёт Tauri, которого в тесте нет, — вместо него ставим свой window.jarvis. */
const SCRIPTS = [
  'theme.js',
  // подписи клавиш: renderer зовёт window.jarvisKeys уже при первой отрисовке
  'keys.js',
  'icons.js',
  'markdown.js',
  'diffview.js',
  'question-answer.js',
  'settings2.js',
  'voice-history.js',
  'loops.js',
  'bundle.js',
  'changes.js',
  'search.js',
  'navigation.js',
  'workspace.js',
  'meetings.js',
  'ai-analytics.js',
  'async-state.js', 'attachments.js',
  'async-state.js', 'attachments.js',
  'renderer.js',
];

/* Мост-заглушка. Любой неизвестный метод возвращает разумную пустоту, иначе
 * тест превратился бы в список из сотни моков и ломался бы от каждой新ой
 * команды. Вызовы записываем — по ним и проверяем, что кнопки делают дело. */
function makeBridge(calls, data = {}) {
  const subs = {};
  const target = {
    getState: async () => data.getState ? data.getState() : data.state || [],
    getSettings: async () => ({}),
    getMeta: async () => ({ version: 'test' }),
    getLimit: async () => null,
    analyticsReport: async () => ({ schemaVersion: 1, generatedAt: 0, sessions: [], projects: [], models: [], outcomes: [], summary: { sessions: 0, harnessScore: null }, coverage: { sessions: 0, filesScanned: 0 }, economics: {} }),
    sessionChanges: async () => ({ ok: true, branch: 'dev', files: data.files || [] }),
    sessionChangeDiff: async () => ({ ok: true, hunks: [] }),
    sessionSearch: async () => ({ ok: true, hits: data.hits || [], capped: false }),
    // Открытие чата отдаёт ленту ходов — панель сразу читает items/spans.
    openChat: async (...args) => data.openChat ? data.openChat(...args) : ({ ok: true, items: [], spans: [], cards: {} }),
    chatHistory: async () => ({ ok: true, items: [] }),
    // Списочные команды обязаны отдавать список: панель зовёт .find/.map по
    // ним прямо на старте, и объект вместо массива гасит весь запуск.
    getPlugins: async () => [],
    getModels: async () => [],
    getAgents: async () => [],
    machinesList: async () => [{ id: 'local', name: 'Эта машина', kind: 'local' }],
    getCommands: async () => [],
    getPrompts: async () => [],
    transcriptsGet: async () => [],
    historyGet: async () => ({ ok: true, items: [] }),
    loopsGet: async () => ({ ok: true, loops: [] }),
    bundleGet: async () => ({ ok: true, bundles: [] }),
    meetingsSources: async () => [{ id: 'microphone', label: 'Микрофон', available: true }],
    meetingsList: async () => [],
    meetingsStatus: async () => null,
    sttGet: async () => ({ engine: 'whisper-turbo', engines: ['whisper-turbo'] }),
    sttInputDevices: async () => data.sttInputDevices ? data.sttInputDevices() : ({ devices: [], current: null }),
    ...data.bridge,
  };
  return new Proxy(target, {
    get(obj, prop) {
      if (typeof prop !== 'string') return undefined;
      if (prop in obj) {
        return (...args) => {
          calls.push([prop, ...args]);
          return obj[prop](...args);
        };
      }
      // on*(cb) — подписка: запоминаем, чтобы тест мог толкнуть событие.
      if (/^on[A-Z]/.test(prop)) {
        return (cb) => {
          subs[prop] = cb;
          return () => {};
        };
      }
      return (...args) => {
        calls.push([prop, ...args]);
        return Promise.resolve({ ok: true });
      };
    },
  });
}

async function boot(data = {}) {
  // linkedom, а не jsdom: нам нужен настоящий DOM с событиями, а не браузер.
  // Скрипты панели выполняем сами — ровно те же файлы, что грузит index.html.
  const { window, document } = parseHTML(read('index.html'));
  window.__JARVIS_WORKSPACE__ = data.workspace || null;
  window.__JARVIS_SURFACE__ = data.surface || (data.workspace ? 'workspace' : null);
  const calls = [];
  const subs = {};
  // Подписки ловим отдельно: Proxy отдаёт функцию, а нам нужен сам колбэк.
  window.jarvis = new Proxy(makeBridge(calls, data), {
    get(obj, prop) {
      if (typeof prop === 'string' && /^on[A-Z]/.test(prop)) {
        return (cb) => { subs[prop] = cb; return () => {}; };
      }
      return obj[prop];
    },
  });
  window.matchMedia = window.matchMedia || (() => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
  window.ResizeObserver = window.ResizeObserver || class { observe() {} unobserve() {} disconnect() {} };
  window.IntersectionObserver = window.IntersectionObserver || class { observe() {} unobserve() {} disconnect() {} };
  window.scrollTo = () => {};
  window.HTMLElement.prototype.scrollIntoView = () => {};
  // Панель помнит мелочи (тема, размеры) в localStorage; в тесте достаточно
  // честной пустой памяти, иначе первый же скрипт падает на ReferenceError.
  const memory = new Map();
  window.localStorage = {
    getItem: (k) => (memory.has(k) ? memory.get(k) : null),
    setItem: (k, v) => memory.set(k, String(v)),
    removeItem: (k) => memory.delete(k),
    clear: () => memory.clear(),
  };
  window.requestAnimationFrame = (cb) => setTimeout(() => cb(Date.now()), 0);
  window.cancelAnimationFrame = (id) => clearTimeout(id);
  document.hasFocus = () => true;

  for (const name of SCRIPTS) {
    // Скрипт панели, выполненный в её окне: ошибка здесь — ровно та, от
    // которой на маке остаётся белый экран.
    const fn = new Function(
      'window', 'document', 'globalThis', 'localStorage', 'navigator',
      'setTimeout', 'clearTimeout', 'setInterval', 'clearInterval',
      'requestAnimationFrame', 'cancelAnimationFrame', 'CustomEvent', 'Event',
      read(name)
    );
    fn(
      window, document, window, window.localStorage, window.navigator || { platform: 'MacIntel' },
      // Периодику панели глушим: она нужна живому окну, а тест иначе никогда
      // не закончится — процесс держат её таймеры.
      setTimeout, clearTimeout, () => 0, () => {},
      window.requestAnimationFrame, window.cancelAnimationFrame, window.CustomEvent, window.Event
    );
  }
  // Дать промисам старта (getState/getSettings/getMeta) отработать.
  await new Promise((r) => setTimeout(r, 0));
  if (data.startAt === 'list') document.getElementById('tabSessions').dispatchEvent(click(document));
  return { window, doc: document, calls, subs };
}

/* Клик и клавиша: linkedom даёт Event, а специализированных конструкторов у
 * него нет — обработчики панели смотрят только на key и на всплытие. */
function click(doc) {
  return new doc.defaultView.Event('click', { bubbles: true });
}

function key(doc, k) {
  const e = new doc.defaultView.Event('keydown', { bubbles: true });
  e.key = k;
  return e;
}

const SESSION = {
  id: 's1',
  status: 'waiting',
  detail: 'ждёт ответа',
  project: 'jarvis',
  cwd: '/srv/jarvis',
  agent: 'claude',
  tmuxPane: '%1',
  updatedAt: Date.now(),
  createdAt: Date.now(),
};

test('quick launcher opens a real workspace and reports native open failures', async () => {
  const ui = await boot({ bridge: { openWorkspace: async () => ({ ok: false, error: 'Window unavailable' }) } });
  ui.doc.getElementById('openWorkspace').dispatchEvent(click(ui.doc));
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.deepEqual(ui.calls.find(c => c[0] === 'openWorkspace'), ['openWorkspace', {}]);
  assert.match(ui.doc.querySelector('.toast').textContent, /Window unavailable/);
});

test('home shows six recent chats while search still covers every session', async () => {
  const state = Array.from({ length: 20 }, (_, i) => ({ ...SESSION, id: 'recent-' + i, title: 'Задача ' + i, updatedAt: i + 1 }));
  const ui = await boot({ state, surface: 'quick' });
  assert.equal(ui.doc.querySelectorAll('.launcher-chat').length, 6);
  assert.equal(ui.doc.querySelector('.launcher-chat strong').textContent, 'Задача 19');
  const search = ui.doc.getElementById('query'); search.value = 'Задача';
  search.dispatchEvent(new ui.window.Event('input', { bubbles: true }));
  assert.equal(ui.doc.querySelectorAll('.launcher-chat').length, 20);
  search.value = 'Задача 0'; search.dispatchEvent(new ui.window.Event('input', { bubbles: true }));
  assert.equal(ui.doc.querySelectorAll('.launcher-chat').length, 2, 'search includes older chats outside the six recent entries');
  search.value = ''; search.dispatchEvent(new ui.window.Event('input', { bubbles: true }));
  assert.equal(ui.doc.querySelectorAll('.launcher-chat').length, 6);
});

test('workspace startup waits for state and a newer route replaces the initial chat', async () => {
  let resolveState;
  const s1 = { ...SESSION, id: 'first', title: 'Первая задача' }, s2 = { ...SESSION, id: 'second', title: 'Вторая задача' };
  const ui = await boot({ workspace: { sessionId: 'first' }, getState: () => new Promise(resolve => { resolveState = resolve; }) });
  assert.equal(ui.calls.filter(c => c[0] === 'openChat').length, 0);
  const routed = ui.subs.onWorkspaceRoute({ sessionId: 'second' });
  resolveState([s1, s2]); await routed;
  assert.deepEqual(ui.calls.filter(c => c[0] === 'openChat'), [['openChat', 'second']]);
  assert.equal(ui.doc.getElementById('chatTitle').textContent, 'Вторая задача');
  assert.equal(ui.doc.getElementById('openWorkspace'), null, 'workspace does not advertise another primary workspace');
  await ui.subs.onWorkspaceRoute({});
  assert.equal(ui.doc.documentElement.dataset.view, 'chat', 'reopening the primary window keeps the current conversation');
  assert.equal(ui.calls.filter(c => c[0] === 'openChat').length, 1);
});

test('native window surface wins over shared appearance mode on load and later changes', async () => {
  const quick = await boot({ surface: 'quick', bridge: { getSettings: async () => ({ mode: 'window' }) } });
  assert.equal(quick.doc.documentElement.dataset.mode, 'overlay');
  quick.subs.onAppearance({ mode: 'window' });
  assert.equal(quick.doc.documentElement.dataset.mode, 'overlay');
  const workspace = await boot({ workspace: {}, bridge: { getSettings: async () => ({ mode: 'overlay' }) } });
  assert.equal(workspace.doc.documentElement.dataset.mode, 'window');
  assert.equal(workspace.doc.documentElement.dataset.view, 'list');
  workspace.subs.onAppearance({ mode: 'overlay' });
  assert.equal(workspace.doc.documentElement.dataset.mode, 'window');
});

test('панель поднимается со всеми скриптами и рисует список сессий', async () => {
  const { doc, subs } = await boot({ startAt: 'list', state: [SESSION] });
  assert.ok(doc.getElementById('list'), 'корень списка не найден');
  // Толкаем состояние тем же путём, каким его шлёт демон.
  assert.ok(subs.onState, 'панель не подписалась на состояние');
  subs.onState([SESSION]);
  await new Promise((r) => setTimeout(r, 0));
  const text = doc.getElementById('list').textContent;
  assert.match(text, /jarvis/, 'сессия не появилась в списке: ' + text);
});

test('новые кнопки есть в шапке чата и подключены к своим экранам', async () => {
  const { doc, window } = await boot();
  for (const id of ['changesBtn', 'searchBtn', 'previewBtn']) {
    assert.ok(doc.getElementById(id), `кнопки ${id} нет в разметке`);
  }
  for (const id of ['chgWrap', 'srchWrap', 'chgBody', 'srchBody']) {
    assert.ok(doc.getElementById(id), `панели ${id} нет в разметке`);
  }
  // Скрипты экранов загрузились и объявили себя — иначе клик дал бы
  // ReferenceError уже на живом маке.
  assert.equal(typeof window.JarvisChanges, 'object');
  assert.equal(typeof window.JarvisSearch, 'object');
});

test('клик по «изменениям» открывает панель и спрашивает свод у бэкенда', async () => {
  const { doc, calls, subs } = await boot({
    startAt: 'list', state: [SESSION],
    files: [{ path: 'src/main.rs', state: 'изменён', added: 3, removed: 1, untracked: false }],
  });
  subs.onState([SESSION]);
  await new Promise((r) => setTimeout(r, 0));
  // Открываем чат так же, как человек: кликом по строке списка.
  const row = doc.querySelector('#list .row, #list [data-id], #list > *');
  assert.ok(row, 'строки сессии нет');
  row.dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));

  const btn = doc.getElementById('changesBtn');
  assert.equal(btn.hidden, false, 'кнопка изменений скрыта у сессии с каталогом');
  btn.dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 10));

  assert.equal(doc.getElementById('chgWrap').hidden, false, 'панель изменений не открылась');
  assert.ok(calls.some((c) => c[0] === 'sessionChanges'), 'свод у бэкенда не спрошен: ' + JSON.stringify(calls.map((c) => c[0])));
  assert.match(doc.getElementById('chgBody').textContent, /src\/main\.rs/, 'файл не показан');
});

test('клик по «поиску» открывает панель и ищет по проекту', async () => {
  const { doc, calls, subs } = await boot({
    startAt: 'list', state: [SESSION],
    hits: [{ path: 'src/main.rs', line: 7, text: 'fn искомое() {}' }],
  });
  subs.onState([SESSION]);
  await new Promise((r) => setTimeout(r, 0));
  doc.querySelector('#list > *').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));

  doc.getElementById('searchBtn').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(doc.getElementById('srchWrap').hidden, false, 'панель поиска не открылась');

  const input = doc.querySelector('#srchBody .srch-input');
  assert.ok(input, 'поля поиска нет');
  input.value = 'искомое';
  doc.querySelector('#srchBody .srch-go').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 10));
  assert.ok(calls.some((c) => c[0] === 'sessionSearch' && c[2] === 'искомое'), 'запрос не ушёл');
  assert.match(doc.getElementById('srchBody').textContent, /fn искомое/, 'находка не показана');
});

test('Esc закрывает открытый экран, а не выкидывает из чата', async () => {
  const { doc, subs } = await boot({ startAt: 'list', state: [SESSION] });
  subs.onState([SESSION]);
  await new Promise((r) => setTimeout(r, 0));
  doc.querySelector('#list > *').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));
  doc.getElementById('changesBtn').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 10));

  doc.defaultView.dispatchEvent(key(doc, 'Escape'));
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(doc.getElementById('chgWrap').hidden, true, 'Esc не закрыл панель изменений');
  assert.equal(doc.getElementById('chat').hidden, false, 'Esc заодно выкинул из чата');
});

test('сессия завершается вторым нажатием, а не первым', async () => {
  const { doc, calls, subs } = await boot({ startAt: 'list', state: [SESSION] });
  subs.onState([SESSION]);
  await new Promise((r) => setTimeout(r, 0));

  const open = () => doc.getElementById('actionsBtn').dispatchEvent(click(doc));
  const item = (re) => [...doc.querySelectorAll('#actionsPop .ap-item')].find((r) => re.test(r.textContent));

  open();
  const kill = item(/Завершить сессию/);
  assert.ok(kill, 'пункта «Завершить сессию» нет в меню действий');
  kill.dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(
    !calls.some((c) => c[0] === 'killSession'),
    'первое нажатие уже завершило сессию — подтверждения нет'
  );

  // Второе нажатие — то же самое место, но пункт уже спрашивает.
  open();
  const armed = item(/Точно завершить/);
  assert.ok(armed, 'подтверждение не показано: ' + doc.getElementById('actionsPop').textContent);
  armed.dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));
  const call = calls.find((c) => c[0] === 'killSession');
  assert.ok(call, 'сессия не завершена вторым нажатием');
  assert.equal(call[1], 's1', 'завершили не ту сессию');
});


test('палитра команд работает с русской раскладкой и не теряет экран на Escape', async () => {
  const { doc } = await boot();
  const open = key(doc, 'л'); open.code = 'KeyK'; open.ctrlKey = true; open.metaKey = true;
  doc.defaultView.dispatchEvent(open);
  assert.equal(doc.getElementById('commandDialog').hidden, false);
  const input = doc.getElementById('commandQuery'); input.value = 'встречи';
  input.dispatchEvent(new doc.defaultView.Event('input', { bubbles: true }));
  assert.equal(doc.querySelectorAll('.command-result').length, 1);
  doc.querySelector('.command-result').dispatchEvent(click(doc));
  await new Promise(r => setTimeout(r, 10));
  assert.equal(doc.getElementById('meetings').hidden, false);
  doc.getElementById('workspaceCommands').dispatchEvent(click(doc));
  doc.defaultView.dispatchEvent(key(doc, 'Escape'));
  assert.equal(doc.getElementById('commandDialog').hidden, true);
  assert.equal(doc.getElementById('meetings').hidden, false);
});

test('настройки ищутся по смыслу, а переход с клавиатуры открывает нужный раздел', async () => {
  const { doc } = await boot();
  doc.getElementById('tabSettings').dispatchEvent(click(doc));
  await new Promise(r => setTimeout(r, 10));
  const input = doc.getElementById('settingsSearch'); input.value = 'микрофон';
  input.dispatchEvent(new doc.defaultView.Event('input', { bubbles: true }));
  const shown = [...doc.querySelectorAll('.snav [data-pane]')].filter(n => !n.hidden);
  assert.deepEqual(shown.map(n => n.dataset.pane), ['stt', 'wake']);
  input.dispatchEvent(key(doc, 'Enter'));
  assert.equal(doc.getElementById('s2-pane-stt').classList.contains('on'), true);
  input.value = 'совсем неизвестное'; input.dispatchEvent(new doc.defaultView.Event('input'));
  assert.equal(doc.querySelector('.settings-search-empty').hidden, false);
});

test('главная — список модулей: поиск, Enter и последовательный Escape сохраняют контекст', async () => {
  const { doc } = await boot();
  assert.equal(doc.getElementById('launcher').hidden, false);
  assert.equal(doc.getElementById('list').hidden, true);
  const query = doc.getElementById('query'); query.value = 'встречи';
  query.dispatchEvent(new doc.defaultView.Event('input', { bubbles: true }));
  assert.equal(doc.getElementById('tabMeetings').hidden, false);
  assert.equal(doc.getElementById('tabHistory').hidden, true);
  doc.defaultView.dispatchEvent(key(doc, 'Enter'));
  await new Promise(r => setTimeout(r, 0));
  assert.equal(doc.getElementById('meetings').hidden, false);
  doc.getElementById('pageCommands').dispatchEvent(click(doc));
  const search = doc.getElementById('commandQuery'); search.value = 'настройки';
  search.dispatchEvent(new doc.defaultView.Event('input', { bubbles: true }));
  doc.querySelector('.command-result').dispatchEvent(click(doc));
  await new Promise(r => setTimeout(r, 0));
  assert.equal(doc.getElementById('settings').hidden, false);
  doc.defaultView.dispatchEvent(key(doc, 'Escape'));
  assert.equal(doc.getElementById('meetings').hidden, false);
  doc.defaultView.dispatchEvent(key(doc, 'Escape'));
  assert.equal(doc.getElementById('launcher').hidden, false);
  assert.equal(query.value, 'встречи');
  doc.defaultView.dispatchEvent(key(doc, 'Escape'));
  assert.equal(query.value, '');
});

test('задержка списка микрофонов не скрывает настройки и не блокирует навигацию', async () => {
  let finishDevices;
  const { doc, calls } = await boot({ sttInputDevices: () => new Promise(resolve => { finishDevices = resolve; }) });
  doc.getElementById('tabSettings').dispatchEvent(click(doc));
  await new Promise(r => setTimeout(r, 0));
  doc.querySelector('.snav [data-pane="stt"]').dispatchEvent(click(doc));
  await new Promise(r => setTimeout(r, 0));
  const pane = doc.getElementById('s2-pane-stt');
  assert.match(pane.textContent, /Движок распознавания/);
  assert.match(pane.textContent, /Получаю список устройств/);
  doc.querySelector('.snav [data-pane="general"]').dispatchEvent(click(doc));
  assert.equal(doc.getElementById('s2-pane-general').classList.contains('on'), true);
  finishDevices({ devices: [], current: 'USB Mic', error: 'Устройство не отвечает' });
  await new Promise(r => setTimeout(r, 0));
  assert.match(pane.textContent, /Устройство не отвечает/);
  assert.match(pane.textContent, /USB Mic/);
  assert.equal(doc.getElementById('s2-pane-general').classList.contains('on'), true);
  assert.ok(!calls.some(c => ['micTest', 'meetingsStart', 'sttSetInputDevice'].includes(c[0])));
});

test('ошибка списка микрофонов оставляет другие настройки доступными', async () => {
  const { doc } = await boot({ sttInputDevices: async () => { throw new Error('driver unavailable'); } });
  doc.getElementById('tabSettings').dispatchEvent(click(doc));
  await new Promise(r => setTimeout(r, 0));
  doc.querySelector('.snav [data-pane="stt"]').dispatchEvent(click(doc));
  await new Promise(r => setTimeout(r, 0));
  const pane = doc.getElementById('s2-pane-stt');
  assert.match(pane.textContent, /Не удалось получить список микрофонов/);
  assert.match(pane.textContent, /Системный по умолчанию/);
  assert.match(pane.textContent, /Шумодав/);
});

test('вход во Встречи не начинает запись и не отправляет текст агенту', async () => {
  const { doc, calls } = await boot();
  doc.getElementById('tabMeetings').dispatchEvent(click(doc));
  await new Promise(r => setTimeout(r, 10));
  assert.equal(doc.getElementById('meetings').hidden, false);
  assert.ok(calls.some(c => c[0] === 'meetingsList'));
  assert.ok(!calls.some(c => ['meetingsStart', 'sendReply', 'launchSession'].includes(c[0])));
  assert.match(doc.getElementById('meetings').textContent, /микрофон/i);
});

test('Enter на сфокусированной строке открывает ровно её, один раз', async () => {
  const { doc, calls, subs } = await boot({ startAt: 'list', state: [SESSION, { ...SESSION, id: 's2', project: 'second' }] });
  subs.onState([SESSION, { ...SESSION, id: 's2', project: 'second' }]);
  await new Promise(r => setTimeout(r, 0));
  const rows = doc.querySelectorAll('#list .row');
  calls.length = 0;
  rows[1].dispatchEvent(key(doc, 'Enter'));
  await new Promise(r => setTimeout(r, 5));
  const opens = calls.filter(c => c[0] === 'openChat');
  assert.equal(opens.length, 1);
  assert.equal(opens[0][1], 's2');
});


test('поздний начальный снимок не затирает уже полученное живое состояние', async () => {
  let finish;
  const { doc, subs } = await boot({ startAt: 'list', getState: () => new Promise(resolve => { finish = resolve; }) });
  subs.onState([SESSION]);
  finish([]);
  await new Promise(r => setTimeout(r, 5));
  assert.match(doc.getElementById('list').textContent, /jarvis/);
});

test('запоздалое открытие чата не подменяет новый выбранный чат', async () => {
  const pending = new Map();
  const { doc } = await boot({ startAt: 'list', state: [SESSION, { ...SESSION, id: 's2', project: 'second' }], openChat: id => new Promise(resolve => pending.set(id, resolve)) });
  doc.querySelectorAll('#list .row')[0].dispatchEvent(click(doc));
  doc.querySelectorAll('#list .row')[1].dispatchEvent(click(doc));
  const result = project => ({ ok: true, project, items: [], spans: [], cards: {} });
  pending.get('s2')(result('second'));
  await new Promise(r => setTimeout(r, 5));
  pending.get('s1')(result('jarvis'));
  await new Promise(r => setTimeout(r, 5));
  assert.equal(doc.getElementById('chatTitle').textContent, 'second');
});

test('переход в настройки отменяет ещё не открывшийся удалённый чат', async () => {
  let finish;
  const { doc, calls } = await boot({ startAt: 'list', state: [SESSION], openChat: () => new Promise(resolve => { finish = resolve; }) });
  doc.querySelector('#list .row').dispatchEvent(click(doc));
  doc.getElementById('tabSettings').dispatchEvent(click(doc));
  finish({ ok: true, items: [], spans: [], cards: {} });
  await new Promise(r => setTimeout(r, 10));
  assert.ok(calls.some(c => c[0] === 'closeChat'));
  assert.equal(doc.getElementById('settings').hidden, false);
  assert.equal(doc.getElementById('chat').hidden, true);
});

test('settings rejects optimistic toggle and select values when backend refuses the change', async () => {
  const { doc } = await boot({ bridge: {
    getSettings: async () => ({ diagnostics: false }),
    setSettings: async () => ({ ok: false, error: 'диск недоступен' }),
    sttGet: async () => ({ engine: 'whisper-turbo', engines: ['whisper-turbo', 'qwen3-0.6b'] }),
    sttSetEngine: async () => ({ ok: false, error: 'Сначала установите модель' }),
  } });
  doc.getElementById('tabSettings').click();
  await new Promise(r => setTimeout(r, 0));
  const diagnostics = [...doc.querySelectorAll('#s2-pane-general .drow')].find(r => r.textContent.includes('Режим логов')).querySelector('input');
  diagnostics.checked = true;
  diagnostics.dispatchEvent(new doc.defaultView.Event('change', { bubbles: true }));
  await new Promise(r => setTimeout(r, 0));
  assert.equal(diagnostics.checked, false);
  assert.equal(diagnostics.disabled, false);
  assert.match(doc.getElementById('settings-save-error').textContent, /диск недоступен/);
  assert.ok(diagnostics.getAttribute('aria-labelledby'));

  doc.querySelector('[data-pane="stt"]').click();
  await new Promise(r => setTimeout(r, 0));
  const select = doc.querySelector('#s2-pane-stt .cselect');
  select.querySelector('.cstrigger').click();
  assert.equal(select.querySelector('.cstrigger').getAttribute('aria-expanded'), 'true');
  select.querySelector('[data-value="qwen3-0.6b"]').click();
  await new Promise(r => setTimeout(r, 0));
  assert.equal(select.querySelector('.cval').textContent, 'whisper-turbo');
  assert.equal(select.classList.contains('busy'), false);
  assert.match(doc.getElementById('settings-save-error').textContent, /Сначала установите модель/);
  assert.equal(doc.documentElement.dataset.view, 'settings');
  doc.querySelector('[data-pane="about"]').click();
  await new Promise(r => setTimeout(r, 0));
  assert.equal(doc.getElementById('settings-save-error'), null, 'an STT error must not leak into About');
  doc.querySelector('[data-pane="stt"]').click();
  assert.match(doc.getElementById('settings-save-error').textContent, /Движок распознавания.*Сначала установите модель/);
});

test('settings search finds parameters in unopened panes and opens the matching row', async () => {
  const { doc, window } = await boot({ bridge: {
    serviceGet: async () => ({ backend: 'codex', codexSidecar: true }),
    claudeAuthGet: async () => ({ connected: false }),
  } });
  doc.getElementById('tabSettings').click(); await new Promise(r => setTimeout(r, 0));
  const search = doc.getElementById('settingsSearch');
  search.value = 'автозапуск'; search.dispatchEvent(new doc.defaultView.Event('input', { bubbles: true }));
  assert.match(doc.querySelector('.settings-results').textContent, /Запускать при старте/);
  doc.querySelector('.settings-result').click(); await new Promise(r => setTimeout(r, 0));
  assert.equal(search.value, '');
  assert.equal(doc.querySelector('#s2-pane-general [tabindex="-1"] .dt').textContent, 'Запускать при старте');
  search.value = 'прокси'; search.dispatchEvent(new doc.defaultView.Event('input', { bubbles: true }));
  [...doc.querySelectorAll('.settings-result')].find(b => b.textContent.includes('Egress-прокси')).click();
  await new Promise(r => setTimeout(r, 0));
  assert.equal(doc.querySelector('#s2-pane-service [tabindex="-1"] .dt').textContent, 'Egress-прокси');
  search.value = 'масштаб'; search.dispatchEvent(new doc.defaultView.Event('input', { bubbles: true }));
  window.dispatchEvent(key(doc, 'Escape'));
  assert.equal(search.value, '');
  assert.equal(doc.documentElement.dataset.view, 'settings');
});

test('a delayed settings failure stays attached to the originating pane', async () => {
  let rejectSave;
  const { doc } = await boot({ bridge: { setSettings: () => new Promise((resolve, reject) => { rejectSave = reject; }) } });
  doc.getElementById('tabSettings').click(); await new Promise(r => setTimeout(r, 0));
  const input = [...doc.querySelectorAll('#s2-pane-general .drow')].find(r => r.textContent.includes('Режим логов')).querySelector('input');
  input.checked = true; input.dispatchEvent(new doc.defaultView.Event('change', { bubbles: true }));
  doc.querySelector('[data-pane="about"]').click();
  rejectSave(new Error('Не удалось записать файл')); await new Promise(r => setTimeout(r, 0));
  assert.equal(doc.getElementById('settings-save-error'), null);
  doc.querySelector('[data-pane="general"]').click();
  assert.match(doc.getElementById('settings-save-error').textContent, /Режим логов.*Не удалось записать файл/);
  assert.equal(input.checked, false);
});

test('Escape dismisses a settings picker before navigating back', async () => {
  const { doc, window } = await boot();
  doc.getElementById('tabSettings').click();
  await new Promise(r => setTimeout(r, 0));
  doc.querySelector('[data-pane="stt"]').click();
  await new Promise(r => setTimeout(r, 0));
  const trigger = doc.querySelector('#s2-pane-stt .cstrigger');
  trigger.click();
  window.dispatchEvent(key(doc, 'Escape'));
  assert.equal(trigger.getAttribute('aria-expanded'), 'false');
  assert.equal(doc.documentElement.dataset.view, 'settings');
  window.dispatchEvent(key(doc, 'Escape'));
  assert.equal(doc.documentElement.dataset.view, 'home');
});

test('failed settings read shows a retry instead of editable invented defaults', async () => {
  let fail = true;
  const { doc } = await boot({ bridge: { getSettings: async () => {
    if (fail) throw new Error('Нет связи');
    return { diagnostics: true };
  } } });
  doc.getElementById('tabSettings').click();
  await new Promise(r => setTimeout(r, 0));
  const pane = doc.getElementById('s2-pane-general');
  assert.equal(pane.querySelector('input'), null);
  assert.equal(pane.querySelector('.skel'), null);
  assert.match(pane.textContent, /Не удалось загрузить раздел:.*Нет связи/);
  fail = false;
  [...pane.querySelectorAll('button')].find(b => b.textContent === 'Повторить загрузку').click();
  await new Promise(r => setTimeout(r, 0));
  assert.equal(pane.querySelector('[role="alert"]'), null);
  assert.match(pane.textContent, /Режим логов/);
});

test('appearance changes retain settings input identity and save failures are visible', async () => {
  const { doc, window } = await boot({ bridge: {
    getSettings: async () => ({ theme: 'dark', mode: 'window' }),
    setSettings: async () => { throw new Error('Хранилище недоступно'); },
  } });
  doc.getElementById('tabSettings').click(); await new Promise(r => setTimeout(r, 0));
  doc.querySelector('[data-pane="look"]').click(); await new Promise(r => setTimeout(r, 0));
  const search = doc.getElementById('settingsSearch');
  search.value = 'не потерять';
  window.jarvisTheme.adopt({ theme: 'light' });
  await new Promise(r => setTimeout(r, 0));
  assert.equal(doc.getElementById('settingsSearch'), search);
  assert.equal(search.value, 'не потерять');
  assert.equal(await window.jarvisTheme.set({ theme: 'light' }), false);
  assert.equal(window.jarvisTheme.get().theme, 'dark');
  assert.match(doc.getElementById('settings-save-error').textContent, /Хранилище недоступно/);
});

test('usage read failure has a working retry', async () => {
  let fail = true;
  const { doc } = await boot({ bridge: { getUsage: async () => {
    if (fail) throw new Error('Нет данных');
    return { total:{tok:4200,api:0,plan:0}, window:{resetInMs:0}, series:[], byModel:[], byProject:[], sessions:[], byBilling:[] };
  } } });
  doc.getElementById('tabStats').click(); await new Promise(r => setTimeout(r, 0));
  assert.match(doc.getElementById('stats').textContent, /Аналитика ИИ/);
  [...doc.querySelectorAll('#stats button')].find(button => button.textContent === 'Расход и лимиты').click(); await new Promise(r => setTimeout(r, 0));
  assert.match(doc.getElementById('stats').textContent, /Не удалось загрузить использование/);
  fail = false;
  doc.querySelector('#stats button').click(); await new Promise(r => setTimeout(r, 0));
  assert.equal(doc.querySelector('#stats [role="alert"]'), null);
  assert.match(doc.getElementById('stats').textContent, /Сегодня/);
});

test('project Escape restores its previous search', async () => {
  const history = [{project:'Jarvis',cwd:'/qa/jarvis',count:1,lastAt:1,sessions:[{id:'h1',agent:'codex',title:'Проверить UI',lastAt:1}]}];
  const { doc, window } = await boot({ bridge: { getHistory: async () => history } });
  doc.getElementById('tabHistory').click(); await new Promise(r => setTimeout(r, 0));
  const query = doc.getElementById('query'); query.value = 'Jarv';
  query.dispatchEvent(new doc.defaultView.Event('input', { bubbles: true })); await new Promise(r => setTimeout(r, 0));
  [...doc.querySelectorAll('#history .hrow')].find(row => row.title === '/qa/jarvis').click();
  await new Promise(r => setTimeout(r, 0));
  assert.equal(query.value, '');
  assert.match(doc.getElementById('history').textContent, /Проверить UI/);
  window.dispatchEvent(key(doc, 'Escape')); await new Promise(r => setTimeout(r, 0));
  assert.equal(query.value, 'Jarv');
  assert.equal(doc.documentElement.dataset.view, 'history');
  assert.match(doc.getElementById('history').textContent, /Новый проект/);
});

test('failed project launch keeps path and task, repeated clicks do not launch twice', async () => {
  let finish;
  const { doc, calls } = await boot({ bridge: {
    getHistory: async () => [], launchSession: () => new Promise(resolve => { finish = resolve; }),
  } });
  doc.getElementById('tabHistory').click(); await new Promise(r => setTimeout(r, 0));
  doc.querySelector('#history .hrow').click(); await new Promise(r => setTimeout(r, 0));
  const input = doc.querySelector('.hnewform > input'); input.value = '/qa/unfinished';
  input.dispatchEvent(new doc.defaultView.Event('input', { bubbles: true }));
  const task = doc.querySelector('.taskinput'); task.value = 'Не потерять задачу';
  task.dispatchEvent(new doc.defaultView.Event('input', { bubbles: true }));
  const button = [...doc.querySelectorAll('.hnewform button')].find(b => b.textContent === 'Codex');
  button.click(); button.click();
  assert.equal(calls.filter(c => c[0] === 'launchSession').length, 1);
  finish({ok:false,error:'Сервер недоступен'}); await new Promise(r => setTimeout(r, 0));
  assert.equal(doc.querySelector('.hnewform > input').value, '/qa/unfinished');
  assert.equal(doc.querySelector('.taskinput').value, 'Не потерять задачу');
  assert.equal(button.disabled, false);
  assert.match(doc.querySelector('.toast').textContent, /Сервер недоступен/);
});

test('opening a slow chat changes the screen immediately and ignores its late response after navigation', async () => {
  let resolve;
  const other = { ...SESSION, id: 's2', title: 'Второй чат' };
  const { doc, subs } = await boot({ startAt: 'list', state: [SESSION, other], openChat: id => id === 's1' ? new Promise(r => { resolve = r; }) : Promise.resolve({ ok: true, items: [{ role: 'assistant', text: 'История второго' }], spans: [] }) });
  subs.onState([SESSION, other]);
  const rows = [...doc.querySelectorAll('#list > *')];
  rows.find(row => row.dataset.id === 's1')?.dispatchEvent(click(doc));
  if (!resolve) rows[0].dispatchEvent(click(doc));
  assert.equal(doc.getElementById('chat').hidden, false);
  assert.match(doc.getElementById('chatlog').textContent, /Загружаем историю/);
  assert.ok(doc.querySelector('#chatlog .ui-skeleton-history'));
  assert.equal(doc.getElementById('chatlog').getAttribute('aria-busy'), 'true');
  doc.getElementById('tabSessions').dispatchEvent(click(doc));
  resolve({ ok: true, items: [{ role: 'assistant', text: 'Поздний ответ' }], spans: [] });
  await new Promise(r => setTimeout(r, 0));
  assert.equal(doc.getElementById('chat').hidden, true);
});

test('send shows pending message before IPC resolves and restores draft on failure', async () => {
  let resolve;
  const { doc, subs } = await boot({ startAt: 'list', state: [SESSION], bridge: { sendReply: () => new Promise(r => { resolve = r; }) } });
  subs.onState([SESSION]); doc.querySelector('#list > *').dispatchEvent(click(doc));
  await new Promise(r => setTimeout(r, 0));
  const reply = doc.getElementById('reply'); reply.value = 'Проверь файл'; reply.dispatchEvent(key(doc, 'Enter'));
  assert.equal(reply.value, '');
  assert.match(doc.getElementById('chatlog').textContent, /Проверь файл/);
  assert.match(doc.getElementById('chatlog').textContent, /Отправляем/);
  await new Promise(r => setTimeout(r, 0)); resolve({ ok: false, error: 'Нет связи' });
  await new Promise(r => setTimeout(r, 0));
  assert.equal(reply.value, 'Проверь файл');
  assert.equal(doc.querySelectorAll('.msg.user.pending').length, 0);
});

test('Codex progress stays collapsed and later runtime events preserve the Markdown answer', async () => {
  const items = [
    { role: 'user', kind: 'text', text: 'Проверь проект', ts: 1 },
    { role: 'assistant', kind: 'tool', text: 'exec · npm test', ts: 2 },
    { role: 'assistant', kind: 'tool', text: 'wait', ts: 3 },
    { role: 'assistant', kind: 'progress', text: 'Проверяю сборку', ts: 4 },
    { role: 'assistant', kind: 'text', text: '**Готово** [файл](https://example.com/file)', ts: 5 },
  ];
  const { doc, subs } = await boot({ startAt: 'list', state: [{ ...SESSION, agent: 'codex' }], openChat: async () => ({ ok: true, items, spans: [], cards: {} }) });
  doc.querySelector('#list .row').dispatchEvent(click(doc));
  await new Promise(resolve => setTimeout(resolve, 0));
  const log = doc.getElementById('chatlog');
  const disclosure = log.querySelector('.tool-disclosure');
  assert.ok(disclosure);
  assert.equal(disclosure.hasAttribute('open'), false);
  assert.match(disclosure.textContent, /Проверяю сборку/);
  assert.equal(log.querySelectorAll('.msg.assistant').length, 1);
  assert.match(log.querySelector('.msg.assistant').textContent, /Готово/);
  assert.equal(log.querySelector('.msg.assistant a').getAttribute('data-href'), 'https://example.com/file');
  subs.onChatAppend({ sessionId: 's1', items: [{ role: 'assistant', kind: 'new-runtime-event', text: 'raw args' }] });
  assert.equal(log.querySelectorAll('.msg.assistant').length, 1);
  assert.doesNotMatch(log.textContent, /raw args/);
});

test('chat Markdown opens complete HTTP URLs with balanced parentheses and leaves other schemes inert', async () => {
  const url = 'https://en.wikipedia.org/wiki/Function_(mathematics)';
  const items = [{ role: 'assistant', kind: 'text', text: `[API](${url}) [local](jarvis://settings)`, ts: 1 }];
  const { doc, calls } = await boot({ startAt: 'list', state: [SESSION], openChat: async () => ({ ok: true, items, spans: [], cards: {} }) });
  doc.querySelector('#list .row').dispatchEvent(click(doc));
  await new Promise(resolve => setTimeout(resolve, 0));
  const link = doc.querySelector('#chatlog .msg.assistant a');
  assert.equal(link?.getAttribute('data-href'), url);
  link.dispatchEvent(click(doc));
  link.dispatchEvent(key(doc, 'Enter'));
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.deepEqual(calls.filter(call => call[0] === 'openUrl'), [
    ['openUrl', url],
    ['openUrl', url],
  ]);
  assert.match(doc.querySelector('#chatlog .msg.assistant').textContent, /local/);
  assert.equal(doc.querySelectorAll('#chatlog .msg.assistant a').length, 1);
});
