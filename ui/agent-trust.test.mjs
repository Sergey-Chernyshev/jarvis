/* Доверие агент-чату: тумблер «писать в сессии без подтверждения».
 *
 * Настройка ценна ровно тем, куда она пишется: гейт в демоне читает
 * grants.agent.autoApprove и сверяет id капабилити поимённо. Промахнись тумблер
 * ключом или формой — он молча ничего не разрешит (или, хуже, разрешит не то).
 * Поэтому тест кликает по настоящему тумблеру и смотрит на патч настроек. */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const read = (name) => readFileSync(new URL(name, import.meta.url), 'utf8');

/** Панель настроек в живом DOM; calls — что уехало в мост. */
async function boot(settings = {}) {
  const { window, document } = parseHTML('<html><body><div id="root"></div></body></html>');
  const calls = [];
  window.jarvis = {
    getSettings: async () => settings,
    getMeta: async () => ({ version: 'test' }),
    hotkeyBindings: async () => ({ ok: true, bindings: [] }),
    agentsList: async () => ({ ok: true, agents: [], presets: [] }),
    setSettings: (patch) => { calls.push(['setSettings', patch]); return Promise.resolve({ ok: true }); },
  };
  for (const name of ['keys.js', 'settings2.js']) {
    const fn = new Function('window', 'document', 'globalThis', 'setTimeout', 'clearTimeout', read(name));
    fn(window, document, window, setTimeout, clearTimeout);
  }
  window.initSettings2(document.getElementById('root'));
  // открыть вкладку «Агенты» и дать её асинхронному рендеру доработать
  document.querySelector('.snav .item[data-pane="agents"]')
    .dispatchEvent(new window.Event('click', { bubbles: true }));
  await new Promise((r) => setTimeout(r, 0));
  return { document, window, calls };
}

/** Тумблер строки с этим заголовком. */
function toggleFor(document, title) {
  const row = [...document.querySelectorAll('#s2-pane-agents .drow')]
    .find((r) => (r.querySelector('.dt') || {}).textContent === title);
  assert.ok(row, `строки «${title}» нет во вкладке «Агенты»`);
  return row.querySelector('input.toggle');
}

const TITLE = 'Писать в сессии без подтверждения';

test('тумблер пишет id капабилити в grants.agent.autoApprove', async () => {
  const { document, window, calls } = await boot({});
  const t = toggleFor(document, TITLE);
  assert.equal(t.checked, false, 'по умолчанию выключен — гейт спрашивает');
  t.checked = true;
  t.dispatchEvent(new window.Event('change', { bubbles: true }));
  const patch = (calls.find(([c]) => c === 'setSettings') || [])[1];
  assert.deepEqual(patch, { grants: { agent: { autoApprove: ['sessions.reply'] } } });
});

test('выключение убирает id, не трогая соседей по списку', async () => {
  const settings = { grants: { agent: { autoApprove: ['sessions.reply', 'tasks.get'] } } };
  const { document, window, calls } = await boot(settings);
  const t = toggleFor(document, TITLE);
  assert.equal(t.checked, true, 'настройка из settings.json не отражена в тумблере');
  t.checked = false;
  t.dispatchEvent(new window.Event('change', { bubbles: true }));
  const patch = (calls.find(([c]) => c === 'setSettings') || [])[1];
  assert.deepEqual(patch, { grants: { agent: { autoApprove: ['tasks.get'] } } });
});

test('строка честно называет риск, а не «ускоряет работу»', async () => {
  const { document } = await boot({});
  const row = [...document.querySelectorAll('#s2-pane-agents .drow')]
    .find((r) => (r.querySelector('.dt') || {}).textContent === TITLE);
  const desc = row.querySelector('.dd').textContent;
  assert.match(desc, /Риск/, 'риск не назван');
  assert.match(desc, /файл/, 'не сказано, куда уйдёт прочитанный агентом текст');
});
