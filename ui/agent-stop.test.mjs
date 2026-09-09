/* Остановка хода: «Esc останавливает работу Джарвиса».
 *
 * Стережёт ровно то, из-за чего остановка бывает на вид: Esc, гасящий индикатор
 * мимо ядра; стёртый вместе с ходом недописанный ответ; общий стоп на все чаты
 * разом; молчащая цепочка, которая через минуту заводит следующий заход; и
 * дочерние CLI, убитые за компанию с ходом, за который они не в ответе.
 *
 * Сравниваем длины и примитивы: провалившийся assert над узлом linkedom
 * печатается минутами и выглядит зависанием.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const HERE = new URL('./', import.meta.url);
const read = (name) => readFileSync(new URL(name, HERE), 'utf8');
const tick = () => new Promise((r) => setTimeout(r, 0));

/* Два разговора: остановка обязана быть про один из них. Ход идёт в обоих —
 * иначе «остановился только мой» доказать нечем. */
const BOOK = (current = 'c1') => ({
  ok: true,
  current,
  hidden: 0,
  chats: [
    { id: 'c1', name: 'Джарвис', named: true, sessionId: 's-1', current: current === 'c1', turns: 1, at: 1000 },
    { id: 'c2', name: 'Выборы', named: true, sessionId: 's-2', current: current === 'c2', turns: 1, at: 2000 },
  ],
});

async function boot(replies = {}) {
  const { window, document } = parseHTML(read('agent-chat.html'));
  const calls = [];
  const handlers = {};
  window.innerWidth = 0;
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        calls.push([cmd, args]);
        if (cmd === 'agent_chat_state') return { sessionId: 's-1' };
        if (cmd === 'agent_chats_list') return BOOK();
        if (cmd === 'agent_chat_switch') return BOOK((args || {}).chatId);
        if (cmd === 'agent_chat_history') return { ok: true, items: [], total: 0 };
        if (cmd === 'agent_send') return { ok: true, chatId: (args || {}).chatId };
        if (replies[cmd]) return replies[cmd](args || {});
        return { ok: true };
      },
    },
    event: {
      listen: async (name, cb) => { handlers[name] = cb; return () => {}; },
      emit: async (name, payload) => { if (handlers[name]) await handlers[name]({ payload }); },
    },
  };
  for (const name of ['markdown.js', 'agent-chat.js']) {
    new Function('window', 'document', 'globalThis', read(name))(window, document, window);
  }
  await tick();
  const emit = async (payload) => { await handlers['agent:event']({ payload }); await tick(); };
  const hit = (el) => { el.dispatchEvent(new window.Event('click', { bubbles: true })); return tick().then(tick); };
  const esc = async () => {
    const e = new window.Event('keydown', { bubbles: true });
    e.key = 'Escape';
    e.preventDefault = () => {};
    document.dispatchEvent(e);
    await tick(); await tick(); await tick();
  };
  const say = async (text) => {
    document.getElementById('input').value = text;
    document.getElementById('send').dispatchEvent(new window.Event('click', { bubbles: true }));
    await tick(); await tick();
  };
  const rows = () => [...document.querySelectorAll('#chats .agchat')];
  const goTo = (re) => hit(rows().find((r) => re.test(r.textContent)));
  return {
    window,
    doc: document,
    calls,
    emit,
    hit,
    esc,
    say,
    goTo,
    rows,
    did: (cmd) => calls.filter(([c]) => c === cmd).map(([, a]) => a),
    text: () => document.getElementById('msgs').textContent,
    stopBtn: () => document.querySelector('.agstop'),
  };
}

/* Ход, идущий прямо сейчас, с половиной ответа в ленте. */
const started = async (s, chatId) => {
  await s.emit({ type: 'delta', text: 'первая половина ответа', chatId });
};

test('Esc посреди хода зовёт остановку ровно один раз и ровно для своего чата', async () => {
  const s = await boot({ agent_stop: ({ chatId }) => ({ ok: true, stopped: true, chatId, children: [] }) });
  await started(s, 'c1');
  await started(s, 'c2');
  await s.goTo(/Выборы/); // открыт второй, отвечают оба

  await s.esc();
  await s.esc(); // второй Esc по тому же ходу — не вторая остановка

  assert.deepEqual(s.did('agent_stop'), [{ chatId: 'c2' }], 'остановка ушла не за свой чат или задвоилась');
  // соседний разговор продолжает писать: поток разведён по chatId
  const busy = [...s.doc.querySelectorAll('#chats .agchat.busy .agname')].map((n) => n.textContent);
  assert.deepEqual(busy, ['Джарвис'], 'остановка одного чата задела соседний');
});

