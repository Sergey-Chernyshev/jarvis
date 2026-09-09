/* Оживление мёртвых сессий в окне чата: карточка цены и строка о расходе.
 *
 * Почему эти две вещи проверяются вместе. Владелец решил, что джарвисы
 * воскрешают сессии БЕЗ подтверждения, — и это убрало карточку, единственное
 * место, где человек видел цену до траты. Взамен цену держат два механизма, и
 * оба живут здесь, в интерфейсе: крупное всё равно спрашивает (карточка с
 * ценой), а прошедшее молча ложится строкой в ленту (расход после факта).
 * Сломайся любой из них — и оживление станет тратой, о которой человек узнаёт
 * из выписки провайдера. Ровно этого допустить нельзя.
 *
 * Окна не трогаем: разметка поднимается в linkedom, дёргаются обработчики.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';

import { mountAgentChat, cardOf, cardBtn, armCard, tick } from './headless.mjs';

const MB = 1_048_576;

/** Карточка подтверждения оживления, как её шлёт демон. */
const reviveCard = (over = {}) => ({
  nonce: 'n-revive',
  id: 'sessions.resume',
  class: 'control',
  provenance: 'trusted',
  card: {
    kind: 'revive',
    sessionId: '3e819d75-6d94-458f-bda4-3f778ba5386c',
    agent: 'claude',
    label: 'FastWorkBot',
    cwd: '~/PycharmProjects/FastWorkBot',
    contextTokens: 310579,
    bytes: 37.4 * MB,
    cost: { kind: 'known', usd: 2.33, model: 'opus' },
    reason: 'нужен контекст вчерашнего разбора платежей',
    ...over,
  },
});

test('карточка оживления показывает цену, а не id сессии', async () => {
  const w = await mountAgentChat();
  await w.confirm(reviveCard());

  const box = cardOf(w.doc);
  assert.ok(box, 'карточки оживления нет вовсе');
  const text = box.textContent;

  // Заголовок называет действие словами: «sessions.resume» человеку не говорит,
  // на что он соглашается, а решает он именно это.
  assert.match(text, /оживить мёртвую сессию/, 'действие не названо словами: ' + text);

  // Цена — то, ради чего карточка и показывается.
  assert.match(text, /\$2\.33/, 'цена первого хода не показана: ' + text);
  assert.match(text, /311k контекста/, 'объём контекста не показан: ' + text);
  assert.match(text, /37\.4 МБ/, 'размер транскрипта не показан: ' + text);
  assert.match(text, /FastWorkBot/, 'не сказано, какую именно сессию поднимают');

  // Зачем — единственное, чего из файла не узнать.
  assert.match(text, /Зачем: нужен контекст вчерашнего разбора платежей/, 'причина потеряна');

  // …а сырой id не показывается вовсе: он не отвечает ни на один вопрос,
  // который человек здесь задаёт.
  assert.doesNotMatch(text, /3e819d75/, 'вместо цены человеку показали uuid');
});

/* Человек нажал тумблер «оживлять без подтверждения» — и всё равно получил
 * карточку. Без объяснения это выглядит как «разрешение не работает», и первое,
 * что он сделает, — пойдёт чинить тумблер. */
test('карточка сверх гранта объясняет, почему спросили вопреки разрешению', async () => {
  const w = await mountAgentChat();
  await w.confirm(reviveCard({ beyondGrant: 'первый ход обойдётся примерно в $2.33 при пороге $1.00' }));
  const text = cardOf(w.doc).textContent;
  assert.match(text, /хотя оживление разрешено/, 'не сказано, почему вопрос вообще задан: ' + text);
  assert.match(text, /при пороге \$1\.00/, 'порог, который перерос транскрипт, не назван');
});

/* Три состояния цены, и сводить их к одному числу нельзя: подставленный ноль
 * или самая дешёвая ставка соврали бы ровно в том месте, ради которого
 * карточка и существует. */
test('неизвестную цену карточка называет неизвестной, а не нулём', async () => {
  const w = await mountAgentChat();
  await w.confirm(reviveCard({ cost: { kind: 'unknown' }, contextTokens: null }));
  const text = cardOf(w.doc).textContent;
  assert.match(text, /определить не удалось/, 'неизвестная цена подана как известная: ' + text);
  assert.doesNotMatch(text, /\$0\.00/, 'неизвестное выдано за бесплатное');
});

