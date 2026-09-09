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
  'theme.js', 'icons.js', 'navigation.js', 'workspace.js', 'async-state.js', 'attachments.js', 'meetings.js', 'ai-analytics.js',
  // подписи клавиш: renderer зовёт window.jarvisKeys уже при первой отрисовке
  'keys.js',
  'markdown.js',
  'diffview.js',
  // каталог агентов: renderer и settings2 спрашивают его уже на первой отрисовке
  'agents.js',
  'question-answer.js',
  'settings2.js',
  'voice-history.js',
  'loops.js',
  'bundle.js',
  'changes.js',
  'search.js',
  // чат с главным агентом: вкладка «Джарвис» зовёт его initAgentChat
  'agent-chat.js',
  'renderer.js',
];

/* Мост-заглушка. Любой неизвестный метод возвращает разумную пустоту, иначе
 * тест превратился бы в список из сотни моков и ломался бы от каждой新ой
 * команды. Вызовы записываем — по ним и проверяем, что кнопки делают дело. */
function makeBridge(calls, data = {}) {
  const subs = {};
  // Книжка чатов главного агента: ответ у всех команд списка один и тот же —
  // весь список с пометкой открытого.
  let chats = (data.agentChats || [{ id: 'c1', name: 'Чат 1', sessionId: data.agentSession || null }]).slice();
  let current = data.agentCurrent || (chats[0] || {}).id || null;
  /* Список сшивается с диском, как у демона: сперва чаты из настроек, потом
   * разговоры, найденные в транскриптах и ни к кому не привязанные (id: null).
   * Именно этот хвост и делает удалённый чат достижимым. */
  /* Спрятанные и забытые — как у демона: скрытая строка выпадает из списка, но
   * файл на диске цел (его считает `hidden`), а забытая исчезает вместе с ним. */
  const hidden = new Set(data.agentHidden || []);
  const gone = new Set();
  const threads = () =>
    (data.agentThreads || []).filter((t) => !gone.has(t.sessionId) && !chats.some((c) => c.sessionId === t.sessionId));
  const book = (next, edit) => {
    if (edit) chats = edit(chats);
    if (next) current = next;
    if (!chats.some((c) => c.id === current)) current = (chats[0] || {}).id || null;
    return {
      ok: true,
      current,
      chats: chats.map((c) => ({ ...c, current: c.id === current })).concat(threads().filter((t) => !hidden.has(t.sessionId))),
      hidden: threads().filter((t) => hidden.has(t.sessionId)).length,
    };
  };
  const target = {
    getState: async () => data.state || [],
    // settings.json: тут же живут ширина и свёрнутость колонки чатов
    getSettings: async () => ({ mode: 'overlay', ...(data.settings || {}) }),
    getMeta: async () => ({ version: 'test' }),
    getLimit: async () => null,
    sessionChanges: async () => ({ ok: true, branch: 'dev', files: data.files || [] }),
    sessionChangeDiff: async () => ({ ok: true, hunks: [] }),
    sessionSearch: async () => ({ ok: true, hits: data.hits || [], capped: false }),
    // Открытие чата отдаёт ленту ходов — панель сразу читает items/spans.
    openChat: async () => ({ ok: true, items: [], spans: [], cards: {} }),
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
    // Главный агент: список разговоров, нить и прошлая переписка. Книжку чатов
    // держим живой — команды правят её, как это делает демон, и окно видит
    // ровно тот список, который получилось бы на настоящем бэкенде.
    agentChatState: async () => ({ sessionId: data.agentSession || null }),
    agentChatsList: async () => book(),
    agentChatSwitch: async (chatId) => data.agentSwitch || book(chatId),
    agentChatRename: async (chatId, name) => data.agentRename || book(null, (list) => list.map((c) => (c.id === chatId ? { ...c, name } : c))),
    agentChatDelete: async (chatId) => data.agentDelete || book(null, (list) => list.filter((c) => c.id !== chatId)),
    agentChatCreate: async (name) => data.agentCreate || book('c9', (list) => list.concat({ id: 'c9', name: name || 'Чат 9', sessionId: null })),
    // Привязать разговор с диска: он уходит из хвоста и становится обычным чатом
    agentChatOpen: async (sessionId) =>
      data.agentOpen ||
      book('c9', (list) =>
        list.concat({ ...(data.agentThreads || []).find((t) => t.sessionId === sessionId), id: 'c9' })),
    // Спрятать строку с диска и вернуть все спрятанные: файла обе не касаются
    agentHistoryHide: async (sessionId) => data.agentHide || (hidden.add(sessionId), book()),
    agentHistoryUnhideAll: async () => { hidden.clear(); return book(); },
    // Забыть насовсем: транскрипт исчезает, и пометка «скрыт» вместе с ним
    agentHistoryForget: async (sessionId) => {
      if (data.agentForget) return data.agentForget;
      gone.add(sessionId);
      hidden.delete(sessionId);
      return book();
    },
    // Историю иногда нужно подержать в пути — тогда тест даёт функцию.
    agentChatHistory: async (chatId) =>
      (typeof data.agentHistory === 'function' ? data.agentHistory(chatId) : data.agentHistory) || { ok: true, items: [] },
    // своё имя чата: по умолчанию удачно, тест на отказ подсовывает свой ответ
    renameSession: async (id, title) => data.rename || { ok: true, name: title || null, title },
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
  // linkedom не знает scrollIntoView, а панель зовёт его при выборе строки —
  // без заглушки раздел падает и «белый экран» в тесте не отличить от беды
  if (!window.Element.prototype.scrollIntoView) window.Element.prototype.scrollIntoView = () => {};
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
      'requestAnimationFrame', 'cancelAnimationFrame', 'CustomEvent',
      read(name)
    );
    fn(
      window, document, window, window.localStorage, window.navigator || { platform: 'MacIntel' },
      // Периодику панели глушим: она нужна живому окну, а тест иначе никогда
      // не закончится — процесс держат её таймеры.
      setTimeout, clearTimeout, () => 0, () => {},
      window.requestAnimationFrame, window.cancelAnimationFrame,
      // Событие берём у linkedom, а не у Node: чужой Event окно не принимает, и
      // «внешность поменялась» (смена режима, темы) не доезжала бы вовсе —
      // молча, потому что theme.js шлёт его из промиса.
      window.CustomEvent
    );
  }
  // Дать промисам старта (getState/getSettings/getMeta) отработать.
  await new Promise((r) => setTimeout(r, 0));
  document.getElementById('tabSessions').dispatchEvent(new window.Event('click', { bubbles: true }));
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

test('панель поднимается со всеми скриптами и рисует список сессий', async () => {
  const { doc, subs } = await boot({ state: [SESSION] });
  assert.ok(doc.getElementById('list'), 'корень списка не найден');
  // Толкаем состояние тем же путём, каким его шлёт демон.
  assert.ok(subs.onState, 'панель не подписалась на состояние');
  subs.onState([SESSION]);
  await new Promise((r) => setTimeout(r, 0));
  const text = doc.getElementById('list').textContent;
  assert.match(text, /jarvis/, 'сессия не появилась в списке: ' + text);
});

/* Третий агент — такой же житель списка: бейдж с его именем, модель короткой
 * подписью из каталога, а не сырым id вроде «kimi-code/k3-256k». */
test('сессия третьего агента рисуется бейджами, а не многоточием', async () => {
  const kimi = { ...SESSION, id: 's2', agent: 'kimi', model: 'kimi-code/k3-256k' };
  const { doc, subs } = await boot({ state: [kimi] });
  subs.onState([kimi]);
  await new Promise((r) => setTimeout(r, 0));
  const badges = [...doc.querySelectorAll('#list .badge')].map((b) => b.textContent);
  assert.ok(badges.includes('kimi'), 'бейджа агента нет: ' + badges.join(' | '));
  assert.ok(badges.includes('K3-256k'), 'модель показана сырым id: ' + badges.join(' | '));
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
    state: [SESSION],
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
    state: [SESSION],
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
  const { doc, subs } = await boot({ state: [SESSION] });
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

/* ---------- вкладка «Джарвис»: разговор с главным агентом ----------
 *
 * Ради этого вкладка и заведена: раньше агент жил отдельным окном, и открывалось
 * оно с пустой лентой — даже когда разговор продолжался. Пустота молча и есть
 * то, что ловят эти тесты. */

// открыть вкладку так же, как человек: кликом по ней
async function openAgent(doc) {
  doc.getElementById('tabAgent').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));
  return doc.getElementById('agLog');
}

test('вкладка «Джарвис» есть в панели и переключает на неё', async () => {
  const { doc } = await boot();
  const tab = doc.getElementById('tabAgent');
  assert.ok(tab, 'вкладки нет в разметке');
  assert.equal(doc.getElementById('agentPane').hidden, true, 'пане показан до клика');

  await openAgent(doc);
  assert.equal(doc.getElementById('agentPane').hidden, false, 'вкладка не открылась');
  assert.ok(tab.classList.contains('active'), 'вкладка не подсвечена');
  assert.equal(doc.getElementById('list').hidden, true, 'список сессий не уступил место');
  // соседние вкладки цифры не потеряли
  const keys = [...doc.querySelectorAll('.tabs .tabkey')].map((k) => k.getAttribute('data-key'));
  assert.deepEqual(keys, ['9', '1', '2', '8', '4', '7', '5', '6', '3', ','], 'номера вкладок разъехались: ' + keys.join(','));
});

/* Порядок вкладок: «Джарвис» первым, остальные сдвинуты на единицу. Номера
 * зашиты в трёх местах (разметка, обработчик хоткеев, меню действий) — тест
 * держит их вместе, иначе ⌘3 молча откроет не то, что подписано. */