test('пришедшее остаётся в ленте и помечено «остановлено вами»', async () => {
  const s = await boot({ agent_stop: ({ chatId }) => ({ ok: true, stopped: true, chatId, children: [] }) });
  await started(s, 'c1');
  await s.esc();

  assert.match(s.text(), /первая половина ответа/, 'недописанный ответ стёрли вместе с ходом');
  const mark = s.doc.querySelector('#msgs .msg.assistant .stopmark');
  assert.ok(mark, 'оборванный ответ ничем не отличить от полного: ' + s.text());
  assert.match(mark.textContent, /остановлено вами/);
  assert.equal(s.doc.getElementById('send').disabled, false, 'после остановки поле осталось запертым');
  assert.equal(s.doc.getElementById('sub').textContent, 'готов', '«думает…» пережило остановку');
});

test('остановленный ход не дорисовывается вторым пузырём из финального done', async () => {
  const s = await boot({ agent_stop: ({ chatId }) => ({ ok: true, stopped: true, chatId, children: [] }) });
  await started(s, 'c1');
  await s.esc();
  await s.emit({ type: 'done', result: 'весь ответ целиком', session_id: 's-1', chatId: 'c1' });

  assert.equal(s.doc.querySelectorAll('#msgs .msg.assistant').length, 1, 'у остановленного хода вырос второй ответ');
  assert.doesNotMatch(s.text(), /весь ответ целиком/, 'ядро дописало ход, который человек прервал');
});

test('Esc без хода не притворяется остановкой, но меню закрывает', async () => {
  const s = await boot();
  const row = s.rows()[0];
  row.querySelector('.agdots').dispatchEvent(new s.window.Event('click', { bubbles: true }));
  await tick();
  assert.equal(s.doc.querySelectorAll('#chats .agmi').length > 0, true, 'меню не открылось — проверять нечего');

  await s.esc();
  assert.equal(s.doc.querySelectorAll('#chats .agmi').length, 0, 'Esc перестал закрывать меню строки');
  await s.esc(); // теперь и меню нет, и хода нет
  assert.deepEqual(s.did('agent_stop'), [], 'Esc вхолостую сходил за остановкой');
  assert.doesNotMatch(s.text(), /останов/i, 'лента объявила остановку там, где ничего не шло');
});

test('«stopped: false» не выдаётся за остановку', async () => {
  const s = await boot({ agent_stop: ({ chatId }) => ({ ok: true, stopped: false, chatId, children: [], note: 'Останавливать было нечего — ход не шёл' }) });
  await started(s, 'c1');
  await s.esc();

  assert.equal(s.doc.querySelectorAll('#msgs .stopmark').length, 0, 'пометка встала за ход, которого не было');
  assert.doesNotMatch(s.text(), /остановлено вами/i, 'остановка объявлена вопреки ядру');
  assert.equal(s.doc.getElementById('send').disabled, false, 'занятость висит, хотя ход не идёт');
});

test('кнопка «стоп» делает то же, что клавиша, и живёт только во время хода', async () => {
  const s = await boot({ agent_stop: ({ chatId }) => ({ ok: true, stopped: true, chatId, children: [] }) });
  assert.equal(s.stopBtn().hidden, true, 'кнопка обещает остановку, когда останавливать нечего');

  await started(s, 'c1');
  assert.equal(s.stopBtn().hidden, false, 'мышью ход не остановить: кнопки нет');
  await s.hit(s.stopBtn());

  assert.deepEqual(s.did('agent_stop'), [{ chatId: 'c1' }], 'кнопка и клавиша ведут в разные двери');
  assert.equal(s.doc.querySelectorAll('#msgs .stopmark').length, 1, 'кнопка остановила молча');
  assert.equal(s.stopBtn().hidden, true, 'кнопка осталась после конца хода');
});

/* Дочерние CLI переживают остановку: там своя оплаченная работа. Но молчать про
 * них нельзя — человек нажал «стоп» и должен видеть, где ещё горят деньги. */
const KIDS = [{ id: 's-9', name: 'Рефактор ядра', agent: 'claude' }];

