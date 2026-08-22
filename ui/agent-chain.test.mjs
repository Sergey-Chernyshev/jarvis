/* Ночная работа Джарвиса — в ленте чата.
 *
 * Дыра, ради которой тест написан: обработчик agent:chain брал из события один
 * срез для шапки и выбрасывал kind с текстом. То есть ночью цепочка честно
 * работала, копила уведомления и писала их на диск, а утром человек не видел
 * НИЧЕГО: ни заходов, ни отказов, ни отложенного до утра, ни самой сводки —
 * ради которой ночной режим и делался.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const HERE = new URL('./', import.meta.url);
const read = (name) => readFileSync(new URL(name, HERE), 'utf8');
const tick = () => new Promise((r) => setTimeout(r, 0));

const BOOK = {
  ok: true,
  current: 'c1',
  hidden: 0,
  chats: [
    { id: 'c1', name: 'Джарвис', named: true, sessionId: 's-1', current: true, turns: 4, at: 1, auto: true },
    { id: 'c2', name: 'Ночной', named: true, sessionId: 's-2', current: false, turns: 9, at: 2, auto: true },
  ],
};
const STATE = (waiting = []) => ({
  ok: true,
  state: {
    chatId: 'c1', active: true, mode: 'ask', sessionId: 's-1', step: 2, maxSteps: 10,
    phase: 'watching', note: '', proposal: null, waiting, visits: [], spend: { known: false },
  },
});

/* Сводка ровно того вида, каким её собирает ядро (chain.rs, morning_digest):
 * пять разделов с фиксированными заголовками, пункты через «•», подвал со
 * счётом непоказанного. Хвост «ещё за ночь» тоже здесь — ядро дописывает туда
 * всё, что не легло в разделы, и пропасть ему нельзя. */
const DIGEST = [
  'Ночная сводка Джарвиса — работа шла, показывать было некому.',
  '',
  'ЧТО СДЕЛАНО (заходов за ночь: 2):',
  '• c1 — заход 3: почини красные тесты (сессия a25d01f8)',
  '• c2 — заход 1: собери отчёт по округам',
  'СКОЛЬКО ПОТРАЧЕНО: 2.10$ за ночь из 3.00$',
  'КУДА ОН УШЁЛ, ПОКА ТЫ СПАЛ:',
  '• c1 — заходов 2, 1.25$; последний (sent): почини тесты → тронул src/a.rs; тесты зелёные',
  '• c2 — заходов 1, 0.85$; последний (sent): собери отчёт → файлов не тронул; проверку не гонял',
  'ЧТО ВСТАЛО И ПОЧЕМУ:',
  '• c1 — Заход не ушёл: пана мертва',
  'ЧТО ЖДЁТ ТЕБЯ:',
  '• пуш в main — отложено до утра (c1, сессия a25d01f8): запушь ветку в main',
  'ЕЩЁ ЗА НОЧЬ:',
  '• c1 — Ночью необратимое не делаю: пуш в main',
  '',
  'Ночью не показал уведомлений: 4',
].join('\n');

const NIGHT = {
  dropped: 0,
  notices: [
    { at: 1, chatId: 'c1', kind: 'sent', text: 'заход 3: почини красные тесты' },
    { at: 2, chatId: 'c2', kind: 'sent', text: 'заход 1: собери отчёт по округам' },
    { at: 3, chatId: 'c1', kind: 'failed', text: 'Заход не ушёл: пана мертва' },
    { at: 4, chatId: 'c1', kind: 'deferred', text: 'Ночью необратимое не делаю: пуш в main' },
  ],
  waiting: [{ chatId: 'c1', sessionId: 'a25d01f8', at: 4, kind: 'пуш в main', prompt: 'запушь ветку в main' }],
};

/* Окно из трея: своей разметкой оно себя и опознаёт. */
async function boot(replies = {}) {
  const { window, document } = parseHTML(read('agent-chat.html'));
  const calls = [];
  const handlers = {};
  window.innerWidth = 900;
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        calls.push([cmd, args]);
        if (replies[cmd]) return replies[cmd](args || {});
        if (cmd === 'agent_chat_state') return { sessionId: 's-1' };
        if (cmd === 'agent_chats_list') return BOOK;
        // Открытый чат называет ядро, а не окно: без этого переключение
        // возвращало бы прежний чат и лента бы не сменилась.
        if (cmd === 'agent_chat_switch') return { ...BOOK, current: (args || {}).chatId };
        if (cmd === 'agent_chat_history') return { ok: true, items: [], total: 0 };
        if (cmd === 'agent_chain_state') return STATE();
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
  await tick();
  return {
    window,
    doc: document,
    calls,
    msgs: document.getElementById('msgs'),
    chain: async (payload) => { await handlers['agent:chain']({ payload }); await tick(); },
    event: async (payload) => { await handlers['agent:event']({ payload }); await tick(); },
    hit: (el) => { el.dispatchEvent(new window.Event('click', { bubbles: true })); return tick().then(tick); },
    did: (cmd) => calls.filter(([c]) => c === cmd).map(([, a]) => a),
  };
}