test('«Джарвис» — первая вкладка, остальные сдвинулись на единицу', async () => {
  const { doc } = await boot();
  const ids = [...doc.querySelectorAll('.tabs .tab')].map((t) => t.id).filter(id => id !== 'openWorkspace');
  assert.equal(ids[0], 'tabAgent', 'Джарвис не первый: ' + ids.join(','));
  assert.deepEqual(
    ids.slice(0, 7),
    ['tabAgent', 'tabSessions', 'tabHistory', 'tabMachines', 'tabVoice', 'tabMeetings', 'tabLoops'],
    'порядок вкладок не тот: ' + ids.join(','),
  );
  // цифра рядом с названием — та же, что откроет вкладку
  const num = (id) => doc.getElementById(id).querySelector('.tabkey').getAttribute('data-key');
  assert.equal(num('tabAgent'), '9');
  assert.equal(num('tabSessions'), '1');
  assert.equal(num('tabBundle'), '6');
  // подсветка на старте осталась у списка сессий: порядок вкладок не меняет
  // того, чем панель открывается
  assert.equal(doc.getElementById('tabSessions').classList.contains('active'), true);
  assert.equal(doc.getElementById('tabAgent').classList.contains('active'), false);
});

/* Хоткей каждой вкладки против её цифры в разметке — на живой панели. Здесь
 * только те экраны, что поднимаются на пустой заглушке; «Статистика», «Циклы»
 * и «Связка» ждут данных демона — за них отвечает соседний тест по исходникам. */
test('цифры вкладок не декорация: ⌘N открывает ровно ту, что подписана', async () => {
  const PANES = {
    tabAgent: 'agentPane',
    tabHistory: 'history',
    tabVoice: 'voicehist',
  };
  // Копим строками и сравниваем строки: неудачный assert над узлом linkedom
  // печатается вечность и выглядит зависанием, а не провалом.
  const bad = [];
  for (const [tabId, paneId] of Object.entries(PANES)) {
    const { doc } = await boot();
    const digit = doc.getElementById(tabId).querySelector('.tabkey').getAttribute('data-key');
    const e = key(doc, digit);
    e.metaKey = true;
    e.ctrlKey = true; // isMod смотрит на ⌘ или Ctrl — зависит от ОС
    doc.defaultView.dispatchEvent(e);
    await new Promise((r) => setTimeout(r, 0));
    if (doc.getElementById(paneId).hidden !== false) bad.push(`⌘${digit} не открыл ${tabId}`);
    if (!doc.getElementById(tabId).classList.contains('active')) bad.push(`⌘${digit}: ${tabId} не подсвечена`);
  }
  assert.deepEqual(bad, []);
  // ⌘2 — список сессий: у него не панель, а сам список
  const { doc } = await boot();
  doc.getElementById('tabAgent').dispatchEvent(click(doc));
  const e = key(doc, '1');
  e.metaKey = true;
  e.ctrlKey = true;
  doc.defaultView.dispatchEvent(e);
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(doc.getElementById('list').hidden, false, '⌘2 не вернул к списку чатов');
  assert.equal(doc.getElementById('agentPane').hidden, true, '⌘2 оставил Джарвиса на экране');
});

/* Порядок вкладок в разметке против веток обработчика — по исходникам, а не по
 * кликам: так проверяется ВСЯ семёрка разом, включая те вкладки, которым в
 * тесте нужны данные демона. Разъедутся — ⌘N откроет соседа. */
test('каждой вкладке по разметке отвечает та же цифра в обработчике', () => {
  const html = read('index.html');
  const renderer = read('renderer.js');
  const VIEW = {
    tabAgent: 'agent', tabSessions: 'list', tabHistory: 'history', tabStats: 'stats',
    tabVoice: 'voicehist', tabLoops: 'loops', tabBundle: 'bundle', tabMachines: 'machines', tabMeetings: 'meetings',
  };
  // разметка: id вкладки → её цифра, в порядке следования
  const inMarkup = [...html.matchAll(/id="(tab\w+)">[^<]*<span class="tabkey" data-key="(\d)"/g)]
    .map(([, id, digit]) => `${digit}=${VIEW[id] || id}`);
  // обработчик: цифра → вид, который она открывает
  const inCode = [...renderer.matchAll(/e\.key === '(\d)'\)[^]*?setView\('(\w+)'\)/g)]
    .map(([, digit, view]) => `${digit}=${view}`);
  assert.deepEqual(inCode.filter(x => x !== '0=home').sort(), [...inMarkup, '7=meetings'].sort());
});

// Меню действий (⌘K) подписывает те же цифры — третье место, где номера зашиты.
test('меню действий подписывает вкладки теми же цифрами', async () => {
  const { doc } = await boot();
  doc.getElementById('actionsBtn').dispatchEvent(click(doc));
  const rows = [...doc.querySelectorAll('#actionsPop .ap-item')].map((r) => r.textContent);
  const hint = (re) => rows.find((t) => re.test(t)) || '';
  assert.match(hint(/Разговор с Джарвисом/), /9/, 'Джарвиса нет в меню или цифра не та: ' + rows.join(' | '));
  assert.match(hint(/Проекты и история/), /2/);
  assert.match(hint(/Аналитика ИИ/), /3/);
  assert.match(hint(/История голоса/), /4/);
});

test('прошлая переписка рисуется в ленте, а не остаётся у агента в памяти', async () => {
  const { doc } = await boot({
    agentSession: 's-42',
    agentHistory: {
      ok: true,
      sessionId: 's-42',
      total: 3,
      items: [
        { role: 'user', kind: 'text', text: 'сколько сессий висит', ts: 1 },
        { role: 'assistant', kind: 'tool', text: 'state_get', ts: 2 },
        { role: 'assistant', kind: 'text', text: 'три, одна ждёт ответа', ts: 3 },
      ],
    },
  });
  const log = await openAgent(doc);

  assert.match(log.textContent, /сколько сессий висит/, 'реплики человека нет в ленте');
  assert.match(log.textContent, /три, одна ждёт ответа/, 'ответа агента нет в ленте');
  assert.ok(log.querySelector('.msg.user .bubble'), 'реплика человека не отличима от ответа');
  assert.ok(log.querySelector('.msg.assistant .bubble'), 'ответ агента нарисован не как реплика');
  // тул-вызов — чипом чата сессии, а не обычным текстом
  const chip = log.querySelector('.msg.tools .chip .tverb');
  assert.ok(chip, 'вызов инструмента нарисован текстом: ' + log.innerHTML);
  assert.equal(chip.textContent, 'state_get');
  assert.equal(doc.getElementById('agTag').hidden, false, 'нет метки продолжения при живом разговоре');
});

test('причина пустой ленты названа словами, а не показана пустотой', async () => {
  const { doc } = await boot({
    agentSession: 's-42',
    agentHistory: { ok: true, sessionId: 's-42', items: [], reason: 'транскрипт не найден — история недоступна' },
  });
  const log = await openAgent(doc);
  assert.match(log.textContent, /транскрипт не найден/, 'причина проглочена: ' + log.textContent);
});

test('разговора ещё не было — приглашение, а не вид поломки', async () => {
  const { doc } = await boot({ agentHistory: { ok: true, items: [], reason: 'нет сохранённого разговора' } });
  const log = await openAgent(doc);
  assert.equal(doc.getElementById('agTag').hidden, true, 'метка продолжения врёт про пустую нить');
  // «нет сохранённого разговора» — не беда, а нормальное первое открытие
  assert.doesNotMatch(log.textContent, /нет сохранённого разговора/);
  assert.match(log.textContent, /Спроси Джарвиса/, 'приглашения писать нет: ' + log.textContent);
});

test('поток ответа во вкладке — тот же, что в отдельном окне', async () => {
  const { doc, subs } = await boot();
  const log = await openAgent(doc);
  assert.ok(subs.onAgentEvent, 'вкладка не подписалась на поток агента');

  subs.onAgentEvent({ type: 'delta', text: 'ду' });
  subs.onAgentEvent({ type: 'delta', text: 'маю' });
  subs.onAgentEvent({ type: 'tool_use', name: 'state_get' });
  subs.onAgentEvent({ type: 'done', session_id: 's-new' });
  await new Promise((r) => setTimeout(r, 0));

  assert.match(log.textContent, /думаю/, 'дельты не собрались в один пузырь');
  assert.ok(log.querySelector('.msg.tools .chip'), 'тул-вызов из потока не показан чипом');
  assert.equal(doc.getElementById('agSend').disabled, false, 'вкладка осталась в «думает…»');
});

test('сообщение из вкладки уходит в ту же нить разговора', async () => {
  const { doc, calls } = await boot({ agentSession: 's-42' });
  await openAgent(doc);
  doc.getElementById('agInput').value = 'привет';
  doc.getElementById('agSend').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));
  // Адресат — чат, нить идёт следом: у свежего чата нити ещё нет, и по одной
  // нити реплика уезжала в тот чат, который в этот момент оказался текущим.
  assert.ok(
    calls.some((c) => c[0] === 'agentSend' && c[1] === 'привет' && c[2] === 'c1' && c[3] === 's-42'),
    'реплика ушла мимо продолжаемого разговора: ' + JSON.stringify(calls.filter((c) => c[0] === 'agentSend')),
  );
});

