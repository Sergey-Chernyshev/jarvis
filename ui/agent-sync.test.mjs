/* Вкладка «Джарвис» и окно из трея — одна и та же поверхность в двух окнах.
 *
 * Оба документа поднимаются здесь одновременно на общей шине событий: именно в
 * этой паре ломались обе вещи, которые тест стережёт. Маркдаун рисовал только
 * чат сессии (рендерер жил в renderer.js, а его нет ни во вкладке, ни в окне),
 * а карточку подтверждения демон шлёт всем — решали её в одном окне, и во
 * втором оставалась живая кнопка от вопроса, которого уже нет.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const HERE = new URL('./', import.meta.url);
const read = (name) => readFileSync(new URL(name, HERE), 'utf8');
const tick = () => new Promise((r) => setTimeout(r, 0));

/* Шина Tauri: одно событие — всем подписчикам обоих окон. */
function makeBus() {
  const subs = new Map();
  return {
    listen: async (name, cb) => {
      const list = subs.get(name) || [];
      subs.set(name, list.concat(cb));
      return () => {};
    },
    emit: async (name, payload) => {
      for (const cb of subs.get(name) || []) await cb({ payload });
      await tick();
    },
  };
}

const SCRIPTS = ['markdown.js', 'agent-chat.js'];
const runScripts = (window, document) => {
  for (const name of SCRIPTS) {
    new Function('window', 'document', 'globalThis', read(name))(window, document, window);
  }
};

/* Книжка чатов — общая на оба входа: история разговоров обязана выглядеть
 * одинаково и во вкладке, и в окне, иначе два места разъедутся молча.
 * Третья строка — разговор с диска: чата за ним нет (id: null), и имя ему уже
 * собрал демон из первой реплики. */
const DISK = { id: null, name: 'Изучите текущие сессии', named: false, sessionId: 'a25d01f8', current: false, turns: 249, at: 1755689100000, preview: 'Изучите текущие сессии' };
const BOOK = {
  ok: true,
  current: 'c1',
  hidden: 0,
  chats: [
    { id: 'c1', name: 'Джарвис', named: true, sessionId: null, current: true, turns: 0, at: null, preview: '' },
    { id: 'c2', name: 'Выборы', named: true, sessionId: 's-2', current: false, turns: 12, at: 1755689100000, preview: 'что с округом' },
    DISK,
  ],
};
/* Тот же список после скрытия строки с диска: её в chats нет, а `hidden` знает,
 * что файл никуда не делся. Ровно этой парой ядро и отвечает всем командам. */
const AFTER_HIDE = { ...BOOK, hidden: 1, chats: BOOK.chats.filter((c) => c.id) };
// А после забвения прятать уже нечего: файла нет, и `hidden` снова ноль.
const AFTER_FORGET = { ...BOOK, hidden: 0, chats: BOOK.chats.filter((c) => c.id) };

/* Окно из трея: своей разметкой оно само себя и опознаёт. */
async function bootWindow(bus) {
  const { window, document } = parseHTML(read('agent-chat.html'));
  const calls = [];
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        calls.push([cmd, args]);
        if (cmd === 'agent_chat_state') return { sessionId: null };
        if (cmd === 'agent_history_hide') return AFTER_HIDE;
        if (cmd === 'agent_history_forget') return AFTER_FORGET;
        if (cmd === 'agent_chats_list' || cmd === 'agent_chat_open' || cmd === 'agent_history_unhide_all') return BOOK;
        if (cmd === 'agent_stop') return { ok: true, stopped: true, chatId: (args || {}).chatId, children: [] };
        return { ok: true };
      },
    },
    event: bus,
  };
  runScripts(window, document);
  await tick();
  return {
    window,
    calls,
    did: (cmd) => calls.filter(([c]) => c === cmd).map(([, a]) => (a || {}).sessionId),
    msgs: document.getElementById('msgs'),
    chats: document.getElementById('chats'),
    // в окне из трея своей строки поиска нет — поле живёт в шапке колонки
    find: null,
  };
}

