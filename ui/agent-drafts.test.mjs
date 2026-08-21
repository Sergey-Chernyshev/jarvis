/* Недописанные реплики чатов Джарвиса в настоящем DOM.
 *
 * Ловит то, ради чего черновики и заведены: поле ввода было ОДНО на все чаты.
 * Текст, написанный одному Джарвису, оставался в поле и уезжал в тот, который
 * открыли следующим. Чаты раздают промпты в сессии с доступом к файлам —
 * промах адресатом здесь не «неловко», а «ушло не туда и там исполнилось».
 *
 * Проверяем окно из трея: оно и вкладка панели поднимаются одним mount(), и
 * разметка у окна своя, целиком в agent-chat.html.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const HERE = new URL('./', import.meta.url);
const read = (name) => readFileSync(new URL(name, HERE), 'utf8');
const tick = () => new Promise((r) => setTimeout(r, 0));
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

/* Общая книжка черновиков — ровно то, что лежит в agent-drafts.json. Живёт ВНЕ
 * окна: через неё проверяется и круг через диск (закрыли-открыли), и второе
 * окно, смотрящее в ту же книжку. */
function makeDisk(names = ['Первый', 'Второй']) {
  const drafts = new Map(); // chatId → { text, caret }
  const chats = names.map((name, i) => ({
    id: 'c' + (i + 1), name, sessionId: 's' + (i + 1), turns: 2, preview: 'последняя реплика',
  }));
  const world = {
    drafts,
    chats,
    current: 'c1',
    sendResult: (args) => ({ ok: true, chatId: args.chatId }),
    book: () => ({ ok: true, current: world.current, chats: world.chats.map((c) => ({ ...c })), hidden: 0 }),
  };
  return world;
}

async function boot(world) {
  const { window, document } = parseHTML(read('agent-chat.html'));
  const calls = [];
  const handlers = {};
  window.innerWidth = 0;
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args = {}) => {
        calls.push([cmd, args]);
        switch (cmd) {
          case 'agent_chat_state':
            return { sessionId: null, chatId: world.current };
          case 'agent_chat_history':
            return { ok: true, items: [], total: 0 };
          case 'agent_chats_list':
            return world.book();
          case 'agent_chat_switch':
            world.current = args.chatId;
            return world.book();
          case 'agent_chat_delete':
            world.chats = world.chats.filter((c) => c.id !== args.chatId);
            if (world.current === args.chatId) world.current = world.chats[0].id;
            return world.book();
          case 'agent_history_forget':
          case 'agent_history_hide':
            return world.book();
          case 'agent_drafts_get': {
            const out = {};
            for (const [id, d] of world.drafts) out[id] = { text: d.text, caret: d.caret };
            return { ok: true, drafts: out };
          }
          case 'agent_draft_set':
            if (String(args.text || '').trim()) world.drafts.set(args.chatId, { text: args.text, caret: args.caret | 0 });
            else world.drafts.delete(args.chatId);
            return { ok: true };
          case 'agent_send':
            return world.sendResult(args);
          default:
            return { ok: true };
        }
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
  await tick();

  const input = document.getElementById('input');
  const fire = (node, name) => { node.dispatchEvent(new window.Event(name, { bubbles: true })); return tick(); };
  /* Набрать текст: значение плюс событие — как настоящая клавиатура. Курсор
   * называем сами: где он стоял, знает только человек. */
  const type = async (text, caret) => {
    input.value = text;
    input.selectionStart = caret == null ? text.length : caret;
    input.selectionEnd = input.selectionStart;
    await fire(input, 'input');
  };
  // Уход фокуса — тот самый принудительный сброс: полсекунды тишины до него не доживают
  const leave = async () => { await fire(input, 'blur'); await tick(); };
  const rows = () => [...document.querySelectorAll('#chats .agchat')];
  const rowOf = (name) => rows().find((r) => r.querySelector('.agname').textContent === name);
  const open = async (name) => { await fire(rowOf(name), 'click'); await tick(); await tick(); };
  const menu = async (name, label) => {
    await fire(rowOf(name).querySelector('.agdots'), 'click');
    const item = [...document.querySelectorAll('#chats .agmi')].find((m) => m.textContent === label);
    assert.ok(item, 'в меню строки нет пункта «' + label + '»');
    await fire(item, 'click');
    await tick();
  };
  const send = async () => {
    await fire(document.getElementById('send'), 'click');
    await tick();
    await tick();
  };
  return { window, doc: document, input, calls, type, leave, rows, rowOf, open, menu, send, fire };
}