test('дочерние сессии показаны, сами не закрываются и закрываются в два нажатия', async () => {
  const s = await boot({ agent_stop: ({ chatId }) => ({ ok: true, stopped: true, chatId, children: KIDS, note: 'Остановлено вами; продолжают работу: Рефактор ядра (claude)' }) });
  await started(s, 'c1');
  await s.esc();

  assert.match(s.text(), /Рефактор ядра/, 'про живую дочернюю сессию промолчали: ' + s.text());
  assert.match(s.text(), /продолжают работу/, 'не сказано, что сессии живы');
  assert.deepEqual(s.did('session_kill'), [], 'дочернюю сессию закрыли за компанию с ходом');

  const btn = [...s.doc.querySelectorAll('#msgs .stoprow .agbtn')].find((b) => /Закрыть/.test(b.textContent));
  assert.ok(btn, 'закрыть дочернюю сессию нечем: ' + s.text());
  await s.hit(btn);
  assert.deepEqual(s.did('session_kill'), [], 'одно нажатие — и чужая работа оборвана');
  assert.match(btn.textContent, /Точно/, 'второе нажатие ничего не переспрашивает');

  await s.hit(btn);
  assert.deepEqual(s.did('session_kill'), [{ sessionId: 's-9' }], 'сессия не закрылась и со второго нажатия');
  assert.match(s.text(), /закрыта/, 'закрыли — а строка про это молчит');
});

/* Цепочку рвёт ядро тем же agent_stop. Молчать об этом нельзя: остановленный ход
 * иначе сменился бы следующим, а человек не понял бы, почему больше не сменяется. */
const CHAIN = (over) => ({ ok: true, state: { chatId: 'c1', active: true, mode: 'auto', step: 2, phase: 'watching', ...over } });

test('строка «цепочка остановлена» приходит со способом продолжить', async () => {
  const s = await boot({
    agent_stop: ({ chatId }) => ({ ok: true, stopped: true, chatId, children: [] }),
    agent_chain_state: () => CHAIN(),
  });
  await started(s, 'c1');
  await s.esc();

  assert.match(s.text(), /Цепочка остановлена/, 'про оборванную цепочку промолчали: ' + s.text());
  const go = [...s.doc.querySelectorAll('#msgs .stoprow .agbtn')].find((b) => /Продолжить/.test(b.textContent));
  assert.ok(go, 'вернуть авто-продолжение нечем: ' + s.text());

  await s.hit(go);
  assert.deepEqual(s.did('agent_chain_mode'), [{ chatId: 'c1', auto: true }], 'кнопка не вернула цепочку');
  assert.match(s.text(), /снова продолжает сама/, 'вернули — а лента об этом молчит');
});

test('цепочки не было — про неё и не говорим', async () => {
  const s = await boot({
    agent_stop: ({ chatId }) => ({ ok: true, stopped: true, chatId, children: [] }),
    agent_chain_state: () => CHAIN({ active: false, mode: 'ask', phase: 'stopped' }),
  });
  await started(s, 'c1');
  await s.esc();

  assert.doesNotMatch(s.text(), /Цепочка остановлена/, 'остановили цепочку, которой не было');
  assert.equal(s.doc.querySelectorAll('#msgs .stoprow').length, 0, 'в ленте действие без повода');
});

/* Ход могли остановить из соседнего окна или с кнопки трея — лента обязана
 * сказать то же самое, а не ждать, пока человек догадается по замолчавшему
 * пузырю. Событие помечено чатом, как весь остальной поток. */
test('событие stopped из соседнего окна помечает ленту и называет сессии', async () => {
  const s = await boot();
  await started(s, 'c1');
  await s.emit({ type: 'stopped', chatId: 'c1', by: 'user', children: KIDS });

  assert.equal(s.doc.querySelectorAll('#msgs .stopmark').length, 1, 'остановка из другого окна прошла незамеченной');
  assert.match(s.text(), /первая половина ответа/, 'пришедшее стёрли по событию');
  assert.match(s.text(), /Рефактор ядра/, 'про живые дочерние сессии промолчали');
  assert.equal(s.doc.getElementById('send').disabled, false, 'занятость пережила остановку');
  assert.deepEqual(s.did('agent_stop'), [], 'на своё же событие окно сходило в ядро ещё раз');
});

test('своя остановка и её событие не пишутся в ленту дважды', async () => {
  const s = await boot({ agent_stop: ({ chatId }) => ({ ok: true, stopped: true, chatId, children: KIDS }) });
  await started(s, 'c1');
  await s.emit({ type: 'stopped', chatId: 'c1', by: 'user', children: KIDS }); // ядро шлёт его первым
  await s.esc();

  assert.equal(s.doc.querySelectorAll('#msgs .stopmark').length, 1, 'один ход помечен остановленным дважды');
  assert.equal(s.doc.querySelectorAll('#msgs .stoprow').length, 1, 'список дочерних сессий задвоился');
});

test('отказ на остановку виден, а не проглочен', async () => {
  const s = await boot({ agent_stop: () => ({ ok: false, error: 'чата c1 нет в списке — обнови список' }) });
  await started(s, 'c1');
  await s.esc();

  assert.match(s.text(), /нет в списке/, 'отказ съеден молча: ' + s.text());
  assert.equal(s.doc.querySelectorAll('#msgs .stopmark').length, 0, 'ход помечен остановленным вопреки отказу');
});