/* Вкладка панели: та же логика поверх разметки index.html и моста window.jarvis
 * (renderer.js здесь не нужен — вкладку монтирует initAgentChat). */
async function bootTab(bus) {
  const { window, document } = parseHTML(read('index.html'));
  // Вызовы записываем именем команды демона, а не моста: паритет проверяется по
  // тому, ЧТО уедет ядру, — иначе два входа сойдутся на разных командах.
  const calls = [];
  const rec = (cmd, sessionId, res) => { calls.push([cmd, sessionId]); return res; };
  window.__TAURI__ = { core: { invoke: async () => ({ ok: true }) }, event: bus };
  window.jarvis = {
    agentChatState: async () => ({ sessionId: null }),
    agentChatsList: async () => BOOK,
    agentChatHistory: async () => ({ ok: true, items: [] }),
    agentChatReset: async () => ({ ok: true }),
    agentChatOpen: async (sessionId) => rec('agent_chat_open', sessionId, BOOK),
    agentHistoryHide: async (sessionId) => rec('agent_history_hide', sessionId, AFTER_HIDE),
    agentHistoryUnhideAll: async () => rec('agent_history_unhide_all', undefined, BOOK),
    agentHistoryForget: async (sessionId) => rec('agent_history_forget', sessionId, AFTER_FORGET),
    agentSend: async () => ({ ok: true }),
    agentStop: async (chatId) => rec('agent_stop', chatId, { ok: true, stopped: true, chatId, children: [] }),
    agentChainState: async () => ({ ok: true, state: { active: false, mode: 'ask' } }),
    agentChainMode: async () => ({ ok: true }),
    agentConfirm: async () => ({ ok: true }),
    onAgentEvent: (cb) => bus.listen('agent:event', (e) => cb(e.payload)),
    onAgentConfirm: (cb) => bus.listen('agent:confirm', (e) => cb(e.payload)),
  };
  runScripts(window, document);
  window.initAgentChat(document.getElementById('agentPane'));
  await tick();
  return {
    window,
    did: (cmd) => calls.filter(([c]) => c === cmd).map(([, a]) => a),
    msgs: document.getElementById('agLog'),
    chats: document.getElementById('agChats'),
    // поиск по чатам во вкладке — общая строка поиска панели (renderer.js)
    find: document.getElementById('query'),
  };
}

const CARD = { nonce: 'n-1', id: 'sessions.reply', class: 'effect', card: { kind: 'session', label: 'jarvis', text: 'го' } };

/* Полный список чатов в обоих входах, а не «во вкладке всё, в окне только
 * текущий»: когда панель закрыта, окно из трея — единственная дорога к
 * остальным проектам. Расхождение здесь тихое: увидишь его, только когда
 * понадобится другой чат. */
const click = (s) => new s.window.Event('click', { bubbles: true });
const rowsOf = (s) => [...s.chats.querySelectorAll('.agchat')];
const rowBy = (s, re) => rowsOf(s).find((r) => re.test(r.textContent));
/* Редкие действия строки — под «…»: раньше их было три иконки в ряд, и
 * необратимое стояло вровень с обратимыми. */
const menuOf = async (s, row) => {
  row.querySelector('.agdots').dispatchEvent(click(s));
  await tick();
  return [...s.chats.querySelectorAll('.agmi')];
};
const pick = (items, re) => items.find((i) => re.test(i.textContent));