/* ---------- история разговоров ----------
 *
 * Один разговор на всё — это один контекст на все проекты сразу, а список из
 * «Чат 5» и «Чат 6» — это потерянный разговор на 249 реплик: файл на диске, а
 * дороги к нему нет. Тесты стерегут то, чем такой список ломается тихо: чужая
 * лента после переключения, отказ демона, съеденный молча, кнопка, которой
 * заведомо откажут, и заголовок, выдуманный на месте вместо присланного. */

const CHATS = [
  { id: 'c1', name: 'Джарвис', sessionId: 's-1' },
  { id: 'c2', name: 'Выборы', sessionId: 's-2' },
  { id: 'c3', name: 'Грант', sessionId: null },
];

// Колонка видна всегда — раскрывать нечего; строки берём как есть.
const rows = (doc) => [...doc.querySelectorAll('#agChats .agchat')];
const rowNames = (doc) => rows(doc).map((c) => c.querySelector('.agname').textContent);
const rowBy = (doc, re) => rows(doc).find((c) => re.test(c.textContent));
const settle = () => new Promise((r) => setTimeout(r, 0)).then(() => new Promise((r) => setTimeout(r, 0)));
/* Редкие действия строки — под «…» и под правым кликом. Тремя иконками в ряд
 * они читались как три равные кнопки, хотя одна из них необратима. */
const menuOf = (doc, row) => {
  row.querySelector('.agdots').dispatchEvent(click(doc));
  return [...doc.querySelectorAll('#agChats .agmi')];
};
const pick = (items, re) => items.find((i) => re.test(i.textContent));

test('разговоры Джарвиса видны колонкой без раскрытия, открытый — помечен', async () => {
  const { doc } = await boot({ agentChats: CHATS });
  await openAgent(doc);
  // ровно то, ради чего колонка и заведена: чаты на экране сразу
  assert.deepEqual(rowNames(doc), ['Джарвис', 'Выборы', 'Грант']);
  const on = doc.querySelectorAll('#agChats .agchat.on');
  assert.equal(on.length, 1, 'открытый чат не помечен (или помечено несколько)');
  assert.equal(on[0].querySelector('.agname').textContent, 'Джарвис');
  // «+ Новый чат» сверху и на виду, а не спрятан за раскрытием
  assert.equal(doc.querySelectorAll('#agChats .agnew').length, 1, 'нечем создать чат');
  // выпадающая шапка не вернулась
  assert.equal(doc.querySelectorAll('#agChats .agtoggle').length, 0, 'список снова стал меню');
});

/* Порядок — по последней активности: разговор, в котором только что говорили,
 * обязан быть сверху. Иначе список из двадцати чатов приходится помнить
 * наизусть, а искать в нём — глазами сверху вниз. */
/* Раньше здесь закреплялся порядок «по последней активности». Жалоба на то,
 * что активный чат уползает наверх, повторилась трижды: место перестаёт быть
 * местом. Порядок теперь задаёт человек, и список его не пересобирает. */
test('порядок строк — как у демона, активность его не меняет', async () => {
  const now = Date.now();
  const { doc } = await boot({
    agentChats: [
      { id: 'c1', name: 'Старый', sessionId: 's-1', turns: 3, at: now - 6e5 },
      { id: 'c2', name: 'Свежий', sessionId: 's-2', turns: 5, at: now },
      { id: 'c3', name: 'Позавчерашний', sessionId: 's-3', turns: 1, at: now - 2 * 864e5 },
    ],
  });
  await openAgent(doc);
  assert.deepEqual(rowNames(doc), ['Старый', 'Свежий', 'Позавчерашний'],
    'список пересобрал себя по свежести — ровно то, на что человек жаловался трижды');
});

/* Поиск на экране Джарвиса — один и общий: та же строка в шапке панели, что
 * ищет сессии, здесь ищет его чаты. Своего поля колонка больше не рисует: два
 * почти одинаковых поля стояли рядом, и по виду было не отличить, что где
 * ищет. */
test('поиск панели на экране Джарвиса отсеивает его чаты, а не сессии', async () => {
  const { doc, window } = await boot({ agentChats: CHATS });
  await openAgent(doc);
  const find = doc.getElementById('query');
  assert.equal(doc.querySelectorAll('#agChats .agfind').length, 0, 'колонка снова завела второе поле поиска');
  assert.match(find.placeholder, /Джарвис/, 'поле не сказало, что теперь ищет чаты Джарвиса');

  const type = (v) => { find.value = v; find.dispatchEvent(new window.Event('input', { bubbles: true })); };
  type('выбор');
  assert.deepEqual(rowNames(doc), ['Выборы'], 'поиск не отсеял соседей');

  type('ничего такого');
  assert.equal(rows(doc).length, 0);
  // пустой результат объясняется словами, а не пустотой
  assert.match(doc.querySelector('#agChats .agempty').textContent, /Ничего не нашлось/);

  type('');
  assert.deepEqual(rowNames(doc), ['Джарвис', 'Выборы', 'Грант'], 'список не вернулся');
});

/* Два поиска — два разных: набранное в сессиях не уезжает к Джарвису и не
 * пропадает, пока смотришь его чаты. Одно поле на двоих иначе значило бы, что
 * возврат к сессиям каждый раз стирает фильтр. */
test('поиск по сессиям и поиск по чатам Джарвиса не смешиваются', async () => {
  const { doc, window } = await boot({ agentChats: CHATS, state: [SESSION] });
  const find = doc.getElementById('query');
  const type = (v) => { find.value = v; find.dispatchEvent(new window.Event('input', { bubbles: true })); };
  const was = find.placeholder;
  type('jarvis');

  await openAgent(doc);
  assert.equal(find.value, '', 'фильтр сессий уехал в поиск по чатам Джарвиса');
  type('выбор');
  assert.deepEqual(rowNames(doc), ['Выборы']);

  doc.getElementById('tabSessions').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(find.value, 'jarvis', 'фильтр сессий не вернулся');
  assert.equal(find.placeholder, was, 'подпись поля осталась от Джарвиса');

  await openAgent(doc);
  assert.equal(find.value, 'выбор', 'поиск по чатам Джарвиса не вернулся');
  assert.deepEqual(rowNames(doc), ['Выборы'], 'список чатов вернулся без своего фильтра');
});

/* ---------- «Джарвис» в оконном режиме: отдельный экран, а не третья колонка ----
 *
 * На живой сборке чаты Джарвиса открывались СПРАВА от списка сессий: на экране
 * стояли разом и сессии CLI, и колонка его чатов, и переписка — окно делилось
 * на лишние колонки. Вкладка — другой экран того же окна: слева либо сессии,
 * либо чаты Джарвиса, но не оба списка бок о бок. */

// то же окно, но в оконном режиме (14h): режим приезжает из settings.json
const winBoot = (data = {}) => boot({ ...data, settings: { mode: 'window', ...(data.settings || {}) } });

test('в окне «Джарвис» занимает левую колонку вместо сессий, а не рядом с ними', async () => {
  const { doc } = await winBoot({ agentChats: CHATS, state: [SESSION] });
  assert.equal(doc.documentElement.getAttribute('data-mode'), 'window', 'окно не встало в оконный режим');
  // до вкладки слева сессии, колонка чатов ждёт спрятанной
  assert.equal(doc.getElementById('list').hidden, false, 'сайдбар сессий пуст ещё до Джарвиса');
  assert.equal(doc.getElementById('agChats').hidden, true, 'колонка чатов видна вне своего экрана');

  await openAgent(doc);
  assert.equal(doc.getElementById('list').hidden, true, 'список сессий остался рядом с чатами Джарвиса');
  assert.equal(doc.getElementById('agChats').hidden, false, 'колонка чатов не показана');
  // Колонка стоит в СЕТКЕ окна, а не внутри переписки: живя в правой колонке,
  // она и добавляла третью полосу справа от списка сессий.
  assert.equal(doc.getElementById('agChats').parentElement.id, 'panel', 'колонка чатов не в сетке окна');
  assert.equal(doc.querySelectorAll('#content .agside').length, 0, 'колонка снова делит правую колонку с перепиской');
  assert.equal(doc.getElementById('panel').dataset.agent, '1', 'сетка не узнала, что колонка теперь Джарвиса');
});

test('возврат к сессиям возвращает их список, а Джарвис уходит целиком', async () => {
  const { doc, subs } = await winBoot({ agentChats: CHATS, state: [SESSION] });
  subs.onState([SESSION]);
  await openAgent(doc);
  doc.getElementById('tabSessions').dispatchEvent(click(doc));
  await settle();

  assert.equal(doc.getElementById('list').hidden, false, 'список сессий не вернулся');
  assert.equal(doc.getElementById('agChats').hidden, true, 'чаты Джарвиса остались на экране сессий');
  assert.equal(doc.getElementById('agentPane').hidden, true, 'переписка с Джарвисом осталась на экране');
  assert.equal(doc.getElementById('panel').dataset.agent, '0', 'сетка так и держит колонку за Джарвисом');
  assert.match(doc.getElementById('list').textContent, /jarvis/, 'список вернулся пустым');
});

/* «Ровно две колонки при любой ширине» проверяется не замером (в тесте нет
 * вёрстки), а тем, из-за чего третья колонка вообще появлялась: местом в сетке.
 * Колонка Джарвиса занимает ТУ ЖЕ область, что список сессий, а ширина окна на
 * число колонок не влияет — правил по ширине в раскладке нет вовсе. */
