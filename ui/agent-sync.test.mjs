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
  };
}

const CARD = { nonce: 'n-1', id: 'sessions.reply', class: 'effect', card: { kind: 'session', label: 'jarvis', text: 'го' } };

/* Полный список чатов в обоих входах, а не «во вкладке всё, в окне только
 * текущий»: когда панель закрыта, окно из трея — единственная дорога к
 * остальным проектам. Расхождение здесь тихое: увидишь его, только когда
 * понадобится другой чат. */
const openHist = (s) => {
  const t = s.chats.querySelector('.agtoggle');
  t.dispatchEvent(new s.window.Event('click', { bubbles: true }));
};

test('история разговоров одинакова во вкладке и в окне', async () => {
  const bus = makeBus();
  for (const s of [await bootTab(bus), await bootWindow(bus)]) {
    // свёрнута в обоих местах: место у переписки дороже места у списка
    assert.equal(s.chats.querySelectorAll('.agchat:not(.add)').length, 0, 'история раскрыта без спроса');
    assert.equal(s.chats.querySelector('.agcur').textContent, 'Джарвис', 'открытый чат не назван');

    openHist(s);
    const names = [...s.chats.querySelectorAll('.agchat:not(.add):not(.disk) .agname')].map((n) => n.textContent);
    assert.deepEqual(names, ['Джарвис', 'Выборы']);
    assert.equal(s.chats.querySelectorAll('.agchat.on').length, 1, 'открытый чат не помечен');
    assert.equal(s.chats.querySelectorAll('.agchat.add').length, 1, 'завести чат нечем');
    // чатов больше одного — крестик у каждого; последний удалять не дадут
    assert.equal(s.chats.querySelectorAll('.agx').length, 2);
    // и переименование в обоих местах на виду, а не спрятано в клике по строке
    assert.equal(s.chats.querySelectorAll('.agedit').length, 2, 'переименование негде найти');
  }
});

/* Разговор, найденный на диске, — та самая потеря: чата за ним нет, и без этой
 * строки он недостижим. Обе поверхности обязаны и показать его, и открыть
 * одной и той же командой. */
test('разговор с диска виден и открывается одинаково во вкладке и в окне', async () => {
  const bus = makeBus();
  for (const s of [await bootTab(bus), await bootWindow(bus)]) {
    openHist(s);
    const disk = s.chats.querySelectorAll('.agchat.disk');
    assert.equal(disk.length, 1, 'разговор с диска потерян в списке');
    assert.equal(disk[0].querySelector('.agname').textContent, 'Изучите текущие сессии');
    assert.equal(disk[0].querySelectorAll('.agedit').length, 0, 'переименование обещано там, где чата ещё нет');
    // крестик у него свой — «спрятать»; чатовый «удалить чат» сюда не подсовывают
    assert.equal(disk[0].querySelectorAll('.agx').length, 0, 'крестик чата обещан там, где чата нет');
    assert.equal(disk[0].querySelectorAll('.aghide').length, 1, 'строку с диска снова нечем убрать');

    disk[0].dispatchEvent(new s.window.Event('click', { bubbles: true }));
    await tick();
    await tick();
    assert.deepEqual(s.did('agent_chat_open'), ['a25d01f8'], 'разговор с диска не привязан');
  }
});

/* Два крестика в одном списке значат разное: у чата — «удалить чат», у строки с
 * диска — «спрятать». Разъедется здесь, и человек сотрёт чат, думая, что убирает
 * найденную на диске строку (или наоборот). Обе поверхности обязаны звать разные
 * команды и говорить о них разными словами. */
test('крестик чата и крестик строки с диска не путаются ни во вкладке, ни в окне', async () => {
  const bus = makeBus();
  for (const s of [await bootTab(bus), await bootWindow(bus)]) {
    openHist(s);
    const disk = s.chats.querySelector('.agchat.disk');
    const chat = s.chats.querySelector('.agchat:not(.add):not(.disk)');
    const hide = disk.querySelector('.aghide');
    const x = chat.querySelector('.agx');
    assert.notEqual(hide.title, x.title, 'два крестика обещают одно и то же');
    assert.match(hide.title, /останется на диске/, 'скрытие не сказало, что файл цел: ' + hide.title);
    assert.match(x.title, /чат/, 'крестик чата молчит про чат: ' + x.title);

    hide.dispatchEvent(new s.window.Event('click', { bubbles: true }));
    await tick();
    await tick();
    assert.deepEqual(s.did('agent_history_hide'), ['a25d01f8'], 'крестик строки с диска ничего не спрятал');
    assert.deepEqual(s.did('agent_history_forget'), [], 'скрытие обернулось удалением файла');
    assert.equal(s.chats.querySelectorAll('.agchat.disk').length, 0, 'спрятанная строка осталась в списке');
  }
});

/* Скрытие без следа — это тихая потеря: через неделю не вспомнить, что прятал
 * сам. «Скрыто N · вернуть» и есть весь след, и он обязан быть в обоих входах. */
test('«скрыто N · вернуть» видно и возвращает и во вкладке, и в окне', async () => {
  const bus = makeBus();
  for (const s of [await bootTab(bus), await bootWindow(bus)]) {
    openHist(s);
    assert.equal(s.chats.querySelectorAll('.aghidden').length, 0, 'скрытых нет, а строка про них есть');
    s.chats.querySelector('.agchat.disk .aghide').dispatchEvent(new s.window.Event('click', { bubbles: true }));
    await tick();
    await tick();

    const back = s.chats.querySelector('.aghidden');
    assert.ok(back, 'разговор спрятан бесследно: ' + s.chats.textContent);
    assert.match(back.textContent, /Скрыто 1/);
    // и в свёрнутой шапке тоже: колонку раскрывают не каждый день
    assert.match(s.chats.querySelector('.agtoggle').textContent, /скрыто 1/);

    back.dispatchEvent(new s.window.Event('click', { bubbles: true }));
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
    // свёрнутая история всё равно называет отвечающего соседа
    assert.match(s.chats.querySelector('.agbusy').textContent, /Выборы/, 'занятый сосед не назван в шапке');
    openHist(s);
    const names = [...s.chats.querySelectorAll('.agchat.busy .agname')].map((n) => n.textContent);
    assert.deepEqual(names, ['Выборы'], 'занятость чата не читается в истории');
  }

  await bus.emit('agent:event', { type: 'done', result: 'готово', chatId: 'c2' });
  for (const s of surfaces) {
    assert.equal(s.chats.querySelectorAll('.agbusy').length, 0, 'чат закончил, а в шапке всё ещё отвечает');
    assert.equal(s.chats.querySelectorAll('.agchat.busy').length, 0, 'занятость висит на закончившем разговоре');
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
