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
async function boot(state = { sessionId: null }, replies = {}, width = 0) {
  const { window, document } = parseHTML(read('agent-chat.html'));
  const calls = [];
  const handlers = {};
  /* Ширину окна linkedom не знает; тест называет её сам — от неё зависит,
   * теснит колонка переписку или кроет её. Ставим ВСЕГДА: окна linkedom делят
   * это поле, и 460 от соседнего теста утекли бы в следующий. */
  window.innerWidth = width || 0;
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        calls.push([cmd, args]);
        if (cmd === 'agent_chat_state') return state;
        // Заглушке отдаём аргументы: с несколькими чатами ответ зависит от того,
        // про какой спросили, — иначе тест не отличит свою ленту от соседней.
        if (replies[cmd]) return replies[cmd](args || {});
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

const rowsOf = (doc) => [...doc.querySelectorAll('#chats .agchat')];
/* Редкие действия строки лежат под «…» — там же, куда ведёт и правый клик.
 * Три иконки в ряд обещали три равные кнопки, хотя одна из них необратима. */
const menuOf = async (s, row) => {
  row.querySelector('.agdots').dispatchEvent(new s.window.Event('click', { bubbles: true }));
  await tick();
  return [...s.doc.querySelectorAll('#chats .agmi')];
};
const pick = (items, re) => items.find((i) => re.test(i.textContent));

test('окно из трея показывает всю историю и переключает разговор', async () => {
  const s = await boot({ sessionId: 's-1' }, {
    agent_chats_list: () => BOOK(),
    agent_chat_switch: () => BOOK('c2'),
  });
  const { doc, calls, hit, say, sent } = s;
  // колонка видна сразу: раскрывать нечего, в этом вся затея
  assert.deepEqual(rowsOf(doc).map((c) => c.querySelector('.agname').textContent), ['Джарвис', 'Выборы']);

  await hit(rowsOf(doc).find((c) => /Выборы/.test(c.textContent)));
  const asked = calls.filter(([c]) => c === 'agent_chat_history').map(([, a]) => a.chatId);
  assert.deepEqual(asked, ['c1', 'c2'], 'история спрошена не про выбранный чат');

  // и реплика уходит в нить именно этого чата, а не в прежнюю
  await say('привет');
  assert.equal(sent()[0].sessionId, 's-2', 'сообщение уехало в соседний разговор');
});

test('окно не предлагает убрать последний чат', async () => {
  const s = await boot({ sessionId: null }, {
    agent_chats_list: () => ({ ok: true, current: 'c1', chats: [{ id: 'c1', name: 'Джарвис', sessionId: null, current: true }] }),
  });
  const items = await menuOf(s, rowsOf(s.doc)[0]);
  assert.equal(items.filter((i) => /Скрыть/.test(i.textContent)).length, 0, 'меню обещает то, что демон запретил');
  assert.equal(s.doc.querySelectorAll('#chats .agnew').length, 1, 'завести чат нечем');
});

test('отказ на создание чата виден и в окне из трея', async () => {
  const { doc, hit } = await boot({ sessionId: null }, {
    agent_chats_list: () => BOOK(),
    agent_chat_create: () => ({ ok: false, error: 'пустое имя — у чата должно быть название' }),
  });
  await hit(doc.querySelector('#chats .agnew'));
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
  const { doc, hit } = await boot({ sessionId: 's-1' }, {
    agent_chats_list: () => DISK_BOOK,
    agent_chat_open: () => ({ ok: false, error: 'разговора s-249 нет на диске — открыть его не получится' }),
  });
  const disk = doc.querySelector('#chats .agchat.disk');
  assert.ok(disk, 'разговора с диска нет в истории окна');
  await hit(disk);
  assert.match(text(doc), /нет на диске/, 'отказ съеден молча: ' + text(doc));
});

// Тот же список после скрытия и после забвения: строки нет в обоих случаях, а
// разница — в `hidden`. Файл цел ровно тогда, когда его есть куда вернуть.
const ONLY_CHAT = (hidden) => ({ ok: true, current: 'c1', hidden, chats: [DISK_BOOK.chats[0]] });

test('«Скрыть» у разговора с диска прячет его, а транскрипт не трогает', async () => {
  const s = await boot({ sessionId: 's-1' }, {
    agent_chats_list: () => DISK_BOOK,
    agent_history_hide: () => ONLY_CHAT(1),
  });
  const { doc, calls, hit } = s;
  const hide = pick(await menuOf(s, doc.querySelector('#chats .agchat.disk')), /Скрыть/);
  assert.ok(hide, 'строку с диска нечем убрать: ' + doc.getElementById('chats').textContent);
  await hit(hide);

  assert.deepEqual(calls.filter(([c]) => c === 'agent_history_hide').map(([, a]) => a), [{ sessionId: 's-249' }]);
  assert.equal(calls.filter(([c]) => c === 'agent_history_forget').length, 0, 'скрытие обернулось удалением файла');
  assert.equal(doc.querySelectorAll('#chats .agchat.disk').length, 0, 'спрятанная строка осталась в списке');
  assert.match(doc.querySelector('#chats .aghidden').textContent, /Скрыто 1 · вернуть/, 'скрытие не оставило следа');
});

/* Забвение — единственное необратимое действие окна, и спрашивает оно видимой
 * кнопкой: невидимую комбинацию нельзя обнаружить, а стереть 249 реплик по
 * незнанию можно ровно один раз. */
test('перед забвением окно спрашивает и называет, что исчезнет', async () => {
  const s = await boot({ sessionId: 's-1' }, {
    agent_chats_list: () => DISK_BOOK,
    agent_history_forget: () => ONLY_CHAT(0),
  });
  const { doc, calls, hit } = s;
  const items = await menuOf(s, doc.querySelector('#chats .agchat.disk'));
  // три действия строки — и необратимое среди них названо словом, а не знаком
  assert.deepEqual(items.map((i) => i.textContent), ['Открыть', 'Скрыть', 'Забыть насовсем']);
  const f = pick(items, /Забыть насовсем/);
  assert.ok(f.classList.contains('danger'), 'необратимое неотличимо от обратимых соседей');
  await hit(f);

  const ask = doc.querySelector('#chats .agask');
  assert.ok(ask, 'разговор удалили без вопроса: ' + doc.getElementById('chats').textContent);
  assert.match(ask.textContent, /Изучите текущие сессии/, 'вопрос не назвал разговор: ' + ask.textContent);
  assert.match(ask.textContent, /249 реплик/, 'вопрос не сказал, сколько теряется: ' + ask.textContent);
  assert.equal(calls.filter(([c]) => c === 'agent_history_forget').length, 0, 'спросили — и удалили, не дождавшись ответа');

  await hit(ask.querySelector('.agbtn.danger'));
  assert.deepEqual(calls.filter(([c]) => c === 'agent_history_forget').map(([, a]) => a), [{ sessionId: 's-249' }]);
  assert.equal(doc.querySelectorAll('#chats .agchat.disk').length, 0, 'забытый разговор остался в истории');
  assert.equal(doc.querySelectorAll('#chats .aghidden').length, 0, 'файла нет, а вернуть его всё ещё предлагают');
});

test('отказ на забвение виден в окне из трея, а строка остаётся на месте', async () => {
  const s = await boot({ sessionId: 's-1' }, {
    agent_chats_list: () => DISK_BOOK,
    agent_history_forget: () => ({ ok: false, error: 'по разговору s-249 прямо сейчас идёт ход — дождись ответа агента' }),
  });
  const { doc, hit } = s;
  await hit(pick(await menuOf(s, doc.querySelector('#chats .agchat.disk')), /Забыть/));
  await hit(doc.querySelector('#chats .agask .agbtn.danger'));

  assert.match(text(doc), /дождись ответа агента/, 'отказ съеден молча: ' + text(doc));
  assert.ok(doc.querySelector('#chats .agchat.disk'), 'разговор цел, а строка пропала');
});

test('переименование в окне из трея находится в меню строки и уезжает демону', async () => {
  const s = await boot({ sessionId: 's-1' }, {
    agent_chats_list: () => DISK_BOOK,
    agent_chat_rename: () => ({ ok: true, current: 'c1', chats: [{ ...DISK_BOOK.chats[0], name: 'Главный', named: true }] }),
  });
  const { window, doc, calls, hit } = s;
  const ed = pick(await menuOf(s, rowsOf(doc)[0]), /Переименовать/);
  assert.ok(ed, 'переименование негде найти: ' + doc.getElementById('chats').textContent);
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
  assert.equal(rowsOf(doc)[0].querySelector('.agname').textContent, 'Главный');
});

/* Узкое окно — это как раз окно из трея (~460px). Постоянная колонка тут не
 * влезает: 260 на список и 200 на переписку — две нечитаемые полосы вместо
 * разговора. Поэтому колонка здесь выдвижная: свёрнута до рейки, разворачивается
 * поверх переписки и уходит сразу после выбора. */
test('в узком окне колонка свёрнута и кроет переписку, а не теснит её', async () => {
  const s = await boot({ sessionId: 's-1' }, {
    agent_chats_list: () => BOOK(),
    agent_chat_switch: () => BOOK('c2'),
  }, 460);
  const { doc, hit } = s;
  const side = doc.getElementById('chats');
  assert.ok(side.classList.contains('off'), 'в узком окне колонка заняла треть экрана');
  assert.equal(rowsOf(doc).length, 0, 'свёрнутая колонка всё ещё показывает список');
  // но дорога к чатам осталась, и открытый чат назван в шапке
  assert.ok(side.querySelector('.agfold'), 'развернуть колонку нечем');
  assert.match(doc.getElementById('sub').textContent, /Джарвис/, 'непонятно, куда уйдёт следующая реплика');

  await hit(side.querySelector('.agfold'));
  assert.deepEqual(rowsOf(doc).map((c) => c.querySelector('.agname').textContent), ['Джарвис', 'Выборы']);
  assert.ok(side.parentElement.classList.contains('narrow'), 'колонка делит окно пополам вместо того, чтобы крыть его');

  // выбрали разговор — колонка своё отработала и не заслоняет переписку
  await hit(rowsOf(doc).find((c) => /Выборы/.test(c.textContent)));
  assert.equal(rowsOf(doc).length, 0, 'выдвижная колонка осталась поверх переписки');
  // и тесноту в настройки не записываем: это не выбор человека
  assert.equal(s.calls.filter(([c]) => c === 'settings_set').length, 0, 'ширину узкого окна записали как решение');
});

/* ⌘\ — тот же жест, что сворачивает боковую колонку в редакторах. */
test('колонка сворачивается с клавиатуры и запоминается', async () => {
  const s = await boot({ sessionId: 's-1' }, { agent_chats_list: () => BOOK() });
  const { window, doc, calls } = s;
  const fold = () => {
    const e = new window.Event('keydown', { bubbles: true });
    e.key = '\\';
    e.metaKey = true;
    e.preventDefault = () => {};
    doc.dispatchEvent(e);
  };
  fold();
  await tick();
  assert.equal(rowsOf(doc).length, 0, '⌘\\ не свернул колонку');
  const saved = calls.filter(([c]) => c === 'settings_set').map(([, a]) => a.patch);
  assert.ok(saved.some((p) => p.agentSideOff === true), 'свёрнутость не уехала в настройки: ' + JSON.stringify(saved));

  fold();
  await tick();
  assert.equal(rowsOf(doc).length, 2, '⌘\\ не вернул колонку');
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

/* Несколько Джарвисов разом — то, ради чего всё и затевалось: дать промпт
 * одному и уйти работать со вторым. Пока пузырь был один на окно, уходить из
 * отвечающего чата запрещали — иначе ответ первого дорисовывался бы в ленту
 * второго. Теперь поток помечен chatId, и разводит его по лентам окно. */

const TWO = (current = 'c1') => ({
  ok: true,
  current,
  chats: [
    { id: 'c1', name: 'Джарвис', named: true, sessionId: 's-1', current: current === 'c1', turns: 1, at: Date.now(), preview: '' },
    { id: 'c2', name: 'Выборы', named: true, sessionId: 's-2', current: current === 'c2', turns: 1, at: Date.now(), preview: '' },
  ],
});

// У каждого чата своя реплика в истории: перепутанная лента видна сразу
const bootTwo = () => boot({ sessionId: 's-1' }, {
  agent_chats_list: () => TWO(),
  agent_chat_switch: (a) => TWO(a.chatId),
  agent_chat_history: (a) => ({ ok: true, items: [{ role: 'user', kind: 'text', text: 'это ' + a.chatId, ts: 1 }] }),
  // адресата хода называет ядро — окно берёт метку из ответа, а не гадает
  agent_send: (a) => ({ ok: true, chatId: a.chatId }),
});
const goTo = (s, re) => s.hit(rowsOf(s.doc).find((c) => re.test(c.textContent)));

test('событие уходит в свой чат, а не в тот, что открыт', async () => {
  const { doc, emit } = await bootTwo();
  await emit({ type: 'delta', text: 'ответ про выборы', chatId: 'c2' });
  const msgs = doc.getElementById('msgs');
  assert.doesNotMatch(msgs.textContent, /ответ про выборы/, 'ответ соседнего чата дорисован в открытый');
  assert.equal(msgs.querySelectorAll('.msg.assistant').length, 0, 'в открытой ленте появился чужой пузырь');
});

test('переключиться можно, пока агент отвечает, и ленты не смешиваются', async () => {
  const s = await bootTwo();
  const { doc, window, say, emit } = s;
  await say('посмотри округ');
  assert.equal(doc.getElementById('send').disabled, true, 'ожидание ответа не показано');

  await goTo(s, /Выборы/);
  const msgs = doc.getElementById('msgs');
  assert.doesNotMatch(msgs.textContent, /чат переключится/, 'переключение во время ответа всё ещё запрещено');
  assert.match(msgs.textContent, /это c2/, 'лента второго чата не открылась');
  assert.doesNotMatch(msgs.textContent, /посмотри округ/, 'реплика первого чата утекла во второй');

  // первый продолжает отвечать — но в свою ленту, а не в ту, что на экране
  await emit({ type: 'delta', text: 'по округу тихо', chatId: 'c1' });
  assert.doesNotMatch(msgs.textContent, /по округу тихо/, 'ответ первого чата дорисован во второй');
});

test('ответ доезжает и в закрытый чат: вернулся — а он готов', async () => {
  const s = await bootTwo();
  const { doc, window, say, emit } = s;
  await say('посмотри округ');
  await goTo(s, /Выборы/);

  await emit({ type: 'delta', text: 'по округу тихо', chatId: 'c1' });
  await emit({ type: 'done', result: '', session_id: 's-1', chatId: 'c1' });

  await goTo(s, /Джарвис/);
  const msgs = doc.getElementById('msgs');
  assert.match(msgs.textContent, /по округу тихо/, 'ответ закрытого чата потерян — вернулись к пустоте');
  assert.match(msgs.textContent, /посмотри округ/, 'своя реплика пропала из ленты');
  assert.match(msgs.textContent, /это c1/, 'прошлая переписка не пережила ухода в соседний чат');
});

test('два ответа в полёте не смешиваются даже вперемежку', async () => {
  const s = await bootTwo();
  const { doc, window, say, emit } = s;
  await say('первый вопрос');
  await goTo(s, /Выборы/);
  await say('второй вопрос');

  for (const [text, chatId] of [['один-', 'c1'], ['два-', 'c2'], ['один', 'c1'], ['два', 'c2']]) {
    await emit({ type: 'delta', text, chatId });
  }
  const bubbles = () => [...doc.querySelectorAll('#msgs .msg.assistant .bubble')].map((b) => b.textContent);
  assert.deepEqual(bubbles(), ['два-два'], 'в ленте второго чата чужие куски');

  await goTo(s, /Джарвис/);
  assert.deepEqual(bubbles(), ['один-один'], 'ответ первого чата собрался не из своих дельт');
});

test('занятость видна в строке чата, из которой уже ушли', async () => {
  const s = await bootTwo();
  const { doc, say, emit } = s;
  await say('посмотри округ');
  const busy = () => [...doc.querySelectorAll('#chats .agchat.busy .agname')].map((n) => n.textContent);
  assert.deepEqual(busy(), ['Джарвис'], 'занятость чата не читается в списке');

  // ушли во второй — а первый всё ещё помечен: колонка на виду, и терять его
  // из виду больше не за что
  await goTo(s, /Выборы/);
  assert.deepEqual(busy(), ['Джарвис'], 'про отвечающий чат забыли, едва ушли из него');
  assert.equal(doc.getElementById('sub').textContent, 'готов', '«думает…» осталось от соседнего разговора');

  await emit({ type: 'done', result: 'готово', session_id: 's-1', chatId: 'c1' });
  assert.deepEqual(busy(), [], 'занятость висит на закончившем разговоре');
});

test('пока один чат отвечает, во второй можно писать', async () => {
  const s = await bootTwo();
  const { doc, window, say, sent } = s;
  await say('посмотри округ');
  await goTo(s, /Выборы/);
  assert.equal(doc.getElementById('send').disabled, false, 'свободный чат заперт занятостью соседа');

  await say('а тут что');
  assert.deepEqual(sent().map((a) => a.chatId), ['c1', 'c2'], 'вторая реплика ушла не в тот чат');
  assert.equal(sent()[1].sessionId, 's-2', 'реплика уехала в нить соседнего разговора');
  assert.match(text(doc), /а тут что/, 'своя реплика не попала в ленту');
  assert.doesNotMatch(text(doc), /посмотри округ/, 'реплика соседнего чата нарисована в этой ленте');
});

/* Метку хода называет ядро: окно могло послать только нить или вовсе ничего, а
 * события придут помеченными. Не перевесить свой разговор на эту метку — значит
 * смотреть на пустую ленту, пока ответ рисуется в чат-невидимку. */
test('чат, названный ядром в ответе на отправку, становится своим', async () => {
  const { doc, say, emit } = await boot({ sessionId: null }, {
    agent_send: () => ({ ok: true, chatId: 'c7' }),
  });
  await say('привет');
  await emit({ type: 'delta', text: 'отвечаю', chatId: 'c7' });
  assert.match(text(doc), /отвечаю/, 'ответ на свою же реплику уехал в невидимый чат');
  assert.equal(doc.getElementById('send').disabled, true, 'ход идёт, а поле уже свободно');

  await emit({ type: 'done', result: '', session_id: 's-7', chatId: 'c7' });
  assert.equal(doc.getElementById('send').disabled, false, 'ход кончился, а поле заперто');
});

/* Разговор, отвечающий прямо сейчас, убрать нельзя: ответ ещё едет, а ленты под
 * ним уже не будет. Уйти из него при этом можно — это разные вещи. */
test('убрать отвечающий чат не дают, и говорят почему', async () => {
  const s = await bootTwo();
  const { doc, say, calls } = s;
  await say('посмотри округ');
  const row = rowsOf(doc).find((c) => /Джарвис/.test(c.textContent));
  await s.hit(pick(await menuOf(s, row), /Скрыть/));
  assert.equal(calls.filter(([c]) => c === 'agent_chat_delete').length, 0, 'чат унесли из-под живого ответа');
  assert.match(text(doc), /когда закончит/, 'отказ съеден молча: ' + text(doc));
});

test('свежий id из потока перекрывает восстановленный', async () => {
  const { emit, say, sent } = await boot({ sessionId: null });
  await say('привет');
  await emit({ type: 'init', tools: [], model: 'claude-sonnet-4-5', session_id: 's-new' });
  await emit({ type: 'done', result: 'ок', session_id: 's-new' });
  await say('второе');
  assert.equal(sent()[1].sessionId, 's-new');
});