const chain = (kind, extra = {}) => ({ chatId: 'c1', kind, at: 1000 + kind.length, state: STATE().state, ...extra });
const cards = (s, kind) => [...s.msgs.querySelectorAll('.msg.chain' + (kind ? '.' + kind : ''))];

/* Каждая разновидность события — видимая карточка. Список полный нарочно: в нём
 * ровно то, что ночью копится на диске, и молчаливо пропасть не должно ни одно. */
test('каждый вид события цепочки виден карточкой в ленте', async () => {
  const s = await boot();
  const kinds = [
    ['sent', { step: 3, prompt: 'почини красные тесты' }, /Заход 3: почини красные тесты/],
    ['failed', { text: 'Заход не ушёл: пана мертва' }, /пана мертва/],
    ['stopped', { text: 'Потолок авто-цепочки — 10 заходов подряд' }, /Потолок авто-цепочки/],
    ['cap', { text: 'Дневной потолок этого чата — 10.00$' }, /Дневной потолок/],
    ['deferred', {
      text: 'Ночью необратимое не делаю: пуш в main. Заход готов и ждёт тебя утром',
      waiting: NIGHT.waiting[0],
    }, /ждёт тебя утром/],
    ['proposed', { prompt: 'прогони проверку целиком' }, /прогони проверку целиком/],
  ];
  for (const [kind, extra, re] of kinds) {
    await s.chain(chain(kind, extra));
    const card = cards(s, kind);
    assert.equal(card.length, 1, kind + ': карточки нет вовсе — событие проглочено');
    assert.match(card[0].textContent, re, kind + ': текст события потерян');
  }
  // Служебный срез шапки карточкой не притворяется: он не адресован человеку.
  await s.chain(chain('state'));
  assert.equal(cards(s).length, kinds.length, 'срез шапки нарисовался репликой');
});

/* Метка чата — единственное, что не даёт ночной работе одного проекта лечь в
 * ленту другого. Разъедется здесь — и утром человек прочитает про чужую ночь. */
test('карточка ложится в свой чат, а не в открытый', async () => {
  const s = await boot();
  await s.chain(chain('sent', { chatId: 'c2', step: 1, prompt: 'собери отчёт по округам' }));
  assert.equal(cards(s).length, 0, 'чужая ночная работа в открытой ленте');
  assert.doesNotMatch(s.msgs.textContent, /по округам/);

  const rows = [...s.doc.querySelectorAll('#chats .agchat')];
  await s.hit(rows[1]); // ушли в «Ночной» — там она и ждала
  assert.equal(cards(s, 'sent').length, 1, 'карточка не нашлась в своём чате: ' + s.msgs.textContent);
  assert.match(s.msgs.textContent, /по округам/);
});

/* Утренняя сводка — главная из карточек и самая длинная. Читается она сверху
 * вниз за десять секунд: строка чисел, потом разделы, и раскрыт тот, ради
 * которого её открывают, — «куда он ушёл, пока ты спал». */