test('колонок в окне ровно две при любой ширине', async () => {
  const html = read('index.html');
  const grid = html.match(/\[data-mode='window'\] \.panel \{([^}]*)\}/)[1];
  assert.match(grid, /grid-template-columns: 264px minmax\(0, 1fr\)/, 'колонок в сетке окна не две');
  const docked = html.match(/\.agside\.docked \{([^}]*)\}/)[1];
  assert.match(docked, /grid-area: list/, 'колонка чатов встала мимо области списка сессий');
  const byWidth = [...html.matchAll(/@media ([^{]+)\{/g)].map((m) => m[1]).filter((q) => /width/.test(q));
  assert.deepEqual(byWidth, [], 'раскладка стала зависеть от ширины окна: ' + byWidth.join(' | '));

  // и на живом DOM: в левой колонке всегда ровно один житель
  const { doc } = await winBoot({ agentChats: CHATS, state: [SESSION] });
  const left = () => ['list', 'agChats'].filter((id) => !doc.getElementById(id).hidden);
  assert.deepEqual(left(), ['list']);
  await openAgent(doc);
  assert.deepEqual(left(), ['agChats']);
});

/* Накладной режим (⌘J, 820px) остаётся прежним: там левой колонки нет вовсе,
 * список уходит сам, и Джарвис занимает панель целиком — вместе со своей
 * колонкой внутри переписки. */
test('в накладке Джарвис по-прежнему занимает панель целиком', async () => {
  const { doc } = await boot({ agentChats: CHATS, state: [SESSION] });
  await openAgent(doc);
  assert.equal(doc.getElementById('list').hidden, true, 'список сессий не уступил панель');
  assert.equal(doc.getElementById('agChats').parentElement.className, 'agwrap', 'колонка уехала из переписки');
  assert.equal(doc.querySelectorAll('#content .agside').length, 1, 'колонка потерялась по дороге');
});

test('переключение туда-обратно не теряет чат, прокрутку и черновик', async () => {
  const { doc } = await winBoot({ agentChats: CHATS });
  await openAgent(doc);
  rowBy(doc, /Выборы/).dispatchEvent(click(doc));
  await settle();
  const input = doc.getElementById('agInput');
  const log = doc.getElementById('agLog');
  input.value = 'недописанное Джарвису';
  log.scrollTop = 120;

  doc.getElementById('tabSessions').dispatchEvent(click(doc));
  await settle();
  // спрятанному узлу браузер обнуляет прокрутку сам — повторяем это руками
  log.scrollTop = 0;

  await openAgent(doc);
  await settle();
  assert.equal(input.value, 'недописанное Джарвису', 'черновик Джарвису пропал');
  assert.equal(log.scrollTop, 120, 'лента вернулась не на то место');
  const on = doc.querySelectorAll('#agChats .agchat.on');
  assert.equal(on.length, 1, 'открытый чат не помечен');
  assert.equal(on[0].querySelector('.agname').textContent, 'Выборы', 'вернулись не в тот чат');
});

test('ширину колонки тянут за границу сетки, и она помнится', async () => {
  const { doc, window, calls } = await winBoot({ agentChats: CHATS, settings: { agentSideWidth: 300 } });
  await openAgent(doc);
  const panel = doc.getElementById('panel');
  assert.equal(panel.style.getPropertyValue('--side-w'), '300px', 'запомненная ширина не доехала до сетки');

  const drag = (type, x) => { const e = new window.Event(type, { bubbles: true }); e.clientX = x; return e; };
  doc.querySelector('#agChats .aggrip').dispatchEvent(drag('mousedown', 300));
  doc.dispatchEvent(drag('mousemove', 340));
  doc.dispatchEvent(drag('mouseup', 340));

  assert.equal(panel.style.getPropertyValue('--side-w'), '340px', 'граница колонки не поехала за мышью');
  assert.equal(doc.getElementById('agChats').style.width, '', 'ширина ушла в сам элемент — колонке сетки от неё ни холодно ни жарко');
  const saved = calls.filter((c) => c[0] === 'setSettings').map((c) => c[1]);
  assert.ok(saved.some((p) => p && p.agentSideWidth === 340), 'ширину не запомнили: ' + JSON.stringify(saved));
  // ...а свёрнутость встроенная колонка не выбирает и чужую не затирает
  assert.deepEqual(saved.filter((p) => p && p.agentSideOff !== undefined), [], 'переписана свёрнутость окна из трея');

  // Под вкладки колонку не утянуть: над списком чатов стоят поиск и вкладки
  // окна, и на 180px читать их уже нечем.
  doc.querySelector('#agChats .aggrip').dispatchEvent(drag('mousedown', 340));
  doc.dispatchEvent(drag('mousemove', 40));
  doc.dispatchEvent(drag('mouseup', 40));
  assert.equal(panel.style.getPropertyValue('--side-w'), '240px', 'колонку утянули уже вкладок');
});

/* Свернуть встроенную колонку нечем — и это не пропажа: вместе с ней ушли бы
 * вкладки, то есть дорога назад к сессиям. Внутри переписки (накладка, окно из
 * трея) сворачивание остаётся: там колонка отнимает место у разговора. */
test('свернуть предлагают только ту колонку, что стоит внутри переписки', async () => {
  const { doc } = await winBoot({ agentChats: CHATS });
  await openAgent(doc);
  assert.equal(doc.querySelectorAll('#agChats .agfold').length, 0, 'сворачивание унесло бы вкладки вместе с колонкой');

  const { doc: over } = await boot({ agentChats: CHATS });
  await openAgent(over);
  assert.equal(over.querySelectorAll('#agChats .agfold').length, 1, 'колонку внутри переписки стало нечем свернуть');
});

test('переключение чата — один клик по строке, и история спрошена про него', async () => {
  const { doc, calls } = await boot({ agentChats: CHATS });
  await openAgent(doc);
  rowBy(doc, /Выборы/).dispatchEvent(click(doc));
  await settle();

  assert.ok(calls.some((c) => c[0] === 'agentChatSwitch' && c[1] === 'c2'), 'демон не узнал о переключении');
  const asked = calls.filter((c) => c[0] === 'agentChatHistory').map((c) => c[1]);
  assert.deepEqual(asked, ['c1', 'c2'], 'история спрошена не про тот чат');
  // и колонка осталась на месте: она не меню, чтобы закрываться после выбора
  assert.deepEqual(rowNames(doc), ['Джарвис', 'Выборы', 'Грант']);
  assert.equal(doc.querySelectorAll('#agChats .agchat.on')[0].querySelector('.agname').textContent, 'Выборы');
});

test('пока история едет — видно, что идёт загрузка, а потом лента чужого чата уходит', async () => {
  let release;
  const gate = new Promise((r) => { release = r; });
  const { doc } = await boot({
    agentChats: CHATS,
    agentHistory: (id) => (id === 'c2' ? gate : { ok: true, items: [{ role: 'user', kind: 'text', text: 'это первый чат', ts: 1 }] }),
  });
  const log = await openAgent(doc);
  assert.match(log.textContent, /это первый чат/);

  rowBy(doc, /Выборы/).dispatchEvent(click(doc));
  await settle();
  assert.match(log.textContent, /Загружаю переписку/, 'молчание вместо признака загрузки: ' + log.textContent);
  assert.doesNotMatch(log.textContent, /это первый чат/, 'на экране осталась переписка соседнего чата');

  release({ ok: true, items: [{ role: 'user', kind: 'text', text: 'это второй чат', ts: 1 }] });
  await settle();
  assert.match(log.textContent, /это второй чат/);
  assert.doesNotMatch(log.textContent, /Загружаю переписку/, 'признак загрузки завис после ответа');
});

test('новый чат с потерянной нитью объясняется словами, а не пустотой', async () => {
  const { doc } = await boot({
    agentChats: CHATS,
    agentHistory: (id) => (id === 'c2'
      ? { ok: true, sessionId: 's-2', items: [], reason: 'транскрипт не найден — история недоступна' }
      : { ok: true, items: [] }),
  });
  const log = await openAgent(doc);
  rowBy(doc, /Выборы/).dispatchEvent(click(doc));
  await settle();
  assert.match(log.textContent, /транскрипт не найден/, 'пустая лента без причины: ' + log.textContent);
});

test('отказ демона на переключение чата виден в ленте', async () => {
  const { doc } = await boot({
    agentChats: CHATS,
    agentSwitch: { ok: false, error: 'чата «c9» нет в списке — обнови список' },
  });
  const log = await openAgent(doc);
  rowBy(doc, /Выборы/).dispatchEvent(click(doc));
  await settle();
  assert.match(log.textContent, /нет в списке/, 'отказ съеден молча: ' + log.textContent);
  assert.ok(log.querySelector('.msg.err'), 'отказ нарисован не как отказ');
  // и пометка осталась там, где мы на самом деле остались
  assert.equal(doc.querySelectorAll('#agChats .agchat.on')[0].querySelector('.agname').textContent, 'Джарвис');
});

test('отказ демона на создание чата виден в ленте', async () => {
  const { doc } = await boot({ agentChats: CHATS, agentCreate: { ok: false, error: 'имя длиннее 60 символов' } });
  const log = await openAgent(doc);
  doc.querySelector('#agChats .agnew').dispatchEvent(click(doc));
  await settle();
  assert.match(log.textContent, /длиннее 60/, 'отказ создания съеден: ' + log.textContent);
  assert.deepEqual(rowNames(doc), ['Джарвис', 'Выборы', 'Грант'], 'колонка соврала о несозданном чате');
});

test('последний чат убрать не предлагают — демон всё равно откажет', async () => {
  const one = await boot(); // заглушка по умолчанию даёт ровно один чат
  await openAgent(one.doc);
  // ни убрать, ни стереть за ним файл: ядро откажет и на то, и на другое
  assert.deepEqual(menuOf(one.doc, rows(one.doc)[0]).map((i) => i.textContent), ['Переименовать']);

  const many = await boot({ agentChats: CHATS });
  await openAgent(many.doc);
  assert.ok(pick(menuOf(many.doc, rowBy(many.doc, /Джарвис/)), /Скрыть/), 'убрать чат нечем');
});