const draftLine = (row) => {
  const d = row.querySelector('.agdraft');
  return d ? d.textContent : '';
};

/* ---------- главное: чаты не делят одно поле ---------- */

test('текст двух чатов не смешивается при переключении', async () => {
  const world = makeDisk();
  const w = await boot(world);

  await w.type('промпт для первого');
  await w.open('Второй');
  assert.equal(w.input.value, '', 'чужой черновик приехал в соседний чат — так и промахиваются адресатом');

  await w.type('промпт для второго');
  await w.open('Первый');
  assert.equal(w.input.value, 'промпт для первого', 'свой черновик не вернулся — набранное потеряно');

  await w.open('Второй');
  assert.equal(w.input.value, 'промпт для второго');
});

test('черновик переживает круг через диск', async () => {
  const world = makeDisk();
  const first = await boot(world);
  await first.type('не отправлено и не потеряно');
  await first.leave(); // окно закрывают — на диск дожимаем раньше срока

  assert.equal(world.drafts.get('c1').text, 'не отправлено и не потеряно', 'на диск не легло вовсе');

  const again = await boot(world); // то же, что запуск приложения заново
  assert.equal(again.input.value, 'не отправлено и не потеряно', 'после перезапуска поле пусто — текст пропал');
});

test('на каждый символ в файл не бьём, но через полсекунды тишины пишем', async () => {
  const world = makeDisk();
  const w = await boot(world);
  await w.type('раз');
  assert.equal(world.drafts.size, 0, 'запись на диск ушла прямо с клавиши');
  const wrote = () => w.calls.filter(([c]) => c === 'agent_draft_set').length;
  assert.equal(wrote(), 0);

  await wait(700);
  assert.equal(world.drafts.size, 1, 'после тишины черновик так и не лёг на диск');
  assert.equal(world.drafts.get('c1').text, 'раз');
  assert.equal(wrote(), 1, 'записей больше одной — задержка не собирает нажатия');
});

test('позиция курсора восстанавливается вместе с текстом', async () => {
  const world = makeDisk();
  const w = await boot(world);
  await w.type('начало и хвост', 6); // человек стоит посередине строки
  await w.open('Второй');
  await w.type('другой текст с другим курсором', 20); // курсор соседа не должен приехать сюда
  await w.open('Первый');

  assert.equal(w.input.value, 'начало и хвост');
  assert.equal(w.input.selectionStart, 6, 'курсор уехал в конец — дописывать будут не туда, куда смотрели');
  assert.equal(w.input.selectionEnd, 6);
});

test('курсор переживает и круг через диск', async () => {
  const world = makeDisk();
  const first = await boot(world);
  await first.type('начало и хвост', 6);
  await first.leave();
  assert.equal(world.drafts.get('c1').caret, 6, 'на диск уехал текст без курсора');

  const again = await boot(world);
  assert.equal(again.input.selectionStart, 6);
});

/* ---------- отправка ---------- */

test('удавшаяся отправка чистит черновик — и в поле, и на диске', async () => {
  const world = makeDisk();
  const w = await boot(world);
  await w.type('это уедет агенту');
  await w.leave();
  assert.equal(world.drafts.size, 1);

  await w.type('это уедет агенту');
  await w.send();

  assert.equal(w.input.value, '');
  assert.equal(world.drafts.size, 0, 'отправленное осталось черновиком — строка списка будет врать');
});

test('отказ отправки черновик не трогает: текст остаётся у человека', async () => {
  const world = makeDisk();
  // Отказ приезжает РАЗРЕШЁННЫМ промисом — на этом уже спотыкались.
  world.sendResult = () => ({ ok: false, error: 'claude не найден' });
  const w = await boot(world);
  await w.type('дорогой промпт', 4);
  await w.send();

  assert.equal(w.input.value, 'дорогой промпт', 'поле пусто, а сообщение не ушло — текст набирать заново');
  assert.equal(w.input.selectionStart, 4, 'текст вернули, а курсор нет');
  assert.equal(world.drafts.get('c1').text, 'дорогой промпт', 'черновик стёрт за неслучившуюся отправку');
});