test('колонка чатов одинакова во вкладке и в окне', async () => {
  const bus = makeBus();
  for (const s of [await bootTab(bus), await bootWindow(bus)]) {
    // видна сразу и в обоих местах: раскрывать нечего, в этом вся затея
    const names = [...s.chats.querySelectorAll('.agchat:not(.disk) .agname')].map((n) => n.textContent);
    assert.deepEqual(names, ['Джарвис', 'Выборы']);
    assert.equal(s.chats.querySelectorAll('.agchat.on').length, 1, 'открытый чат не помечен');
    assert.equal(s.chats.querySelectorAll('.agnew').length, 1, 'завести чат нечем');
    /* Искать по чатам можно у обоих входов, но полем — одним: в окне из трея
     * своим, в шапке колонки; во вкладке — общей строкой поиска панели, куда
     * её и переводит renderer.js. Двух похожих полей рядом не бывает: по виду
     * не отличить, что где ищет. */
    const fields = (s.chats.querySelector('.agfind') ? 1 : 0) + (s.find ? 1 : 0);
    assert.equal(fields, 1, 'поиск по чатам либо потерян, либо задвоен');
    // и вход в действия строки один и тот же
    assert.equal(s.chats.querySelectorAll('.agdots').length, 3, 'действия строки негде найти');
  }
});

/* Разговор, найденный на диске, — та самая потеря: чата за ним нет, и без этой
 * строки он недостижим. Отдельного раздела ему не заводим: та же строка в том
 * же списке, отличается бейджем. */
test('разговор с диска — строка того же списка и открывается одинаково', async () => {
  const bus = makeBus();
  for (const s of [await bootTab(bus), await bootWindow(bus)]) {
    const disk = s.chats.querySelectorAll('.agchat.disk');
    assert.equal(disk.length, 1, 'разговор с диска потерян в списке');
    assert.equal(disk[0].querySelector('.agname').textContent, 'Изучите текущие сессии');
    assert.match(disk[0].textContent, /с диска/, 'строку с диска не отличить от чата');
    // переименовывать нечего: чата за ней ещё нет
    const items = await menuOf(s, disk[0]);
    assert.deepEqual(items.map((i) => i.textContent), ['Открыть', 'Скрыть', 'Забыть насовсем']);

    disk[0].dispatchEvent(click(s));
    await tick();
    await tick();
    assert.deepEqual(s.did('agent_chat_open'), ['a25d01f8'], 'разговор с диска не привязан');
  }
});

/* «Скрыть» у чата и у строки с диска значат разное: у чата это agent_chat_delete
 * (разговор вернётся строкой с диска), у строки — agent_history_hide. Разъедется
 * здесь, и человек уберёт не то, что думал. */
test('«Скрыть» зовёт своё в обоих входах и говорит, что останется', async () => {
  const bus = makeBus();
  for (const s of [await bootTab(bus), await bootWindow(bus)]) {
    const chatHide = pick(await menuOf(s, rowBy(s, /Джарвис/)), /Скрыть/);
    const diskHide = pick(await menuOf(s, s.chats.querySelector('.agchat.disk')), /Скрыть/);
    assert.notEqual(chatHide.title, diskHide.title, 'два «Скрыть» обещают одно и то же');
    assert.match(chatHide.title, /чат/, '«Скрыть» у чата молчит про чат: ' + chatHide.title);
    assert.match(diskHide.title, /останется на диске/, 'скрытие не сказало, что файл цел: ' + diskHide.title);

    diskHide.dispatchEvent(click(s));
    await tick();
    await tick();
    assert.deepEqual(s.did('agent_history_hide'), ['a25d01f8'], '«Скрыть» ничего не спрятало');
    assert.deepEqual(s.did('agent_history_forget'), [], 'скрытие обернулось удалением файла');
    assert.equal(s.chats.querySelectorAll('.agchat.disk').length, 0, 'спрятанная строка осталась в списке');
  }
});

/* Скрытие без следа — это тихая потеря: через неделю не вспомнить, что прятал
 * сам. «Скрыто N · вернуть» и есть весь след, и он обязан быть в обоих входах. */
