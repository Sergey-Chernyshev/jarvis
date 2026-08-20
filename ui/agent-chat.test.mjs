/* Окно чата с агентом в настоящем DOM.
 *
 * Ловит то, ради чего окно переделано: id разговора жил в переменной внутри
 * окна — закрыл окно, и следующая реплика молча уходила в НОВУЮ сессию Claude.
 * Плюс честный отказ: `--resume` на пропавший транскрипт раньше приезжал пустым
 * «ответом», и окно делало вид, что агент промолчал.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const HERE = new URL('./', import.meta.url);
const read = (name) => readFileSync(new URL(name, HERE), 'utf8');

const tick = () => new Promise((r) => setTimeout(r, 0));

/* Окно живёт вне общего моста и зовёт Tauri напрямую — подменяем invoke/listen. */
async function boot(state = { sessionId: null }, replies = {}) {
  const { window, document } = parseHTML(read('agent-chat.html'));
  const calls = [];
  const handlers = {};
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        calls.push([cmd, args]);
        if (cmd === 'agent_chat_state') return state;
        if (replies[cmd]) return replies[cmd]();
        return { ok: true };
      },
    },
    event: {
      listen: async (name, cb) => { handlers[name] = cb; return () => {}; },
      // Своё же вещание возвращается и в это окно — как на настоящей шине Tauri.
      emit: async (name, payload) => { if (handlers[name]) await handlers[name]({ payload }); },
    },
  };
  // Скрипты окна в том же порядке, что в agent-chat.html: маркдаун реплик
  // общий с панелью и грузится до чата.
  for (const name of ['markdown.js', 'agent-chat.js']) {
    new Function('window', 'document', 'globalThis', read(name))(window, document, window);
  }
  await tick();
  const emit = async (payload) => { await handlers['agent:event']({ payload }); await tick(); };
  const say = async (text) => {
    document.getElementById('input').value = text;
    document.getElementById('send').dispatchEvent(new window.Event('click', { bubbles: true }));
    await tick();
  };
  const sent = () => calls.filter(([c]) => c === 'agent_send').map(([, a]) => a);
  const hit = (el) => { el.dispatchEvent(new window.Event('click', { bubbles: true })); return tick().then(tick); };
  const fire = async (name, payload) => { await handlers[name]({ payload }); await tick(); };
  return { window, doc: document, calls, emit, say, sent, hit, fire };
}

const text = (doc) => doc.getElementById('msgs').textContent;

test('после перезапуска окна разговор продолжается, а не начинается заново', async () => {
  const { doc, say, sent } = await boot({ sessionId: 's-42' });

  assert.equal(doc.getElementById('tag').hidden, false, 'нет метки продолжения — человек гадает');
  assert.match(text(doc), /Продолжаю прошлый разговор/);

  await say('привет');
  assert.deepEqual(sent(), [{ message: 'привет', chatId: null, sessionId: 's-42' }]);
});

test('без сохранённого разговора окно молчит про продолжение', async () => {
  const { doc, say, sent } = await boot({ sessionId: null });
  assert.equal(doc.getElementById('tag').hidden, true);
  assert.doesNotMatch(text(doc), /Продолжаю прошлый разговор/);
  await say('привет');
  assert.deepEqual(sent(), [{ message: 'привет', chatId: null, sessionId: null }]);
});

test('«Новый чат» сбрасывает id и на демоне, и в окне', async () => {
  const { window, doc, calls, say, sent } = await boot({ sessionId: 's-42' });
  await say('первое');

  doc.getElementById('newChat').dispatchEvent(new window.Event('click', { bubbles: true }));
  await tick();

  assert.ok(calls.some(([c]) => c === 'agent_chat_reset'), 'демон не узнал про сброс — id воскреснет');
  assert.equal(doc.getElementById('tag').hidden, true);
  assert.doesNotMatch(text(doc), /первое/, 'лента не очищена');

  await say('второе');
  assert.equal(sent()[1].sessionId, null, 'новый чат ушёл в старую сессию');
});