test('упавшая отправка тоже оставляет текст', async () => {
  const world = makeDisk();
  world.sendResult = () => { throw new Error('мост умер'); };
  const w = await boot(world);
  await w.type('дорогой промпт');
  await w.send();

  assert.equal(w.input.value, 'дорогой промпт');
  assert.equal(world.drafts.get('c1').text, 'дорогой промпт');
});

/* ---------- удаление против скрытия ---------- */

test('скрытие чата черновик оставляет — оно обратимо', async () => {
  const world = makeDisk();
  const w = await boot(world);
  await w.open('Второй');
  await w.type('черновик второго');
  await w.leave();
  await w.open('Первый');

  await w.menu('Второй', 'Скрыть');
  assert.ok(world.drafts.has('c2'), 'скрытие унесло набранное — обратимость обещана, а текста нет');
  assert.equal(world.drafts.get('c2').text, 'черновик второго');
});

test('забвение чата уносит и черновик — он был репликой именно в него', async () => {
  const world = makeDisk();
  const w = await boot(world);
  await w.open('Второй');
  await w.type('черновик второго');
  await w.leave();
  await w.open('Первый');

  assert.equal(world.drafts.get('c2').text, 'черновик второго', 'черновика не было — проверять нечего');

  await w.menu('Второй', 'Забыть насовсем');
  const yes = [...w.doc.querySelectorAll('#chats .agbtn')].find((b) => b.textContent === 'Удалить');
  assert.ok(yes, 'вопрос про необратимое удаление не задан');
  await w.fire(yes, 'click');
  await tick();
  await tick();

  assert.equal(world.drafts.has('c2'), false, 'разговора нет, а черновик в него остался');
});

/* ---------- пометка в списке ---------- */

test('строка с непустым черновиком помечена, а пустая — нет', async () => {
  const world = makeDisk();
  const w = await boot(world);
  assert.equal(draftLine(w.rowOf('Первый')), '', 'пометка есть там, где ничего не набрано');

  await w.type('начало текста и дальше ещё много слов');
  assert.match(draftLine(w.rowOf('Первый')), /^Черновик: начало текста/,
    'непустой черновик ничем не помечен — про него забудут либо отправят не туда');

  await w.type(''); // человек стёр набранное сам
  assert.equal(draftLine(w.rowOf('Первый')), '', 'пометка пережила стирание текста');
});

test('пометка стоит у своего чата, а не у открытого', async () => {
  const world = makeDisk();
  const w = await boot(world);
  await w.type('только у первого');
  await w.open('Второй');

  assert.match(draftLine(w.rowOf('Первый')), /^Черновик: только у первого/);
  assert.equal(draftLine(w.rowOf('Второй')), '');
});

test('пометка исчезает после удавшейся отправки', async () => {
  const world = makeDisk();
  const w = await boot(world);
  await w.type('уедет');
  assert.match(draftLine(w.rowOf('Первый')), /Черновик/);
  await w.send();
  assert.equal(draftLine(w.rowOf('Первый')), '');
});

/* ---------- два окна на один чат ---------- */

test('второе окно видит черновик первого, а не пустое поле', async () => {
  const world = makeDisk();
  const tray = await boot(world);
  await tray.type('набрано в первом окне');
  await tray.leave(); // уход фокуса дожимает черновик на диск

  const second = await boot(world); // вкладка панели: тот же mount, та же книжка
  assert.equal(second.input.value, 'набрано в первом окне');
  assert.match(draftLine(second.rowOf('Первый')), /Черновик: набрано в первом окне/);
});

test('окно, где ничего не набирали, чужой черновик обратно не пишет', async () => {
  const world = makeDisk();
  const first = await boot(world);
  await first.type('свежий текст');
  await first.leave();

  const second = await boot(world); // только показало — не трогало
  await second.open('Второй');
  await second.open('Первый');
  assert.equal(world.drafts.get('c1').text, 'свежий текст', 'окно-зритель переписало книжку своей копией');
});