test('удаление открытого чата уводит на соседний и перерисовывает ленту', async () => {
  const { doc, calls } = await boot({ agentChats: CHATS });
  await openAgent(doc);
  pick(menuOf(doc, rowBy(doc, /Джарвис/)), /Скрыть/).dispatchEvent(click(doc));
  await settle();
  assert.ok(calls.some((c) => c[0] === 'agentChatDelete' && c[1] === 'c1'), 'демон не узнал об удалении');
  assert.deepEqual(rowNames(doc), ['Выборы', 'Грант']);
  const asked = calls.filter((c) => c[0] === 'agentChatHistory').map((c) => c[1]);
  assert.deepEqual(asked, ['c1', 'c2'], 'лента осталась от удалённого чата');
});

/* Вкладка монтируется однажды, а чат могли переключить в окне из трея: при
 * возврате список сверяется заново. Ленту при этом не трогаем — она мигала бы
 * на каждом переключении вкладок. */
test('возврат на вкладку сверяет список чатов с демоном', async () => {
  const { doc, calls } = await boot({ agentChats: CHATS });
  await openAgent(doc);
  doc.getElementById('tabSessions').dispatchEvent(click(doc));
  await openAgent(doc);
  await settle();
  assert.equal(calls.filter((c) => c[0] === 'agentChatsList').length, 2, 'вкладка показывает список из прошлой жизни');
  assert.equal(calls.filter((c) => c[0] === 'agentChatHistory').length, 1, 'лента перечитана без причины');
});

/* Переименование обязано НАХОДИТЬСЯ. Кликом по открытому чипу его знал только
 * тот, кто это писал: «а то сейчас это как-то криво сделано». Теперь оно первым
 * пунктом в меню строки, а клик по самой строке значит ровно то, что обещает, —
 * открыть чат. */
test('имя чата правится из меню строки, отказ называет причину', async () => {
  const { doc, calls } = await boot({ agentChats: CHATS });
  await openAgent(doc);
  const items = menuOf(doc, rowBy(doc, /Джарвис/));
  assert.deepEqual(items.map((i) => i.textContent), ['Переименовать', 'Скрыть', 'Забыть насовсем']);
  pick(items, /Переименовать/).dispatchEvent(click(doc));
  const inp = doc.querySelector('#agChats input.agrename');
  assert.ok(inp, 'правка имени не открылась: ' + doc.getElementById('agChats').innerHTML);
  assert.equal(inp.maxLength, 60, 'потолок длины разошёлся с демоном');
  inp.value = 'ГД-2026';
  inp.dispatchEvent(key(doc, 'Enter'));
  await settle();
  assert.ok(calls.some((c) => c[0] === 'agentChatRename' && c[1] === 'c1' && c[2] === 'ГД-2026'), 'имя не уехало демону');
  assert.deepEqual(rowNames(doc), ['ГД-2026', 'Выборы', 'Грант']);
  assert.equal(calls.filter((c) => c[0] === 'agentChatSwitch').length, 0, 'правка имени переключила чат');
});

// Правый клик по строке ведёт туда же, куда «…»: два набора действий разъехались бы
test('правый клик по строке открывает то же меню, что и «…»', async () => {
  const { doc, window } = await boot({ agentChats: CHATS });
  await openAgent(doc);
  const e = new window.Event('contextmenu', { bubbles: true });
  e.preventDefault = () => {};
  rowBy(doc, /Выборы/).dispatchEvent(e);
  const items = [...doc.querySelectorAll('#agChats .agmi')].map((i) => i.textContent);
  assert.deepEqual(items, ['Переименовать', 'Скрыть', 'Забыть насовсем']);
});

test('отказ переименования чата не притворяется удавшимся', async () => {
  const { doc } = await boot({ agentChats: CHATS, agentRename: { ok: false, error: 'пустое имя — у чата должно быть название' } });
  const log = await openAgent(doc);
  pick(menuOf(doc, rowBy(doc, /Джарвис/)), /Переименовать/).dispatchEvent(click(doc));
  const inp = doc.querySelector('#agChats input.agrename');
  inp.value = '';
  inp.dispatchEvent(key(doc, 'Enter'));
  await settle();
  assert.match(log.textContent, /должно быть название/, 'отказ съеден: ' + log.textContent);
  assert.deepEqual(rowNames(doc), ['Джарвис', 'Выборы', 'Грант'], 'колонка показала имя, которого нет');
});

/* Ровно та беда, с которой всё началось: разговор на 249 реплик лежал в
 * транскриптах, а чата за ним не было — и дотянуться до него было нельзя.
 * Строка с диска и есть эта дорога, а клик по ней — привязка, а не
 * переключение: переключать нечего, чата ещё нет. */
const THREAD = (sessionId, name, turns = 249) => ({
  id: null, name, named: false, sessionId, current: false,
  turns, at: Date.now(), preview: name,
});

test('разговор с диска — строка того же списка, а не отдельный раздел', async () => {
  const { doc, calls } = await boot({ agentChats: CHATS, agentThreads: [THREAD('a25d01f8', 'Изучите текущие сессии')] });
  await openAgent(doc);
  assert.equal(rows(doc).length, 4, 'разговор с диска не попал в список');

  const disk = doc.querySelector('#agChats .agchat.disk');
  assert.ok(disk, 'разговора с диска нет в списке: ' + rowNames(doc).join(' · '));
  assert.match(disk.textContent, /с диска/, 'строку с диска не отличить от привязанного чата');
  assert.match(disk.textContent, /249 реплик/, 'не видно, насколько разговор большой');

  disk.dispatchEvent(click(doc));
  await settle();
  assert.ok(calls.some((c) => c[0] === 'agentChatOpen' && c[1] === 'a25d01f8'), 'клик по разговору с диска ничего не привязал');
  assert.equal(calls.filter((c) => c[0] === 'agentChatSwitch').length, 0, 'строку с диска пробовали переключить — чата за ней нет');

  // привязался — и стал обычным чатом: строка с диска ушла, имя правится
  assert.equal(doc.querySelectorAll('#agChats .agchat.disk').length, 0, 'разговор задвоился: и чатом, и строкой с диска');
  assert.ok(pick(menuOf(doc, rowBy(doc, /Изучите/)), /Переименовать/), 'привязанный разговор всё ещё нельзя переименовать');
});

test('отказ на открытие разговора с диска виден в ленте', async () => {
  const { doc } = await boot({
    agentChats: CHATS,
    agentThreads: [THREAD('a25d01f8', 'Изучите текущие сессии')],
    agentOpen: { ok: false, error: 'разговора a25d01f8 нет на диске — открыть его не получится' },
  });
  const log = await openAgent(doc);
  doc.querySelector('#agChats .agchat.disk').dispatchEvent(click(doc));
  await settle();
  assert.match(log.textContent, /нет на диске/, 'отказ съеден молча: ' + log.textContent);
});

/* Удаление теряет имя и место, а не беседу: разговор возвращается в список
 * строкой с диска. Иначе «Скрыть» — это тихое уничтожение переписки. */
test('убранный чат остаётся в списке разговором с диска', async () => {
  const { doc } = await boot({ agentChats: CHATS, agentThreads: [THREAD('s-1', 'Главный разговор')] });
  await openAgent(doc);
  assert.equal(doc.querySelectorAll('#agChats .agchat.disk').length, 0, 'живой чат задвоен строкой с диска');

  pick(menuOf(doc, rowBy(doc, /Джарвис/)), /Скрыть/).dispatchEvent(click(doc));
  await settle();
  const disk = doc.querySelector('#agChats .agchat.disk');
  assert.ok(disk, 'чат убрали — и разговор пропал вместе с ним: ' + rowNames(doc).join(' · '));
  assert.match(disk.textContent, /Главный разговор/);
  assert.match(disk.textContent, /249 реплик/, 'разговор в списке, но без размера его не узнать');
});

/* ---------- убрать и забыть ----------
 *
 * «Почему я не могу удалить некоторые чаты»: у строки, найденной на диске, не
 * было ни одной кнопки — убрать её из списка было нечем. Теперь у каждой строки
 * своё меню, и пункты в нём значат разное. «Скрыть» обратимо: файл остаётся,
 * возврат одним нажатием. «Забыть насовсем» стирает транскрипт и потому
 * спрашивает вслух. */

const DISK_ROW = (doc) => doc.querySelector('#agChats .agchat.disk');
const withThread = (over = {}) => ({
  agentChats: CHATS,
  agentThreads: [THREAD('a25d01f8', 'Изучите текущие сессии')],
  ...over,
});

test('«Скрыть» у строки с диска прячет её, а транскрипт не трогает', async () => {
  const { doc, calls } = await boot(withThread());
  await openAgent(doc);
  const hide = pick(menuOf(doc, DISK_ROW(doc)), /Скрыть/);
  assert.ok(hide, 'строку с диска по-прежнему нечем убрать: ' + doc.getElementById('agChats').textContent);
  hide.dispatchEvent(click(doc));
  await settle();

  assert.ok(calls.some((c) => c[0] === 'agentHistoryHide' && c[1] === 'a25d01f8'), '«Скрыть» ничего не спрятало');
  assert.equal(calls.filter((c) => c[0] === 'agentHistoryForget').length, 0, 'скрытие обернулось удалением файла');
  assert.equal(doc.querySelectorAll('#agChats .agchat.disk').length, 0, 'спрятанная строка осталась в списке');
  assert.deepEqual(rowNames(doc), ['Джарвис', 'Выборы', 'Грант'], 'вместе со строкой ушёл чей-то чат');
});

