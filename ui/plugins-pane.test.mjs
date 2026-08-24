/* Вкладка «Плагины»: всё рисуется из манифеста, ничего — из знания о плагине.
 *
 * Смысл теста не в «нарисовалась строчка», а в инварианте INV-KERNEL
 * (спека 2026-08-19-everything-is-plugin-design.md §2.2): UI не должен знать
 * ни одного идентификатора плагина. Поэтому здесь подсовывается ВЫДУМАННЫЙ
 * плагин, которого в коде нет и быть не может, — и вкладка обязана показать
 * его настройки, права и команды так же, как показала бы наши.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const HERE = new URL('./', import.meta.url);
const read = (name) => readFileSync(new URL(name, HERE), 'utf8');

const EXTERNAL = {
  id: 'выдуманный-плагин',
  name: 'Выдуманный',
  version: '2.3.1',
  description: 'Плагина с таким id в коде Jarvis нет',
  kind: 'external',
  builtin: false,
  enabled: true,
  pane: null,
  tray: true,
  status: { что_угодно: 1 },
  health: { status: 'running', error: null, pid: 4242, restarts: 2, uptimeMs: 125000 },
  settingsSchema: [
    { key: 'громкость', type: 'segmented', title: 'Громкость', default: 'тихо',
      options: [{ value: 'тихо', label: 'Тихо' }, { value: 'громко', label: 'Громко' }] },
    { key: 'порог', type: 'number', title: 'Порог', default: 7, min: 1, max: 9 },
    { key: 'подробности', type: 'toggle', title: 'Подробности', default: false,
      depends: 'нетакогоключа' },
  ],
  settingsValues: { 'громкость': 'громко', 'порог': 7 },
  commands: [{ name: 'бахнуть', title: 'Бахнуть' }, { name: 'молча' }],
  capabilities: ['read', 'control'],
  uses: ['notify.toast'],
};

const BUILTIN = {
  id: 'встроенный-плагин',
  name: 'Встроенный',
  version: '1.0.0',
  description: 'Наш код в нашем процессе',
  kind: 'builtin',
  builtin: true,
  enabled: false,
  pane: 'awake',
  tray: true,
  status: null,
  health: { status: 'stopped', error: null, pid: null, restarts: 0, uptimeMs: 0 },
  settingsSchema: [{ key: 'авто', type: 'toggle', title: 'Авто', default: false }],
  settingsValues: { 'авто': false },
  commands: [],
  capabilities: [],
  uses: [],
};

/* Настройки в изоляции: грузим только settings2.js и зовём initSettings2. */
async function openPlugins(plugins) {
  const { window, document } = parseHTML('<!doctype html><html><body><div id="root"></div></body></html>');
  const calls = [];
  window.jarvis = new Proxy({}, {
    get(_t, prop) {
      if (typeof prop !== 'string') return undefined;
      if (/^on[A-Z]/.test(prop)) return () => () => {};
      if (prop === 'getPlugins') return async () => { calls.push(['getPlugins']); return plugins; };
      return (...args) => { calls.push([prop, ...args]); return Promise.resolve({ ok: true }); };
    },
  });
  window.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
  const memory = new Map();
  window.localStorage = {
    getItem: (k) => (memory.has(k) ? memory.get(k) : null),
    setItem: (k, v) => memory.set(k, String(v)),
    removeItem: (k) => memory.delete(k),
    clear: () => memory.clear(),
  };
  const fn = new Function(
    'window', 'document', 'globalThis', 'localStorage', 'navigator',
    'setTimeout', 'clearTimeout', 'setInterval', 'clearInterval',
    read('settings2.js')
  );
  fn(window, document, window, window.localStorage, { platform: 'Linux x86_64' },
     setTimeout, clearTimeout, () => 0, () => {});
  window.initSettings2(document.getElementById('root'));
  // переключаемся на вкладку «Плагины» так же, как это делает человек
  const nav = [...document.querySelectorAll('.sidebar .nav, .sidebar *')]
    .find((n) => n.textContent === 'Плагины');
  assert.ok(nav, 'в сайдбаре нет вкладки «Плагины»');
  nav.dispatchEvent(new window.Event('click', { bubbles: true }));
  await new Promise((r) => setTimeout(r, 0));
  await new Promise((r) => setTimeout(r, 0));
  const pane = document.getElementById('s2-pane-plugins');
  assert.ok(pane, 'панели вкладки «Плагины» нет');
  return { window, document, pane, calls };
}

test('вкладка рисует незнакомый плагин целиком из его манифеста', async () => {
  const { pane } = await openPlugins([EXTERNAL]);
  const text = pane.textContent;
  assert.match(text, /Выдуманный/, 'имя из манифеста не отрисовалось: ' + text);
  assert.match(text, /2\.3\.1/, 'версии нет');
  assert.match(text, /внешний/, 'рантайм не показан');
  assert.match(text, /работает/, 'статус не показан');
  assert.match(text, /Громкость/, 'поле схемы не отрисовалось');
  assert.match(text, /Порог/, 'числовое поле не отрисовалось');
  assert.match(text, /notify\.toast/, 'права не показаны');
  assert.match(text, /Бахнуть/, 'кнопки команды нет');
  assert.doesNotMatch(text, /молча/, 'команда без подписи не для человека');
  assert.match(text, /pid 4242/, 'здоровье внешнего плагина не показано');
});

test('поле с невыполненным depends спрятано', async () => {
  const { pane } = await openPlugins([EXTERNAL]);
  assert.doesNotMatch(pane.textContent, /Подробности/);
});

test('тумблер зовёт _enable у ядра, а не что-то плагино-специфичное', async () => {
  const { window, pane, calls } = await openPlugins([EXTERNAL]);
  const t = pane.querySelector('input.toggle');
  assert.ok(t, 'тумблера нет');
  t.checked = false;
  t.dispatchEvent(new window.Event('change', { bubbles: true }));
  const call = calls.find((c) => c[0] === 'pluginCmd');
  assert.deepEqual(call, ['pluginCmd', 'выдуманный-плагин', '_enable', { on: false }]);
});

test('правка настройки уходит в plugin_set по ключу схемы', async () => {
  const { window, pane, calls } = await openPlugins([EXTERNAL]);
  const num = pane.querySelector('input[type=number]');
  assert.ok(num, 'числового поля нет');
  num.value = '3';
  num.dispatchEvent(new window.Event('change', { bubbles: true }));
  const call = calls.find((c) => c[0] === 'pluginSet');
  assert.deepEqual(call, ['pluginSet', 'выдуманный-плагин', 'порог', 3]);
});

test('встроенный плагин честно отличается от внешнего', async () => {
  const { pane } = await openPlugins([BUILTIN]);
  const text = pane.textContent;
  assert.match(text, /встроенный/, 'рантайм не назван');
  assert.doesNotMatch(text, /Здоровье/, 'у встроенного нет ни pid, ни падений');
  // настройки живут в своей вкладке — вкладка про это говорит, а не дублирует
  assert.match(text, /Бодрость/, 'не сказано, где искать настройки');
  assert.doesNotMatch(text, /Авто/, 'настройка продублирована в двух вкладках');
});

test('пустой список плагинов не ломает вкладку', async () => {
  const { pane } = await openPlugins([]);
  assert.match(pane.textContent, /Пусто/);
});
