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
  'markdown.js',
  'diffview.js',
  'question-answer.js',
  'settings2.js',
  'voice-history.js',
  'loops.js',
  'bundle.js',
  'changes.js',
  'search.js',
  'renderer.js',
];

/* Мост-заглушка. Любой неизвестный метод возвращает разумную пустоту, иначе
 * тест превратился бы в список из сотни моков и ломался бы от каждой新ой
 * команды. Вызовы записываем — по ним и проверяем, что кнопки делают дело. */
function makeBridge(calls, data = {}) {
  const subs = {};
  const target = {
    getState: async () => data.state || [],
    getSettings: async () => ({}),
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
      'requestAnimationFrame', 'cancelAnimationFrame',
      read(name)
    );
    fn(
      window, document, window, window.localStorage, window.navigator || { platform: 'MacIntel' },
      // Периодику панели глушим: она нужна живому окну, а тест иначе никогда
      // не закончится — процесс держат её таймеры.
      setTimeout, clearTimeout, () => 0, () => {},
      window.requestAnimationFrame, window.cancelAnimationFrame
    );
  }
  // Дать промисам старта (getState/getSettings/getMeta) отработать.
  await new Promise((r) => setTimeout(r, 0));
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