test('«скрыто N · вернуть» видно и возвращает и во вкладке, и в окне', async () => {
  const bus = makeBus();
  for (const s of [await bootTab(bus), await bootWindow(bus)]) {
    assert.equal(s.chats.querySelectorAll('.aghidden').length, 0, 'скрытых нет, а строка про них есть');
    const hide = pick(await menuOf(s, s.chats.querySelector('.agchat.disk')), /Скрыть/);
    hide.dispatchEvent(click(s));
    await tick();
    await tick();

    const back = s.chats.querySelector('.aghidden');
    assert.ok(back, 'разговор спрятан бесследно: ' + s.chats.textContent);
    assert.match(back.textContent, /Скрыто 1/);

    back.dispatchEvent(click(s));
    await tick();
    await tick();
    assert.deepEqual(s.did('agent_history_unhide_all'), [undefined], 'возврат не уехал демону');
    assert.equal(s.chats.querySelectorAll('.agchat.disk').length, 1, 'вернули — а строка не вернулась');
    assert.equal(s.chats.querySelectorAll('.aghidden').length, 0, 'прятать больше нечего, а строка висит');
  }
});

test('маркдаун ответа одинаков во вкладке и в окне', async () => {
  const bus = makeBus();
  const surfaces = [await bootTab(bus), await bootWindow(bus)];
  await bus.emit('agent:event', { type: 'delta', text: '## Готово\n\n- пункт\n\n```\ncargo test\n```\n' });

  for (const { msgs } of surfaces) {
    const b = msgs.querySelector('.msg.assistant .bubble');
    assert.equal(b.querySelector('.md-h').textContent, 'Готово');
    assert.equal(b.querySelectorAll('ul li').length, 1);
    assert.equal(b.querySelector('pre').textContent, 'cargo test');
  }
});

/* Оба окна слушают ОДИН поток, и метка чата в нём — единственное, что не даёт
 * ответу одного разговора дорисоваться в ленту другого. Разъедется здесь — и
 * человек увидит чужой ответ в своём чате, причём заметит не сразу. */
test('помеченное чатом событие не путает ленты ни во вкладке, ни в окне', async () => {
  const bus = makeBus();
  const surfaces = [await bootTab(bus), await bootWindow(bus)];
  await bus.emit('agent:event', { type: 'delta', text: 'ответ второго чата', chatId: 'c2' });
  for (const s of surfaces) {
    assert.equal(s.msgs.querySelectorAll('.msg.assistant').length, 0, 'чужой пузырь в открытой ленте');
    assert.doesNotMatch(s.msgs.textContent, /ответ второго чата/);
  }

  await bus.emit('agent:event', { type: 'delta', text: 'ответ первого', chatId: 'c1' });
  for (const s of surfaces) assert.match(s.msgs.textContent, /ответ первого/, 'событие открытого чата потеряно');
});

/* Занятость — тоже про оба входа: ушёл из вкладки в окно из трея и должен
 * видеть тот же список отвечающих, иначе один из них потеряется молча. */
test('занятый разговор помечен и во вкладке, и в окне', async () => {
  const bus = makeBus();
  const surfaces = [await bootTab(bus), await bootWindow(bus)];
  await bus.emit('agent:event', { type: 'delta', text: 'думаю', chatId: 'c2' });

  for (const s of surfaces) {
    const names = [...s.chats.querySelectorAll('.agchat.busy .agname')].map((n) => n.textContent);
    assert.deepEqual(names, ['Выборы'], 'занятость чата не читается в колонке');
  }

  await bus.emit('agent:event', { type: 'done', result: 'готово', chatId: 'c2' });
  for (const s of surfaces) {
    assert.equal(s.chats.querySelectorAll('.agchat.busy').length, 0, 'занятость висит на закончившем разговоре');
  }
});

/* Остановка хода — тоже про оба входа. Разметки у вкладки и окна две, и кнопку
 * «стоп» ставит код: нарисованная в одной из них, она оставила бы второй вход с
 * единственной клавишей — а её ещё надо знать. */