test('неудачный сброс не притворяется удавшимся', async () => {
  const { window, doc, say, sent } = await boot(
    { sessionId: 's-42' },
    { agent_chat_reset: () => { throw new Error('диск только для чтения'); } },
  );
  doc.getElementById('newChat').dispatchEvent(new window.Event('click', { bubbles: true }));
  await tick();

  assert.match(text(doc), /Не удалось начать новый чат/);
  assert.equal(doc.getElementById('tag').hidden, false, 'метка снята, хотя id остался');
  await say('дальше');
  assert.equal(sent()[0].sessionId, 's-42', 'сброс провалился, а окно ушло в новую сессию');
});

test('недоступная сессия названа вслух, а не проглочена', async () => {
  const { doc, emit, say, sent } = await boot({ sessionId: 's-42' });
  await say('привет');
  assert.equal(doc.getElementById('send').disabled, true, 'ожидание ответа не показано');

  await emit({
    type: 'failed',
    message: 'No conversation found with session ID: s-42',
    lost_session: true,
  });

  assert.match(text(doc), /No conversation found/, 'причина отказа не показана');
  assert.match(text(doc), /не удалён/, 'не сказано, что прошлый разговор цел');
  assert.equal(doc.getElementById('send').disabled, false, 'окно осталось в «думает…»');
  assert.equal(doc.getElementById('tag').hidden, true, 'метка продолжения врёт про мёртвую сессию');

  await say('ещё раз');
  assert.equal(sent()[1].sessionId, null, 'вторая попытка снова уедет в пропавший транскрипт');
});

test('обычный отказ агента не стирает нить разговора', async () => {
  const { doc, emit, say, sent } = await boot({ sessionId: 's-42' });
  await say('привет');
  await emit({ type: 'failed', message: 'API Error: 529 overloaded', lost_session: false });

  assert.match(text(doc), /529 overloaded/);
  assert.equal(doc.getElementById('send').disabled, false);
  assert.equal(doc.getElementById('tag').hidden, false, 'сеть моргнула — разговор терять не за что');
  await say('ещё раз');
  assert.equal(sent()[1].sessionId, 's-42');
});

/* История: окно и вкладка панели поднимаются одним mount(), поэтому лента
 * прошлых реплик обязана появиться и здесь — иначе «продолжение» снова
 * выглядит как потерянный разговор. */
test('окно рисует прошлую переписку, а не только метку продолжения', async () => {
  const { doc } = await boot({ sessionId: 's-42' }, {
    agent_chat_history: () => ({
      ok: true,
      sessionId: 's-42',
      total: 200,
      items: [
        { role: 'user', kind: 'text', text: 'что с лимитом', ts: 1 },
        { role: 'assistant', kind: 'tool', text: 'limit_get', ts: 2 },
        { role: 'assistant', kind: 'text', text: 'осталось 40%', ts: 3 },
      ],
    }),
  });
  const msgs = doc.getElementById('msgs');
  assert.match(msgs.textContent, /что с лимитом/);
  assert.match(msgs.textContent, /осталось 40%/);
  assert.ok(msgs.querySelector('.msg.tools .chip'), 'тул-вызов нарисован обычным текстом');
  // хвост отдан не целиком — об этом надо сказать, а не делать вид, что всё
  assert.match(msgs.textContent, /Показаны последние 3 реплики из 200/);
});

/* Маркдаун ответа: рендерер общий с чатом сессии (markdown.js). Пока он жил в
 * renderer.js, окно из трея показывало решётки заголовков и звёздочки списков —
 * ровно тот текст, который агент считает разметкой. */
