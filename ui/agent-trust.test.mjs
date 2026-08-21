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
async function boot(settings = {}, reply = { ok: true }) {
  const { window, document } = parseHTML('<html><body><div id="root"></div></body></html>');
  const calls = [];
  window.jarvis = {
    getSettings: async () => settings,
    getMeta: async () => ({ version: 'test' }),
    hotkeyBindings: async () => ({ ok: true, bindings: [] }),
    agentsList: async () => ({ ok: true, agents: [], presets: [] }),
    setSettings: (patch) => { calls.push(['setSettings', patch]); return Promise.resolve(reply); },
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

/* ── Подъём сессий без спроса ──────────────────────────────────────────── */

const SPAWN = 'Поднимать сессии без подтверждения';
const settle = async () => { for (let i = 0; i < 8; i++) await new Promise((r) => setTimeout(r, 0)); };
const patches = (calls) => calls.filter(([c]) => c === 'setSettings').map(([, p]) => p);

test('тумблер подъёма сессий добавляет sessions.spawn рядом с уже разрешённым', async () => {
  const { document, window, calls } = await boot({ grants: { agent: { autoApprove: ['sessions.reply'] } } });
  const t = toggleFor(document, SPAWN);
  assert.equal(t.checked, false, 'по умолчанию подъём со спросом');
  t.checked = true;
  t.dispatchEvent(new window.Event('change', { bubbles: true }));
  assert.deepEqual(patches(calls)[0],
    { grants: { agent: { autoApprove: ['sessions.reply', 'sessions.spawn'] } } });
});

test('тумблер подъёма отражает настройки и выключается обратно', async () => {
  const { document, window, calls } = await boot({ grants: { agent: { autoApprove: ['sessions.spawn'] } } });
  const t = toggleFor(document, SPAWN);
  assert.equal(t.checked, true, 'разрешение из settings.json не отражено');
  t.checked = false;
  t.dispatchEvent(new window.Event('change', { bubbles: true }));
  assert.deepEqual(patches(calls)[0], { grants: { agent: { autoApprove: [] } } });
});

/* Список — весь, а не только то, на что есть тумблер: id, дописанный в файл
 * руками, иначе оставался бы невидимым разрешением. */
test('список показывает всё разрешённое, включая id без тумблера', async () => {
  const { document } = await boot({ grants: { agent: { autoApprove: ['sessions.reply', 'tasks.get'] } } });
  const rows = [...document.querySelectorAll('#s2-pane-agents .s2trust-list .drow')];
  const titles = rows.map((r) => (r.querySelector('.dt') || {}).textContent);
  assert.equal(titles.includes('Писать в сессии'), true, 'разрешённое не показано: ' + titles.join(' | '));
  assert.equal(titles.includes('tasks.get'), true, 'id из файла не виден в списке');
  const text = document.querySelector('#s2-pane-agents .s2trust-list').textContent;
  assert.match(text, /правкой settings\.json/, 'про происхождение чужого id не сказано');
});

test('пустой список говорит словами, а не пустотой', async () => {
  const { document } = await boot({});
  const text = document.querySelector('#s2-pane-agents .s2trust-list').textContent;
  assert.match(text, /Пока ничего/, 'пустой список молчит');
});

test('снятие разрешения — одно нажатие «Убрать»', async () => {
  const { document, window, calls } = await boot({
    grants: { agent: { autoApprove: ['sessions.spawn', 'sessions.reply'] } },
  });
  const row = [...document.querySelectorAll('#s2-pane-agents .s2trust-list .drow')]
    .find((r) => (r.querySelector('.dt') || {}).textContent === 'Поднимать новые сессии');
  assert.ok(row, 'строки sessions.spawn нет в списке');
  const btn = row.querySelector('button.btn');
  assert.ok(btn, 'снять разрешение нечем');
  btn.dispatchEvent(new window.Event('click', { bubbles: true }));
  assert.deepEqual(patches(calls)[0], { grants: { agent: { autoApprove: ['sessions.reply'] } } });
});

/* Ключ grants закрыт гейтом, и отказ записи здесь — не абстракция: тумблер,
 * который молча остался включённым, врёт про права до самого перезапуска. */
test('отказ записи виден человеку, а тумблер возвращается назад', async () => {
  const reason = 'Права не сохранены: нет доступа к ~/.jarvis/settings.json';
  const { document, window } = await boot({}, { ok: false, error: reason });
  const t = toggleFor(document, SPAWN);
  t.checked = true;
  t.dispatchEvent(new window.Event('change', { bubbles: true }));
  await settle();

  const err = document.querySelector('#s2-pane-agents .s2trust-err');
  assert.ok(err, 'места под отказ нет');
  assert.equal(err.style.display, '', 'плашка отказа спрятана');
  assert.equal(err.textContent.includes(reason), true, 'причина отказа проглочена: ' + err.textContent);
  assert.equal(t.checked, false, 'тумблер остался включённым, хотя права не записались');
});

/* Разрешение без спроса — не разрешение молча: ограничители названы поимённо.
 * Потолка на ЧИСЛО сессий среди них больше нет — его убрали целиком, и обещать
 * несуществующий отказ хуже, чем молчать. Остался расход. */
test('рядом с разрешением перечислено то, что остаётся в силе', async () => {
  const { document } = await boot({});
  const text = document.querySelector('#s2-pane-agents').textContent;
  assert.doesNotMatch(text, /sessionsSpawnMax|Потолок одновременных/,
    'потолок числа сессий вернулся в настройки');
  assert.match(text, /не ограничено/, 'про то, что сессий может быть сколько угодно, не сказано');
  assert.match(text, /бюджет и лестница порогов/i, 'про бюджет и пороги не сказано');
  assert.match(text, /ночной потолок расхода/, 'про ночные правила не сказано');
  assert.match(text, /аудит/, 'про запись в лог не сказано');
});

/* sessions.close уже в SELF_LIMITED гейта: отдельный тумблер там не нужен, но
 * молчать нельзя — иначе «поднимать можно, закрывать нет» читается как мусор. */
test('закрытие своих дочерних сессий показано как разрешённое всегда', async () => {
  const { document } = await boot({});
  const titles = [...document.querySelectorAll('#s2-pane-agents .drow .dt')].map((n) => n.textContent);
  assert.equal(titles.filter((t) => /Закрывать свои дочерние сессии/.test(t)).length, 1,
    'про sessions.close не сказано: ' + titles.join(' | '));
  const row = [...document.querySelectorAll('#s2-pane-agents .drow')]
    .find((r) => /Закрывать свои дочерние сессии/.test((r.querySelector('.dt') || {}).textContent || ''));
  assert.equal(row.querySelectorAll('input.toggle').length, 0, 'у sessions.close завёлся лишний тумблер');
  assert.match(row.querySelector('.dd').textContent, /поднял сам/, 'причина «всегда» не объяснена');
});