test('утренняя сводка размечена разделами и отвечает числами сразу', async () => {
  const s = await boot();
  await s.chain(chain('morning', { digest: DIGEST, night: NIGHT }));
  const box = cards(s, 'morning')[0];
  assert.ok(box, 'утренней сводки нет — ночной режим показывать нечем');

  // числа: заходы, чаты, деньги, вставшее и ждущее решения
  const nums = [...box.querySelectorAll('.chnum')].map((n) => n.textContent);
  assert.deepEqual(nums, ['2 захода', 'в 2 чатах', '2.10$ за ночь из 3.00$', '1 встало', '1 ждёт тебя'], nums.join(' | '));

  // разделы — все, что прислало ядро, включая хвост «ещё за ночь»
  const heads = [...box.querySelectorAll('.chsecname')].map((h) => h.textContent);
  assert.deepEqual(heads, [
    'ЧТО СДЕЛАНО (заходов за ночь: 2)', 'СКОЛЬКО ПОТРАЧЕНО', 'КУДА ОН УШЁЛ, ПОКА ТЫ СПАЛ',
    'ЧТО ВСТАЛО И ПОЧЕМУ', 'ЧТО ЖДЁТ ТЕБЯ', 'ЕЩЁ ЗА НОЧЬ',
  ], heads.join(' | '));

  const open = [...box.querySelectorAll('.chsec.open .chsecname')].map((h) => h.textContent);
  assert.deepEqual(open, ['КУДА ОН УШЁЛ, ПОКА ТЫ СПАЛ', 'ЧТО ЖДЁТ ТЕБЯ'], 'раскрыто не то, за чем пришли: ' + open);

  // разметку рисует общий renderChat: пункты — списком, а не простынёй
  const where = [...box.querySelectorAll('.chsec')][2];
  assert.equal(where.querySelectorAll('li').length, 2, 'пункты раздела не размечены: ' + where.textContent);
  assert.match(where.textContent, /заходов 2, 1\.25\$/);

  // свернуть и раскрыть — руками, а не по одному разу на жизнь карточки
  await s.hit(where.querySelector('.chsechead'));
  assert.equal(where.classList.contains('open'), false, 'раздел не сворачивается');

  // подвал остаётся подвалом, а не пунктом раздела «что ждёт тебя»
  assert.match(box.querySelector('.chfoot').textContent, /Ночью не показал уведомлений: 4/);
  const waits = [...box.querySelectorAll('.chsec')][4];
  assert.doesNotMatch(waits.textContent, /не показал уведомлений/, 'счёт непоказанного уехал в чужой раздел');
});

/* Ручной режим: ядро предложило текст и ждёт. Кнопка — единственная дорога
 * дальше, а правка уходит тем же вызовом: переписывать заход в поле ввода
 * значило бы отправить его обычной репликой не туда. */
test('предложенный заход уходит кнопкой — и с поправкой, если её внесли', async () => {
  const s = await boot();
  await s.chain(chain('proposed', { prompt: 'прогони проверку целиком' }));
  const box = cards(s, 'proposed')[0];
  const area = box.querySelector('.chedit');
  assert.equal(area.value, 'прогони проверку целиком', 'предложенный текст негде прочитать');

  await s.hit(box.querySelector('.agbtn'));
  assert.deepEqual(s.did('agent_chain_send'), [{ chatId: 'c1', text: null }], 'заход не ушёл по кнопке');
  assert.equal(box.querySelectorAll('.agbtn').length, 0, 'кнопка жива у отправленного захода');
  assert.match(box.textContent, /Заход отправлен/);

  await s.chain(chain('proposed', { at: 2000, prompt: 'прогони проверку целиком' }));
  const next = cards(s, 'proposed')[1];
  next.querySelector('.chedit').value = 'сначала почини красные';
  await s.hit(next.querySelector('.agbtn'));
  assert.deepEqual(s.did('agent_chain_send')[1], { chatId: 'c1', text: 'сначала почини красные' }, 'поправка потеряна');
});

/* «Ждёт тебя» — не только карточка минуты, когда это случилось: решение висит и
 * после перезапуска. Шапка берёт его из того же ChainState.waiting. */
test('отложенное до утра видно и в ленте, и в шапке', async () => {
  const s = await boot({ agent_chain_state: () => STATE(NIGHT.waiting) });
  const mark = s.doc.querySelector('.chainwait');
  assert.ok(mark, 'в шапке не сказано, что чат ждёт решения: ' + s.doc.getElementById('sub').textContent);
  assert.match(mark.textContent, /ждёт тебя · 1/);

  await s.hit(mark);
  const pop = s.doc.querySelector('.ctxpop');
  assert.ok(pop, 'что именно отложено — не выяснить');
  assert.match(pop.textContent, /пуш в main: запушь ветку в main/, pop.textContent);

  // а сам заход лежит в карточке целиком: утром решают по нему, а не по пересказу
  await s.chain(chain('deferred', {
    text: 'Ночью необратимое не делаю: пуш в main. Заход готов и ждёт тебя утром',
    waiting: NIGHT.waiting[0],
  }));
  assert.match(cards(s, 'deferred')[0].textContent, /запушь ветку в main/);
});

/* Список чатов перерисовывается на каждое событие потока — и однажды уже сносил
 * этим открытый вопрос про удаление. Карточка цепочки живёт в ленте чата и
 * переживает перерисовку: иначе ночная сводка исчезала бы от чужого ответа. */
test('карточка переживает перерисовку списка чатов', async () => {
  const s = await boot();
  await s.chain(chain('morning', { digest: DIGEST, night: NIGHT }));
  await s.event({ type: 'delta', text: 'сосед пишет', chatId: 'c2' }); // renderChats()
  await s.chain(chain('state', { chatId: 'c2' })); // и ещё раз, уже срезом шапки
  assert.equal(cards(s, 'morning').length, 1, 'сводку снесло перерисовкой списка');
  assert.match(s.msgs.textContent, /КУДА ОН УШЁЛ/);
});