test('«скрыто N · вернуть» видно в колонке и возвращает одним нажатием', async () => {
  const { doc, calls } = await boot(withThread());
  await openAgent(doc);
  pick(menuOf(doc, DISK_ROW(doc)), /Скрыть/).dispatchEvent(click(doc));
  await settle();

  const back = doc.querySelector('#agChats .aghidden');
  assert.ok(back, 'разговор спрятан бесследно: ' + doc.getElementById('agChats').textContent);
  assert.match(back.textContent, /Скрыто 1 · вернуть/);

  back.dispatchEvent(click(doc));
  await settle();
  assert.ok(calls.some((c) => c[0] === 'agentHistoryUnhideAll'), 'возврат не уехал демону');
  assert.ok(DISK_ROW(doc), 'вернули — а строка не вернулась: ' + rowNames(doc).join(' · '));
  assert.equal(doc.querySelectorAll('#agChats .aghidden').length, 0, 'прятать нечего, а строка про скрытое висит');
});

/* Необратимое действие обязано спросить — и спросить видимой кнопкой: alt-клик
 * нельзя обнаружить, и человек стирал бы разговор, не зная, что согласился. */
test('забыть насовсем спрашивает и до ответа ничего не удаляет', async () => {
  const { doc, calls } = await boot(withThread());
  await openAgent(doc);
  const f = pick(menuOf(doc, DISK_ROW(doc)), /Забыть насовсем/);
  assert.ok(f, 'удалить разговор с диска нечем: ' + doc.getElementById('agChats').textContent);
  // необратимое отличимо от обратимых соседей, но не кричит кнопкой-светофором
  assert.ok(f.classList.contains('danger'), 'забвение набрано вровень с обратимым скрытием');
  f.dispatchEvent(click(doc));
  await settle();

  const ask = doc.querySelector('#agChats .agask');
  assert.ok(ask, 'удаление случилось без вопроса: ' + doc.getElementById('agChats').textContent);
  assert.equal(calls.filter((c) => c[0] === 'agentHistoryForget').length, 0, 'спросили — и удалили, не дожидаясь ответа');
  // вопрос называет, ЧТО исчезнет: по заголовку и размеру разговор и узнают
  assert.match(ask.textContent, /Изучите текущие сессии/, 'вопрос не назвал разговор: ' + ask.textContent);
  assert.match(ask.textContent, /249 реплик/, 'вопрос не сказал, сколько теряется: ' + ask.textContent);
  assert.doesNotMatch(ask.textContent, /!/, 'спокойный тон разменяли на восклицание');

  const yes = ask.querySelector('.agbtn.danger');
  yes.dispatchEvent(click(doc));
  yes.dispatchEvent(click(doc)); // повтор по уже отвеченному вопросу
  await settle();
  const forgot = calls.filter((c) => c[0] === 'agentHistoryForget').map((c) => c[1]);
  assert.deepEqual(forgot, ['a25d01f8'], 'согласие не дошло до демона или уехало дважды');
  assert.equal(doc.querySelectorAll('#agChats .agchat.disk').length, 0, 'забытый разговор остался в списке');
  assert.deepEqual(rowNames(doc), ['Джарвис', 'Выборы', 'Грант'], 'вместе с разговором ушёл чей-то чат');
});

/* Вопрос жил внутри перерисовываемой строки — и ответ соседнего чата сносил его
 * из-под руки: человек тянулся к «Удалить», а карточки уже не было. Теперь он
 * стоит слоем поверх колонки и перерисовку списка переживает. */
test('вопрос про удаление не сносит событие соседнего разговора', async () => {
  const { doc, subs } = await boot(withThread());
  await openAgent(doc);
  pick(menuOf(doc, DISK_ROW(doc)), /Забыть насовсем/).dispatchEvent(click(doc));
  await settle();
  assert.ok(doc.querySelector('#agChats .agask'), 'вопроса нет ещё до всяких событий');

  subs.onAgentEvent({ type: 'delta', text: 'думаю', chatId: 'c2' });
  await settle();
  const ask = doc.querySelector('#agChats .agask');
  assert.ok(ask, 'вопрос снесло ответом соседнего чата: ' + doc.getElementById('agChats').textContent);
  assert.equal(ask.querySelectorAll('.agbtn').length, 2, 'у живого вопроса пропал выбор');
});

test('«Отмена» закрывает вопрос и оставляет транскрипт на месте', async () => {
  const { doc, calls } = await boot(withThread());
  await openAgent(doc);
  pick(menuOf(doc, DISK_ROW(doc)), /Забыть насовсем/).dispatchEvent(click(doc));
  await settle();
  const no = [...doc.querySelectorAll('#agChats .agask .agbtn')].find((b) => /Отмена/.test(b.textContent));
  assert.ok(no, 'из вопроса нет выхода без удаления');
  no.dispatchEvent(click(doc));
  await settle();

  assert.equal(calls.filter((c) => c[0] === 'agentHistoryForget').length, 0, 'отмена удалила разговор');
  assert.equal(doc.querySelectorAll('#agChats .agask').length, 0, 'вопрос остался висеть после отмены');
  assert.ok(DISK_ROW(doc), 'строка пропала после отмены: ' + rowNames(doc).join(' · '));
});

/* Отказы ядра тут не ошибки, а порядок действий: «идёт ход» и «привязан к чату»
 * говорят, что сделать сначала. Съесть их молча — значит показать список, из
 * которого непонятно, почему разговор всё ещё здесь. */
test('отказ «идёт ход» виден словами, а строка возвращается на место', async () => {
  const { doc } = await boot(withThread({
    agentForget: { ok: false, error: 'по разговору a25d01f8 прямо сейчас идёт ход — дождись ответа агента' },
  }));
  const log = await openAgent(doc);
  pick(menuOf(doc, DISK_ROW(doc)), /Забыть насовсем/).dispatchEvent(click(doc));
  await settle();
  doc.querySelector('#agChats .agask .agbtn.danger').dispatchEvent(click(doc));
  await settle();

  assert.match(log.textContent, /дождись ответа агента/, 'отказ съеден молча: ' + log.textContent);
  assert.ok(log.querySelector('.msg.err'), 'отказ нарисован не как отказ');
  assert.ok(DISK_ROW(doc), 'разговор цел, а строка пропала: ' + rowNames(doc).join(' · '));
});

/* «Забыть насовсем» у чата: ядро само файл за чатом не отдаст («привязан к чату
 * — сначала удали чат»), поэтому оба шага делает окно. Согласились именно на
 * это, и вопрос так и был задан. */
test('забыть насовсем у чата сперва убирает чат, потом стирает транскрипт', async () => {
  const { doc, calls } = await boot({ agentChats: CHATS });
  await openAgent(doc);
  pick(menuOf(doc, rowBy(doc, /Выборы/)), /Забыть насовсем/).dispatchEvent(click(doc));
  await settle();
  const ask = doc.querySelector('#agChats .agask');
  assert.match(ask.textContent, /Чат исчезнет вместе с ним/, 'вопрос умолчал про судьбу чата: ' + ask.textContent);

  ask.querySelector('.agbtn.danger').dispatchEvent(click(doc));
  await settle();
  assert.ok(calls.some((c) => c[0] === 'agentChatDelete' && c[1] === 'c2'), 'чат остался, а файл уже стирают');
  assert.ok(calls.some((c) => c[0] === 'agentHistoryForget' && c[1] === 's-2'), 'транскрипт не стёрли');
});

/* Заголовок собирает демон: имя человека → первая реплика → «Новый чат». Свой
 * фолбэк здесь и был тем самым «Чат 5», по которому разговор не найти. */
test('заголовок строки берётся у демона, а не выдумывается на месте', async () => {
  const { doc } = await boot({
    agentChats: [
      { id: 'c1', name: 'Изучите текущие сессии', named: false, sessionId: 's-1', turns: 249, at: Date.now(), preview: 'Изучите текущие сессии' },
      { id: 'c2', name: 'Новый чат', named: false, sessionId: null, turns: 0, at: null, preview: '' },
    ],
  });
  await openAgent(doc);
  // порядок — как у демона: пустой чат создан последним и стоит последним
  assert.deepEqual(rowNames(doc), ['Изучите текущие сессии', 'Новый чат']);
  const all = doc.getElementById('agChats').textContent;
  assert.doesNotMatch(all, /Чат \d/, 'вернулся выдуманный номер вместо заголовка: ' + all);
  // когда и насколько большой — то, по чему разговор и узнают в списке
  const big = rowBy(doc, /Изучите/);
  assert.match(big.querySelector('.agtime').textContent, /\d\d:\d\d/, 'времени последней реплики не видно');
  assert.match(big.querySelector('.agmeta').textContent, /249 реплик/);
  assert.match(rowBy(doc, /Новый чат/).querySelector('.agmeta').textContent, /пока ни одной реплики/);
});

test('пустой список зовёт начать разговор, а не молчит пустотой', async () => {
  const { doc } = await boot({ agentChats: [] });
  await openAgent(doc);
  const empty = doc.querySelector('#agChats .agempty');
  assert.ok(empty, 'пустой список — просто пустота: ' + doc.getElementById('agChats').textContent);
  assert.match(empty.textContent, /Разговоров пока нет/);
});

/* Ширина и свёрнутость колонки живут в settings.json — там же, где тема. Своего
 * механизма памяти тут нет: панель и окно из трея читают одну книжку. */