test('ход останавливают и во вкладке, и в окне — кнопкой и клавишей', async () => {
  const bus = makeBus();
  const surfaces = [await bootTab(bus), await bootWindow(bus)];
  for (const s of surfaces) {
    const btn = s.msgs.ownerDocument.querySelector('.agstop');
    assert.ok(btn, 'кнопки «стоп» нет вовсе');
    assert.equal(btn.hidden, true, 'кнопка обещает остановку, когда останавливать нечего');
  }

  await bus.emit('agent:event', { type: 'delta', text: 'половина ответа', chatId: 'c1' });
  for (const s of surfaces) {
    assert.equal(s.msgs.ownerDocument.querySelector('.agstop').hidden, false, 'мышью ход не остановить');
  }

  // во вкладке — клавишей (вкладка на экране: спрятанной ей Esc не адресован),
  // в окне — кнопкой: оба способа обязаны кончаться одним и тем же
  const tabDoc = surfaces[0].msgs.ownerDocument;
  tabDoc.getElementById('agentPane').removeAttribute('hidden');
  const esc = new surfaces[0].window.Event('keydown', { bubbles: true });
  esc.key = 'Escape';
  esc.preventDefault = () => {};
  tabDoc.dispatchEvent(esc);
  surfaces[1].msgs.ownerDocument.querySelector('.agstop')
    .dispatchEvent(new surfaces[1].window.Event('click', { bubbles: true }));
  await tick();
  await tick();

  for (const s of surfaces) {
    assert.equal(s.did('agent_stop').length, 1, 'остановка не уехала ядру');
    const doc = s.msgs.ownerDocument;
    assert.equal(doc.querySelectorAll('.stopmark').length, 1, 'оборванный ответ не помечен');
    assert.match(s.msgs.textContent, /половина ответа/, 'пришедшее стёрли вместе с ходом');
    assert.equal(doc.querySelector('.agstop').hidden, true, 'кнопка осталась после конца хода');
  }
});

test('решённая карточка снимается в обоих окнах, а не только там, где нажали', async () => {
  const bus = makeBus();
  const tab = await bootTab(bus);
  const win = await bootWindow(bus);
  await bus.emit('agent:confirm', CARD);

  const card = (s) => s.msgs.querySelector('.msg.confirm .cbox');
  assert.ok(card(tab) && card(win), 'вопрос пришёл не всем');

  card(tab).querySelector('.cbtn.yes').dispatchEvent(new tab.window.Event('click', { bubbles: true }));
  await tick();

  for (const s of [tab, win]) {
    // считаем кнопки, а не сравниваем узлы: неудачный assert над узлом linkedom
    // печатается вечность, и тест выглядит зависшим вместо провалившегося
    assert.equal(card(s).querySelectorAll('.cbtn').length, 0, 'кнопки живы у решённого вопроса');
    assert.doesNotMatch(card(s).textContent, /разрешено/, 'исход объявлен раньше, чем его назвал демон');
  }

  // Исход знает только демон: до его слова карточка ничего не обещает, после —
  // говорит ровно то, что случилось. Разрешили — а гейт успел истечь.
  await bus.emit('agent:confirm-done', { nonce: 'n-1', approved: false, outcome: 'expired' });
  for (const s of [tab, win]) {
    assert.match(card(s).textContent, /не выполнено/, 'нажали «Разрешить» — и получили тишину об исходе');
    assert.doesNotMatch(card(s).textContent, /отклонено/, '«отклонено» тому, кто ничего не отклонял');
  }
});

/* Своё же вещание возвращается и в то окно, где нажали: снятие карточки обязано
 * быть однократным, иначе второй ответ уедет демону по уже съеденному нонсу. */
test('решённый вопрос уходит демону ровно один раз', async () => {
  const bus = makeBus();
  const win = await bootWindow(bus);
  await bus.emit('agent:confirm', CARD);

  const yes = win.msgs.querySelector('.cbtn.yes');
  yes.dispatchEvent(new win.window.Event('click', { bubbles: true }));
  yes.dispatchEvent(new win.window.Event('click', { bubbles: true })); // повтор по мёртвой карточке
  await tick();

  const asked = win.calls.filter(([c]) => c === 'agent_confirm');
  assert.deepEqual(asked, [['agent_confirm', { nonce: 'n-1', approved: true }]]);
});