/* Событие уходит ВСЕМ окнам, а лента у чата одна: второй его приход нарисовал
 * бы вторую карточку про один и тот же заход. */
test('одно событие — одна карточка, сколько бы раз оно ни пришло', async () => {
  const s = await boot();
  const ev = chain('sent', { step: 3, prompt: 'почини красные тесты' });
  await s.chain(ev);
  await s.chain(ev);
  await s.chain({ ...ev }); // копия того же события с той же меткой ядра
  assert.equal(cards(s, 'sent').length, 1, 'заход задвоился в ленте');

  // а следующий заход — уже свой: метка ядра у него другая
  await s.chain(chain('sent', { at: 2000, step: 4, prompt: 'прогони проверку' }));
  assert.equal(cards(s, 'sent').length, 2, 'второй заход проглочен вместе с повтором первого');
});

/* Жалоба человека: «спрашивает разрешение на каждый заход». Тест разбирает, ЧТО
 * именно спрашивает, потому что кандидатов два и лечатся они по-разному:
 * карточка гейта (её снимает грант в настройках) и собственное предложение
 * цепочки в режиме «спроси» (её снимает тумблер в шапке чата).
 *
 * Здесь закрепляется второе: в режиме «сам» заход приходит уже отправленным и
 * нажимать нечего, в режиме «спроси» — приходит предложением с кнопкой. */
test('режим «сам»: заход уходит без единой кнопки, режим «спроси»: с кнопкой', async () => {
  const s = await boot();

  // «сам» — ядро прислало УЖЕ отправленный заход
  await s.chain(chain('sent', { step: 1, prompt: 'проверь статус ботов' }));
  const sent = cards(s, 'sent');
  assert.equal(sent.length, 1, 'заход не показан в ленте');
  // Кнопки здесь — div.agbtn, а не <button>: селектор по тегу пропустил бы их
  // и тест был бы зелёным при любом поведении.
  assert.equal(sent[0].querySelectorAll('.agbtn').length, 0,
    'в режиме «сам» у захода есть кнопка — человек снова будет нажимать');
  assert.equal(s.doc.querySelectorAll('.msg.confirm').length, 0,
    'на заход всплыла карточка подтверждения гейта');

  // «спроси» — ядро прислало ПРЕДЛОЖЕНИЕ, отправка только по нажатию
  await s.chain(chain('proposed', { step: 2, prompt: 'посмотри логи' }));
  const prop = cards(s, 'proposed');
  assert.equal(prop.length, 1, 'предложение не показано');
  assert.ok(prop[0].querySelectorAll('.agbtn').length >= 1,
    'в режиме «спроси» отправить нечем — режим стал неотличим от «сам»');
  assert.ok(prop[0].querySelector('textarea.chedit'), 'заход нельзя поправить перед отправкой');

  // И до нажатия ядру ничего не ушло: предложение — это не отправка.
  assert.equal(s.did('agent_chain_send').length, 0, 'предложение уехало само');
});

/* Цепочка уступила сессию человеку. Дефект был конструктивный: в одну сессию
 * писали двое — Джарвис по просьбе человека и механизм цепочки, — и ни один не
 * знал о другом. Карточка обязана дать оба выхода: человек мог зайти на минуту,
 * а мог перехватить сессию насовсем. */
test('пауза цепочки видна карточкой и даёт оба выхода', async () => {
  const s = await boot();
  await s.chain(chain('paused', { sessionId: 's1' }));
  const c = cards(s, 'paused');
  assert.equal(c.length, 1, 'пауза не показана — цепочка «почему-то больше не идёт»');
  const btns = [...c[0].querySelectorAll('.agbtn')].map((b) => b.textContent);
  assert.deepEqual(btns, ['Продолжить цепочку', 'Отменить цепочку']);

  await s.hit(c[0].querySelectorAll('.agbtn')[0]);
  assert.equal(s.did('agent_chain_resume').length, 1, 'продолжение не уехало в ядро');
  assert.match(c[0].textContent, /Цепочка продолжена/);
});

test('отмена цепочки из карточки паузы рвёт именно цепочку', async () => {
  const s = await boot();
  await s.chain(chain('paused', { sessionId: 's1' }));
  const c = cards(s, 'paused')[0];
  await s.hit(c.querySelectorAll('.agbtn')[1]);
  assert.equal(s.did('agent_chain_stop').length, 1, 'отмена не уехала в ядро');
  assert.equal(s.did('agent_chain_resume').length, 0, 'заодно продолжили');
  assert.match(c.textContent, /Цепочка отменена/);
});