test('свёрнутость и ширина колонки переживают перезапуск', async () => {
  const { doc, calls } = await boot({ agentChats: CHATS, settings: { agentSideWidth: 320, agentSideOff: false } });
  await openAgent(doc);
  const side = doc.getElementById('agChats');
  assert.equal(side.style.width, '320px', 'сохранённая ширина не применилась');
  assert.equal(side.classList.contains('off'), false);

  // свернули — и это уехало в настройки, а не осталось в голове у окна
  doc.querySelector('#agChats .agfold').dispatchEvent(click(doc));
  await settle();
  assert.ok(side.classList.contains('off'), 'колонка не свернулась');
  assert.equal(rows(doc).length, 0, 'свёрнутая колонка всё ещё показывает список');
  const saved = calls.filter((c) => c[0] === 'setSettings').map((c) => c[1]);
  assert.ok(saved.some((p) => p.agentSideOff === true && p.agentSideWidth === 320), 'свёрнутость не сохранена: ' + JSON.stringify(saved));

  // со свёрнутой колонкой имя открытого чата уходит в шапку — иначе непонятно,
  // куда уйдёт следующая реплика
  assert.match(doc.getElementById('agSub').textContent, /Джарвис/);
  // и дорога назад остаётся: рейка с двумя знаками, а не пустота
  assert.ok(doc.querySelector('#agChats .agfold'), 'развернуть колонку нечем');
  assert.ok(doc.querySelector('#agChats .agnew'), 'со свёрнутой колонкой нельзя завести чат');
});

/* Границу тянут мышью — этим и закрыт довод «список отнял треть окна»: место
 * делит человек, а не автор. Отпустил — ширина уехала в настройки. */
test('ширина колонки тянется мышью и сохраняется, когда кнопку отпустили', async () => {
  const { doc, window, calls } = await boot({ agentChats: CHATS, settings: { agentSideWidth: 240 } });
  await openAgent(doc);
  const side = doc.getElementById('agChats');
  const grip = side.querySelector('.aggrip');
  assert.ok(grip, 'границу колонки не за что взять: ' + side.innerHTML);

  const mouse = (name, x) => {
    const e = new window.Event(name, { bubbles: true });
    e.clientX = x;
    e.preventDefault = () => {};
    return e;
  };
  grip.dispatchEvent(mouse('mousedown', 240));
  doc.dispatchEvent(mouse('mousemove', 300));
  assert.equal(side.style.width, '300px', 'колонка не поехала за мышью');
  // и в потолок упирается, а не растягивается на всё окно
  doc.dispatchEvent(mouse('mousemove', 4000));
  assert.equal(side.style.width, '420px', 'колонка съела всю переписку');
  doc.dispatchEvent(mouse('mouseup', 4000));
  await settle();

  const saved = calls.filter((c) => c[0] === 'setSettings').map((c) => c[1]);
  assert.ok(saved.some((p) => p.agentSideWidth === 420), 'ширина не уехала в настройки: ' + JSON.stringify(saved));
  // мышь отпустили — тянуть перестали
  doc.dispatchEvent(mouse('mousemove', 200));
  assert.equal(side.style.width, '420px', 'колонка едет за мышью после отпускания');
});

test('свёрнутая колонка возвращается той же кнопкой', async () => {
  const { doc } = await boot({ agentChats: CHATS, settings: { agentSideOff: true } });
  await openAgent(doc);
  assert.equal(rows(doc).length, 0, 'сохранённая свёрнутость не применилась');
  doc.querySelector('#agChats .agfold').dispatchEvent(click(doc));
  await settle();
  assert.deepEqual(rowNames(doc), ['Джарвис', 'Выборы', 'Грант'], 'колонка не развернулась');
});

/* Своё имя чата (спека «имена чатов»): имя сильнее автозаголовка, правится из
 * строки и из меню действий, снимается пустым значением, а отказ — со словами. */

// Сессия, которой человек уже дал имя: title приходит с бэкенда уже собранным.
const NAMED = {
  ...SESSION,
  status: 'done',
  detail: '',
  name: 'БД',
  autoTitle: 'Fix the migration parser',
  title: 'БД',
};

async function withList(data) {
  const b = await boot(data);
  b.subs.onState(data.state);
  await new Promise((r) => setTimeout(r, 0));
  return b;
}

// Открыть правку через меню действий (⌘K) — тот же путь, что у человека.
function openRename(doc, re = /чату имя|Переименовать чат/) {
  doc.getElementById('actionsBtn').dispatchEvent(click(doc));
  const item = [...doc.querySelectorAll('#actionsPop .ap-item')].find((r) => re.test(r.textContent));
  assert.ok(item, 'пункта переименования нет в меню: ' + doc.getElementById('actionsPop').textContent);
  item.dispatchEvent(click(doc));
  return doc.querySelector('#list input.rename');
}

test('имя чата стоит в строке и не дублируется автозаголовком', async () => {
  const { doc } = await withList({ state: [NAMED] });
  const chip = doc.querySelector('#list .badge.chatname');
  assert.ok(chip, 'имени чата в строке нет');
  assert.equal(chip.textContent, 'БД');
  const summary = doc.querySelector('#list .summary').textContent;
  assert.ok(!summary.includes('Fix the migration'), 'автозаголовок повторяет имя: ' + summary);

  // безымянная сессия живёт как раньше — на автозаголовке
  const { doc: d2 } = await withList({ state: [{ ...SESSION, status: 'done', detail: '', title: 'Авто' }] });
  assert.equal(d2.querySelector('#list .badge.chatname'), null, 'чип появился без имени');
  assert.match(d2.querySelector('#list .summary').textContent, /Авто/);
});

test('имя чата ищется наравне с проектом', async () => {
  const { doc } = await withList({ state: [NAMED, { ...SESSION, id: 's2', project: 'другое' }] });
  const q = doc.getElementById('query');
  q.value = 'бд';
  q.dispatchEvent(new doc.defaultView.Event('input', { bubbles: true }));
  await new Promise((r) => setTimeout(r, 0));
  const rows = [...doc.querySelectorAll('#list .row')];
  assert.equal(rows.length, 1, 'по имени чата нашлось не то: ' + doc.getElementById('list').textContent);
  assert.equal(rows[0].dataset.sid, 's1');
});

test('правка имени уходит в демон по ↵ и не гасится пушем состояния', async () => {
  const { doc, calls, subs } = await withList({ state: [NAMED] });
  const inp = openRename(doc, /Переименовать чат/);
  assert.ok(inp, 'поле правки не открылось');
  assert.equal(inp.value, 'БД', 'в поле не подставлено текущее имя');

  // демон продолжает слать состояние — поле обязано пережить это
  subs.onState([NAMED]);
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(doc.querySelector('#list input.rename'), 'пуш состояния стёр поле правки');

  inp.value = 'Миграции';
  inp.dispatchEvent(key(doc, 'Enter'));
  await new Promise((r) => setTimeout(r, 0));
  const call = calls.find((c) => c[0] === 'renameSession');
  assert.ok(call, 'имя никуда не ушло');
  assert.deepEqual([call[1], call[2]], ['s1', 'Миграции']);
});

test('пустое значение снимает имя и возвращает автозаголовок', async () => {
  const { doc, calls } = await withList({ state: [NAMED] });
  const inp = openRename(doc, /Переименовать чат/);
  inp.value = '   ';
  inp.dispatchEvent(key(doc, 'Enter'));
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(calls.find((c) => c[0] === 'renameSession').slice(1), ['s1', '   ']);

  // и отдельным пунктом меню — без открытия поля
  const { doc: d2, calls: c2 } = await withList({ state: [NAMED] });
  d2.getElementById('actionsBtn').dispatchEvent(click(d2));
  const back = [...d2.querySelectorAll('#actionsPop .ap-item')].find((r) => /автозаголовок/.test(r.textContent));
  assert.ok(back, 'пункта возврата к автозаголовку нет у именованного чата');
  back.dispatchEvent(click(d2));
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(c2.find((c) => c[0] === 'renameSession').slice(1), ['s1', '']);
});

test('отказ переименования называет причину, а не молчит', async () => {
  const { doc } = await withList({
    state: [NAMED],
    rename: { ok: false, error: 'имя длиннее 60 символов (77) — сократи' },
  });
  const inp = openRename(doc, /Переименовать чат/);
  inp.value = 'очень длинное имя';
  inp.dispatchEvent(key(doc, 'Enter'));
  await new Promise((r) => setTimeout(r, 0));
  const toast = doc.querySelector('.toast');
  assert.ok(toast, 'отказ прошёл молча — ни слова человеку');
  assert.match(toast.textContent, /сократи/);
});