test('ответ агента размечен, а не показан сырым текстом', async () => {
  const { doc, emit, say } = await boot();
  await say('расскажи');
  await emit({ type: 'delta', text: '# Итог\n\n- первый **пункт**\n- второй `код`\n\n```\nls -la\n```\n' });
  await emit({ type: 'done', result: '', session_id: 's-1' });

  const b = doc.querySelector('#msgs .msg.assistant .bubble');
  assert.equal(b.querySelector('.md-h').textContent, 'Итог', 'заголовок остался абзацем с решёткой');
  assert.equal(b.querySelectorAll('ul li').length, 2, 'список не собрался');
  assert.equal(b.querySelector('li strong').textContent, 'пункт');
  assert.equal(b.querySelector('li code').textContent, 'код');
  assert.equal(b.querySelector('pre').textContent, 'ls -la', 'фенс не стал блоком кода');
  assert.doesNotMatch(b.textContent, /```|\*\*/, 'разметка утекла в текст');
});

test('окно подключает общий markdown.js', () => {
  assert.match(read('agent-chat.html'), /<script src="\.\/markdown\.js">/);
});

/* Несколько разговоров: окно из трея показывает ПОЛНЫЙ список, а не только
 * открытый чат. Когда панель закрыта, это окно — единственный вход, и «только
 * текущий» отрезал бы от остальных проектов. */

const BOOK = (current = 'c1') => ({
  ok: true,
  current,
  chats: [
    { id: 'c1', name: 'Джарвис', sessionId: 's-1', current: current === 'c1' },
    { id: 'c2', name: 'Выборы', sessionId: 's-2', current: current === 'c2' },
  ],
});

const rowsOf = (doc) => [...doc.querySelectorAll('#chats .agchat:not(.add)')];
// история свёрнута — раскрываем её так же, как человек
const openHist = (doc, window) =>
  doc.querySelector('#chats .agtoggle').dispatchEvent(new window.Event('click', { bubbles: true }));

test('окно из трея показывает всю историю и переключает разговор', async () => {
  const { window, doc, calls, hit, say, sent } = await boot({ sessionId: 's-1' }, {
    agent_chats_list: () => BOOK(),
    agent_chat_switch: () => BOOK('c2'),
  });
  openHist(doc, window);
  assert.deepEqual(rowsOf(doc).map((c) => c.querySelector('.agname').textContent), ['Джарвис', 'Выборы']);

  await hit(rowsOf(doc).find((c) => /Выборы/.test(c.textContent)));
  const asked = calls.filter(([c]) => c === 'agent_chat_history').map(([, a]) => a.chatId);
  assert.deepEqual(asked, ['c1', 'c2'], 'история спрошена не про выбранный чат');

  // и реплика уходит в нить именно этого чата, а не в прежнюю
  await say('привет');
  assert.equal(sent()[0].sessionId, 's-2', 'сообщение уехало в соседний разговор');
});

test('окно не предлагает удалить последний чат', async () => {
  const { window, doc } = await boot({ sessionId: null }, {
    agent_chats_list: () => ({ ok: true, current: 'c1', chats: [{ id: 'c1', name: 'Джарвис', sessionId: null, current: true }] }),
  });
  openHist(doc, window);
  assert.equal(doc.querySelectorAll('#chats .agx').length, 0, 'крестик обещает то, что демон запретил');
  assert.equal(doc.querySelectorAll('#chats .agchat.add').length, 1, 'завести чат нечем');
});

test('отказ на создание чата виден и в окне из трея', async () => {
  const { doc, hit } = await boot({ sessionId: null }, {
    agent_chats_list: () => BOOK(),
    agent_chat_create: () => ({ ok: false, error: 'пустое имя — у чата должно быть название' }),
  });
  await hit(doc.querySelector('#chats .agchat.add'));
  assert.match(text(doc), /должно быть название/, 'отказ съеден молча: ' + text(doc));
});

/* Разговор, найденный в транскриптах: чата за ним нет, и без строки в истории
 * он недостижим. Окно из трея — единственный вход, когда панель закрыта, и
 * отказ здесь обязан быть таким же громким. */
const DISK_BOOK = {
  ok: true,
  current: 'c1',
  chats: [
    { id: 'c1', name: 'Джарвис', named: true, sessionId: 's-1', current: true, turns: 4, at: Date.now(), preview: 'привет' },
    { id: null, name: 'Изучите текущие сессии', named: false, sessionId: 's-249', current: false, turns: 249, at: Date.now(), preview: 'Изучите текущие сессии' },
  ],
};

test('отказ на открытие разговора с диска виден и в окне из трея', async () => {
  const { window, doc, hit } = await boot({ sessionId: 's-1' }, {
    agent_chats_list: () => DISK_BOOK,
    agent_chat_open: () => ({ ok: false, error: 'разговора s-249 нет на диске — открыть его не получится' }),
  });
  openHist(doc, window);
  const disk = doc.querySelector('#chats .agchat.disk');
  assert.ok(disk, 'разговора с диска нет в истории окна');
  await hit(disk);
  assert.match(text(doc), /нет на диске/, 'отказ съеден молча: ' + text(doc));
});

test('переименование в окне из трея находится кнопкой и уезжает демону', async () => {
  const { window, doc, calls, hit } = await boot({ sessionId: 's-1' }, {
    agent_chats_list: () => DISK_BOOK,
    agent_chat_rename: () => ({ ok: true, current: 'c1', chats: [{ ...DISK_BOOK.chats[0], name: 'Главный', named: true }] }),
  });
  openHist(doc, window);
  const ed = rowsOf(doc)[0].querySelector('.agedit');
  assert.ok(ed, 'переименование негде найти: ' + rowsOf(doc)[0].innerHTML);
  await hit(ed);
  const inp = doc.querySelector('#chats input.agrename');
  assert.ok(inp, 'правка имени не открылась');
  inp.value = 'Главный';
  const e = new window.Event('keydown', { bubbles: true });
  e.key = 'Enter';
  inp.dispatchEvent(e);
  await tick();
  const asked = calls.filter(([c]) => c === 'agent_chat_rename').map(([, a]) => a);
  assert.deepEqual(asked, [{ chatId: 'c1', name: 'Главный' }], 'имя не уехало демону');
  assert.equal(doc.querySelector('#chats .agcur').textContent, 'Главный');
});

/* Карточка подтверждения — про права: человек решает, случится ли побочный
 * эффект, и обязан понимать, на что соглашается и что вышло. */

const ASK = (over = {}) => ({
  nonce: 'n-1', id: 'sessions.control', class: 'effect',
  card: { kind: 'session', label: 'Выборы', model: 'opus', effort: 'high' },
  ...over,
});
const cbox = (doc) => doc.querySelector('.msg.confirm .cbox');

test('карточка называет действие словами, а не внутренним именем команды', async () => {
  const { doc, fire } = await boot();
  await fire('agent:confirm', ASK());
  const box = cbox(doc);
  assert.match(box.querySelector('.ctitle').textContent, /сменить модель сессии/);
  assert.doesNotMatch(box.textContent, /sessions\.control/, 'id команды человеку ничего не говорит');
  assert.match(box.querySelector('.cdesc').textContent, /усилие high/);
  assert.doesNotMatch(box.textContent, /effort/, 'английское слово посреди русской фразы');
});

test('незнакомая команда показывает аргументы словами, а не JSON в лицо', async () => {
  const { doc, fire } = await boot();
  await fire('agent:confirm', ASK({ id: 'limits.raise', card: { kind: 'other', args: { session_id: 's-1', minutes: 30 } } }));
  const box = cbox(doc);
  assert.match(box.querySelector('.ctitle').textContent, /limits\.raise/, 'что за команда — не сказано вовсе');
  const desc = box.querySelector('.cdesc').textContent;
  assert.match(desc, /session_id: s-1/);
  assert.match(desc, /minutes: 30/);
  assert.doesNotMatch(desc, /[{}"]/, 'JSON в лицо: ' + desc);
});

test('правка настроек показывает, что из чего станет', async () => {
  const { doc, fire } = await boot();
  await fire('agent:confirm', ASK({ id: 'settings.set', card: { kind: 'settings', diff: { theme: { from: 'dark', to: 'light' } } } }));
  assert.match(cbox(doc).querySelector('.cdesc').textContent, /theme: dark → light/);
});

/* Главное враньё: гейт истёк, пока карточка ждала, agent_confirm вернул
 * {ok:false} — а человеку писали «✓ разрешено» за действие, которого не было. */
test('«Разрешить» по истёкшему вопросу не превращается в «разрешено»', async () => {
  const { doc, hit, fire } = await boot({ sessionId: null }, { agent_confirm: () => ({ ok: false }) });
  await fire('agent:confirm', ASK());
  const box = cbox(doc);
  await hit(box.querySelector('.cbtn.yes'));

  assert.equal(box.querySelectorAll('.cbtn').length, 0, 'кнопки живы у решённого вопроса');
  assert.doesNotMatch(box.textContent, /разрешено/, 'обещано разрешение, которого демон не принял');
  assert.match(box.textContent, /не выполнено/);
});

test('все четыре исхода демона названы своими словами', async () => {
  const cases = [
    ['approved', /разрешено/, /не выполнено|отклонено/],
    ['rejected', /отклонено/, /разрешено/],
    ['expired', /время вышло[\s\S]*не выполнено/, /отклонено|✓/],
    ['stale', /цель изменилась[\s\S]*не выполнено/, /отклонено|✓/],
  ];
  for (const [outcome, want, nope] of cases) {
    const { doc, hit, fire } = await boot();
    await fire('agent:confirm', ASK());
    const box = cbox(doc);
    await hit(box.querySelector('.cbtn.yes'));
    // до слова демона карточка ничего не обещает
    assert.doesNotMatch(box.textContent, /разрешено|отклонено/, outcome + ': исход объявлен раньше демона');
    await fire('agent:confirm-done', { nonce: 'n-1', approved: outcome === 'approved', outcome });
    assert.match(box.textContent, want, outcome);
    assert.doesNotMatch(box.textContent, nope, outcome + ': сказано лишнее');
  }
});

test('исход, о котором это окно не знает, не выдаётся за согласие', async () => {
  const { doc, hit, fire } = await boot();
  await fire('agent:confirm', ASK());
  const box = cbox(doc);
  await hit(box.querySelector('.cbtn.yes'));
  await fire('agent:confirm-done', { nonce: 'n-1', approved: false, outcome: 'denied-by-policy' });
  assert.doesNotMatch(box.textContent, /✓/, 'неизвестный исход показан как разрешение');
  assert.match(box.textContent, /неизвестно/);
});

/* Вечное «думает…»: agent_send отвечает отказом РАЗРЕШЁННЫМ промисом (нет ни
 * claude, ни codex; не прочитан jarvis-mcp.json; чат удалили в соседнем окне),
 * а события done/failed уже не будет. */
test('отказ на отправку снимает ожидание, а не вешает поле навсегда', async () => {
  const { doc, say } = await boot({ sessionId: null }, {
    agent_send: () => ({ ok: false, error: 'Нет ни claude, ни codex — агент недоступен' }),
  });
  await say('привет');
  assert.match(text(doc), /Нет ни claude/, 'отказ проглочен молча');
  assert.equal(doc.getElementById('send').disabled, false, 'поле ввода заблокировано навсегда');
  await say('ещё раз');
  assert.equal(doc.getElementById('sub').textContent, 'готов');
});

test('исключение в отправке показано человеческой строкой, а не стеком', async () => {
  const { doc, say } = await boot({ sessionId: null }, {
    agent_send: () => { throw { code: 'EPIPE' }; }, // не Error: у Tauri бывает объект
  });
  await say('привет');
  assert.doesNotMatch(text(doc), /object Object/, 'в ленту утёк сырой эксепшен');
  assert.equal(doc.getElementById('send').disabled, false);
});

/* Стрим: полная перерисовка пузыря на каждой дельте уносила выделение (из
 * растущего ответа нельзя было скопировать ни строчки) и захлопывала Insight. */
test('стрим дописывает хвост, а не пересобирает готовую часть', async () => {
  const { doc, emit, say } = await boot();
  await say('давай');
  await emit({ type: 'delta', text: 'первый абзац\n\n' });
  const p = doc.querySelector('.msg.assistant .bubble p');
  p.dataset.keep = '1'; // метка переживёт дельту, только если узел не пересобран

  await emit({ type: 'delta', text: 'второй абзац' });
  const bubble = doc.querySelector('.msg.assistant .bubble');
  assert.equal(bubble.querySelectorAll('p').length, 2);
  assert.equal(bubble.querySelector('p').dataset.keep, '1', 'готовая часть перерисована на каждой дельте');

  await emit({ type: 'done', result: '', session_id: 's-1' });
  assert.equal(bubble.querySelector('p').dataset.keep, '1', 'финал ответа пересобрал пузырь');
  assert.equal(bubble.querySelectorAll('p').length, 2, 'хвост потерялся: ' + bubble.textContent);
});

test('ответ, оборванный на фенсе, дорисовывается кодом, а не пропадает', async () => {
  const { doc, emit, say } = await boot();
  await say('покажи');
  await emit({ type: 'delta', text: 'вот:\n\n```\ncargo test\n' });
  await emit({ type: 'done', result: '', session_id: 's-1' });
  assert.match(doc.querySelector('.msg.assistant .bubble pre').textContent, /cargo test/);
});

/* Разговор с главным агентом переживает закрытие окна: за пару суток активной
 * переписки в ленте накопились бы тысячи узлов. */
test('лента чата с агентом не растёт бесконечно', async () => {
  const items = Array.from({ length: 500 }, (_, i) => ({ role: 'user', kind: 'text', text: 'реплика ' + i, ts: i }));
  const { doc } = await boot({ sessionId: null }, {
    agent_chat_history: () => ({ ok: true, total: 500, items }),
  });
  const msgs = doc.getElementById('msgs');
  assert.ok(msgs.childElementCount <= 400, 'потолка у ленты нет: ' + msgs.childElementCount);
  assert.match(msgs.textContent, /реплика 499/, 'унесли хвост вместо начала');
  assert.doesNotMatch(msgs.textContent, /реплика 0\b/, 'начало ленты осталось');
});

test('живой вопрос не уносится подрезкой ленты', async () => {
  const { doc, fire, emit } = await boot();
  await fire('agent:confirm', ASK()); // вопрос в самом начале ленты
  for (let i = 0; i < 210; i++) {
    await emit({ type: 'tool_use', name: 'limit_get' }); // чип рвёт пузырь надвое
    await emit({ type: 'delta', text: 'ответ ' + i });
  }
  const msgs = doc.getElementById('msgs');
  assert.ok(msgs.childElementCount <= 401, 'потолок снят живой карточкой: ' + msgs.childElementCount);
  assert.ok(cbox(doc), 'вопрос унесло подрезкой вместе с кнопками');
  assert.equal(cbox(doc).querySelectorAll('.cbtn').length, 2, 'выбор у живого вопроса пропал');
});

test('свежий id из потока перекрывает восстановленный', async () => {
  const { emit, say, sent } = await boot({ sessionId: null });
  await say('привет');
  await emit({ type: 'init', tools: [], model: 'claude-sonnet-4-5', session_id: 's-new' });
  await emit({ type: 'done', result: 'ок', session_id: 's-new' });
  await say('второе');
  assert.equal(sent()[1].sessionId, 's-new');
});