test('незнакомая модель даёт вилку цен, а не самую дешёвую ставку', async () => {
  const w = await mountAgentChat();
  await w.confirm(reviveCard({ cost: { kind: 'range', usdLow: 0.31, usdHigh: 1.55 } }));
  const text = cardOf(w.doc).textContent;
  assert.match(text, /\$0\.31–\$1\.55/, 'вилка не показана: ' + text);
  assert.match(text, /модель в транскрипте не названа/, 'не сказано, почему цена вилкой');
});

/* Транскрипт мог исчезнуть, пока карточка ехала. Молчать нельзя: человек
 * согласился бы на оживление того, чего нет. */
test('исчезнувший транскрипт карточка называет исчезнувшим', async () => {
  const w = await mountAgentChat();
  await w.confirm(reviveCard({ gone: true }));
  assert.match(cardOf(w.doc).textContent, /на диске больше нет/, 'пропажа проглочена');
});

/* Карточка оживления — обычная карточка гейта, и защита от подброшенного
 * нажатия на ней та же: согласие требует признаков человека, отказ проходит
 * всегда. Своей копии кнопок у оживления нет и не должно быть. */
test('согласие на оживление требует признаков человека, отказ — нет', async () => {
  const w = await mountAgentChat();
  await w.confirm(reviveCard());
  const box = cardOf(w.doc);

  // слепой клик по «Разрешить» — карточка остаётся ждать и говорит, чего не хватило
  cardBtn(w.doc, 'yes').dispatchEvent(new w.window.Event('click', { bubbles: true }));
  await tick();
  assert.equal(w.calls.some(([c]) => c === 'agent_confirm'), false,
    'подброшенный клик согласился за человека — оживление ушло без него');
  assert.match(box.textContent, /нажатие не принято/, 'отказ в согласии проглочен молча');

  // а с признаками человека — уходит, и уходит как согласие
  await armCard(w.window, box);
  cardBtn(w.doc, 'yes').dispatchEvent(new w.window.Event('click', { bubbles: true }));
  await tick();
  const call = w.calls.find(([c]) => c === 'agent_confirm');
  assert.ok(call, 'человеческое согласие не дошло до демона');
  assert.equal(call[1].approved, true);
  assert.equal(call[1].armed, true, 'признак человека не доехал — демон отклонит согласие');
});

/* ── Строка о расходе: вторая половина решения «без подтверждения» ──────── */

test('оживление без карточки всё равно оставляет строку в ленте', async () => {
  const w = await mountAgentChat();
  await w.fire('agent:resumed', {
    sessionId: '3e819d75',
    label: 'FastWorkBot',
    agent: 'claude',
    reason: 'нужен контекст вчерашнего разбора платежей',
    text: 'Оживил «FastWorkBot» (claude) в ~/PycharmProjects/FastWorkBot. '
      + 'Зачем: нужен контекст вчерашнего разбора платежей. '
      + 'Контекст 310579 токенов, 489 реплик, 37.4 МБ — первый ход по холодному кэшу ~$2.33 (opus).',
    at: 1000,
  });

  const row = w.doc.querySelector('#msgs .msg .chbox.revived');
  assert.ok(row, 'оживление прошло молча — человек узнает о расходе из выписки провайдера');
  const text = row.textContent;
  assert.match(text, /сессия оживлена/, 'строка не подписана');
  assert.match(text, /FastWorkBot/, 'не сказано, что подняли');
  assert.match(text, /Зачем: нужен контекст/, 'не сказано, зачем подняли');
  assert.match(text, /\$2\.33/, 'не сказано, сколько это стоило');
});

/* Событие приходит и во вкладку, и в отдельное окно, а при переподписке —
 * дважды в одно. Две строки об одном оживлении читались бы как два оживления,
 * то есть как двойной расход. */
test('одно оживление — одна строка, сколько бы раз событие ни пришло', async () => {
  const w = await mountAgentChat();
  const ev = { sessionId: 'sid-1', label: 'Проект', text: 'Оживил «Проект».', at: 42 };
  await w.fire('agent:resumed', ev);
  await w.fire('agent:resumed', ev);
  assert.equal(w.doc.querySelectorAll('#msgs .msg .chbox.revived').length, 1,
    'одно оживление показано дважды — выглядит как двойной расход');
});

test('событие без текста строки не рисует', async () => {
  const w = await mountAgentChat();
  await w.fire('agent:resumed', { sessionId: 'sid-1' });
  assert.equal(w.doc.querySelectorAll('#msgs .msg .chbox.revived').length, 0,
    'пустая строка «сессия оживлена» без единого числа хуже, чем её отсутствие');
});