test('сессия завершается вторым нажатием, а не первым', async () => {
  const { doc, calls, subs } = await boot({ state: [SESSION] });
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

/* ============================================================================
   Различимость состояний. Продукт про то, чтобы боковым зрением видеть, кто
   ТЕБЯ ЖДЁТ. Всё, что ниже, держит одно свойство: «ждёт» отличимо от
   «работает» в любой краске и при любых настройках движения.
   ========================================================================= */

/** Тело правила по селектору: ищем в исходнике, а не в вычисленных стилях —
 *  linkedom их не считает, а разъезжаются как раз объявления. */
function ruleBody(css, selector) {
  const at = css.indexOf(selector + ' {');
  if (at < 0) return '';
  return css.slice(at, css.indexOf('}', at));
}

test('движение достаётся «ждёт», а не «работает»', () => {
  const html = read('index.html');
  const bad = [];
  // список
  if (/animation/.test(ruleBody(html, '.row.working .dot'))) bad.push('точка «работает» пульсирует — взгляд уходит на того, кто спокойно работает');
  if (!/animation/.test(ruleBody(html, '.row.waiting .dot'))) bad.push('точка «ждёт» не движется — молчит про того, кто встал');
  // чат: та же азбука
  if (/animation/.test(ruleBody(html, '.chatstatus::before'))) bad.push('полоса чата пульсирует на «работает»');
  if (!/animation/.test(ruleBody(html, '.chatstatus.waiting::before'))) bad.push('полоса чата не движется на «ждёт»');
  if (/animation/.test(ruleBody(html, '.chatdot.working'))) bad.push('точка чата пульсирует на «работает»');
  if (!/animation/.test(ruleBody(html, '.chatdot.waiting'))) bad.push('точка чата не движется на «ждёт»');
  assert.equal(bad.join(' | '), '');
});

/* «Уголь» — краска, где --accent равен --ink пиксель в пиксель. Плюс системное
 * «уменьшить движение» гасит анимацию. Если различие держалось только на них
 * двоих, два главных состояния сливались полностью. */
test('в краске «уголь» без движения «ждёт» и «работает» всё равно различимы', () => {
  const css = read('theme.css');
  const html = read('index.html');
  const coal = css.slice(css.indexOf("data-paint='coal'"), css.indexOf("data-paint='raspberry'"));
  const val = (name) => (coal.match(new RegExp(`\\${name}:\\s*([^;]+);`)) || [])[1];
  // предпосылка теста: краска и правда одна на оба состояния
  assert.equal(val('--ink'), val('--accent'), 'уголь перестал быть одной краской — тест пора переписать');

  const working = ruleBody(html, '.row.working .dot');
  const waiting = ruleBody(html, '.row.waiting .dot');
  // различие формой: гало у «ждёт», ничего похожего у «работает»
  assert.ok(/box-shadow/.test(waiting), 'у «ждёт» нет гало: ' + waiting);
  assert.ok(!/box-shadow/.test(working), 'гало появилось и у «работает» — различия снова нет: ' + working);
  // …и оно переживает выключенное движение: там гасят только анимацию
  const reduced = [...html.matchAll(/@media \(prefers-reduced-motion: reduce\) \{/g)]
    .map((m) => html.slice(m.index, html.indexOf('\n  }', m.index)));
  assert.ok(reduced.length >= 2, 'блоков уменьшенного движения стало меньше — тест смотрит не туда');
  const paints = reduced.filter((b) => /box-shadow|background/.test(b));
  assert.equal(paints.join(' | '), '', 'уменьшенное движение трогает не только анимацию — состояния сольются');
  assert.ok(reduced.some((b) => /\.row\.waiting \.dot/.test(b)), 'пульс «ждёт» не гасится при уменьшенном движении');
});

/* Упавшая сессия получает от демона Done с пометкой в detail и выглядела ровно
 * как успешно закончившая. Кольцо остаётся кольцом — меняется обводка. */
test('упавшая сессия не выдаёт себя за успешно закончившую', async () => {
  const dead = { ...SESSION, status: 'done', detail: 'сессия остановлена', name: '', title: '' };
  const { doc } = await withList({ state: [dead] });
  const row = doc.querySelector('#list .row');
  assert.ok(row.className.includes('failed'), 'у упавшей сессии нет пометки: ' + row.className);
  assert.equal(doc.querySelector('#list .dot').getAttribute('aria-label'), 'упала');
  // а обычная завершённая — по-прежнему просто done
  const { doc: d2 } = await withList({ state: [{ ...SESSION, status: 'done', detail: '' }] });
  assert.ok(!d2.querySelector('#list .row').className.includes('failed'), 'здоровую сессию записали в упавшие');
});

/* Многоточие режет КОНЕЦ строки. Раньше состояние стояло последним, и
 * отваливалось ровно то, ради чего в список и смотрят; у «готово» и «лимита»
 * оно не показывалось вообще — контекст есть почти всегда и побеждал. */
test('состояние переживает обрезку строки и видно у всех статусов', async () => {
  const long = 'переписываю разбор транскрипта, чтобы ходы не склеивались в один блок на длинной сессии';
  const bad = [];
  for (const [status, detail, word] of [
    ['working', '', 'работает'],
    ['waiting', '', 'ждёт тебя'],
    ['done', '', 'готово'],
    ['limit', '', 'лимит — ждёт сброса'],
  ]) {
    const { doc } = await withList({ state: [{ ...SESSION, status, detail, task: long, name: '', title: '' }] });
    const text = doc.querySelector('#list .summary').textContent;
    if (!text.startsWith(word)) bad.push(`${status}: состояние не впереди — «${text}»`);
    if (!text.includes(long)) bad.push(`${status}: контекст потерян — «${text}»`);
    // обрезанное саммари должно быть где дочитать
    if (doc.querySelector('#list .summary').title !== text) bad.push(`${status}: саммари негде дочитать`);
  }
  assert.equal(bad.join(' | '), '');
});

/* ============================================================================
   Клавиатура.
   ========================================================================= */

/* Гашение клавиш через stopPropagation в поле бесполезно: главный обработчик
 * висит на window в фазе ПЕРЕХВАТА и отрабатывает раньше. Esc уходил в
 * hidePanel(), ↵ — в openChat(), и только потом коммитилось имя. */
test('Esc в поле правки имени закрывает правку, а не всю панель', async () => {
  const { doc, calls } = await withList({ state: [NAMED] });
  const inp = openRename(doc, /Переименовать чат/);
  assert.ok(inp, 'поле правки не открылось');
  inp.dispatchEvent(key(doc, 'Escape'));
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(!calls.some((c) => c[0] === 'hidePanel'), 'Esc из поля спрятал всю панель');
  assert.equal(doc.querySelectorAll('#list input.rename').length, 0, 'правка не закрылась');
});

test('↵ в поле правки имени коммитит имя, а не проваливается в чат', async () => {
  const { doc, calls } = await withList({ state: [NAMED] });
  const inp = openRename(doc, /Переименовать чат/);
  inp.value = 'Миграции';
  inp.dispatchEvent(key(doc, 'Enter'));
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(calls.some((c) => c[0] === 'renameSession'), 'имя не ушло демону');
  assert.ok(!calls.some((c) => c[0] === 'openChat'), '↵ провалился в чат мимо правки имени');
});

/* Главный сценарий: хоткей → ↵ по сессии → варианты открылись сами → ↵ по
 * варианту. openVarPanel сам разфокусировал поле ответа, а закрытие фокус не
 * возвращало — дописать реплику можно было только мышью. */
test('после ответа вариантом клавиатура возвращается в поле ответа', async () => {
  const asked = {
    ...SESSION,
    question: { questions: [{ question: 'Чем чинить?', options: [{ label: 'форма' }, { label: 'краска' }] }] },
  };
  const { doc } = await withList({ state: [asked] });
  doc.querySelector('#list .row').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 10));
  // варианты поднимаются сами — это и есть главный сценарий
  assert.equal(doc.getElementById('varBtn').hidden, false, 'кнопки вариантов нет у сессии с вопросом');
  assert.equal(doc.getElementById('qWrap').hidden, false, 'слайд-овер не открылся сам');

  const reply = doc.getElementById('reply');
  let focused = 0;
  reply.focus = () => { focused += 1; };
  reply.blur = () => {};

  // закрытие — общий путь у Esc, крестика и finalizeQ
  doc.getElementById('qpClose').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(doc.getElementById('qWrap').hidden, true, 'слайд-овер не закрылся');
  assert.equal(focused, 1, 'фокус не вернулся в поле ответа — дальше только мышью');
  // и открыть его снова есть чем без мыши
  assert.match(doc.getElementById('varBtn').title, /O$/, 'у кнопки вариантов нет клавиши: ' + doc.getElementById('varBtn').title);
});

/* ⌘6 и ⌘7 были зашиты на metaKey, тогда как семь соседних веток спрашивают
 * isMod («⌘ на маке, Ctrl на Linux»). На Linux вкладки были мертвы, а keys.js
 * честно рисовал на них Ctrl+6/Ctrl+7. Вкладки «Циклы» и «Связка» в тесте не
 * открываем — они поднимают свои модули с опросом демона; ветки проверяем по
 * исходнику, а живость самого isMod — на соседней вкладке без таймеров. */
test('⌘6 и ⌘7 идут через isMod, как остальные вкладки', async () => {
  const renderer = read('renderer.js');
  const hardwired = [...renderer.matchAll(/e\.metaKey && e\.key === '\d'/g)].map((m) => m[0]);
  assert.equal(hardwired.join(' | '), '', 'ветка вкладки зашита на ⌘ мимо isMod — на Linux она мертва');
  // все семь цифр вкладок спрашивают именно isMod
  const guards = [...renderer.matchAll(/if \((.{0,24})e\.key === '(\d)'\)/g)].map(([, pre, d]) => `${d}:${/isMod\(e\)/.test(pre)}`);
  assert.equal(guards.join(' '), '9:true 0:true 1:true 2:true 3:true 4:true 5:true 6:true 7:true 8:true');

  // и isMod действительно решает: один модификатор, тот, что главный на этой ОС
  const { doc, window } = await boot();
  doc.getElementById('tabAgent').dispatchEvent(click(doc));
  const e = key(doc, '1');
  e[window.jarvisKeys.isMac ? 'metaKey' : 'ctrlKey'] = true;
  doc.defaultView.dispatchEvent(e);
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(doc.getElementById('list').hidden, false, 'главный модификатор ОС не сработал');
});
