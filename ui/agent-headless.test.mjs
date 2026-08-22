/* Логика взаимодействия окна чата — целиком без окна.
 *
 * Всё, что здесь проверяется, до сих пор проверяли мышью в ЖИВОМ окне
 * приложения, где параллельно работал человек. Так делать нельзя никогда:
 * синтетический клик уходит в чужой сеанс. Обвязка ui/headless.mjs поднимает
 * настоящую разметку и настоящий agent-chat.js и дёргает обработчики напрямую
 * — тот же код, та же ветка, только без окна и без чужого сеанса.
 *
 * Вёрстки здесь нет и быть не может: linkedom не считает геометрию и не
 * применяет CSS. Что именно остаётся человеку и скриншотам — сказано в шапке
 * ui/headless.mjs.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mountAgentChat, makeDaemon, cardOf, cardBtn, tick } from './headless.mjs';

const THREE = () => makeDaemon(['Первый', 'Второй', 'Третий']);

/* ── 1. Порядок строк в колонке ──────────────────────────────────────────────
 *
 * Жалоба повторялась трижды: «активный чат уползает наверх». Требование
 * человека жёсткое и простое: АКТИВНОСТЬ НИКОГДА НЕ ДВИГАЕТ СТРОКУ. Порядок
 * задаёт только человек — перетаскиванием, и только он переживает перерисовку.
 * ────────────────────────────────────────────────────────────────────────────*/

test('ответ в неактивном чате не двигает его строку', async () => {
  const d = THREE();
  const w = await mountAgentChat({ daemon: d });
  assert.deepEqual(w.names(), ['Первый', 'Второй', 'Третий'], 'порядок на старте — как у демона');

  // во втором пошёл ответ: самая свежая активность из всех трёх
  await w.emit({ type: 'delta', text: 'пишу', chatId: 'c2' });
  assert.deepEqual(w.names(), ['Первый', 'Второй', 'Третий'], 'дельта в чужой чат сдвинула строку');

  await w.emit({ type: 'done', result: 'готово', session_id: 's2', chatId: 'c2' });
  assert.deepEqual(w.names(), ['Первый', 'Второй', 'Третий'], 'завершение хода сдвинуло строку');

  // и отказ — тоже активность, и он тоже не повод переезжать
  await w.emit({ type: 'failed', message: 'агент упал', chatId: 'c3' });
  assert.deepEqual(w.names(), ['Первый', 'Второй', 'Третий'], 'ошибка в чужом чате сдвинула строку');
});

test('переключение чата не двигает строки', async () => {
  const d = THREE();
  const w = await mountAgentChat({ daemon: d });

  await w.open('Третий');
  assert.deepEqual(w.names(), ['Первый', 'Второй', 'Третий'], 'открытие подняло строку наверх');
  assert.equal(d.current, 'c3', 'демону не сказали, что чат сменился');

  await w.open('Первый');
  assert.deepEqual(w.names(), ['Первый', 'Второй', 'Третий'], 'возврат в первый чат переставил колонку');
});

test('новый чат встаёт в конец, а не наверх', async () => {
  const d = THREE();
  const w = await mountAgentChat({ daemon: d });

  await w.addChat();
  assert.deepEqual(w.names(), ['Первый', 'Второй', 'Третий', 'Новый чат'],
    'новый чат встал не в конец — полка молча переехала под рукой');
  assert.equal(d.order().length, 4, 'демон о новом чате не узнал');

  // и второй новый — тоже в конец, за первым
  await w.addChat();
  assert.deepEqual(w.names().slice(-2), ['Новый чат', 'Новый чат']);
  assert.equal(d.order()[4], 'c5', 'второй новый чат влез не за первым');
});

test('перетаскивание меняет порядок и записывает его демону', async () => {
  const d = THREE();
  const w = await mountAgentChat({ daemon: d });

  await w.drag('Второй', 'Первый'); // тащим второй на место первого
  assert.deepEqual(w.names(), ['Второй', 'Первый', 'Третий'], 'строки не переставились');
  assert.deepEqual(d.nameOrder(), ['Второй', 'Первый', 'Третий'],
    'новый порядок не записан — после перезапуска вернётся старый');
  assert.deepEqual(w.invoked('agent_chat_reorder'), [{ chatId: 'c2', toIndex: 0 }]);
});

test('порядок, заданный рукой, переживает перерисовку и чужую активность', async () => {
  const d = THREE();
  const w = await mountAgentChat({ daemon: d });
  await w.drag('Третий', 'Первый');
  assert.deepEqual(w.names(), ['Третий', 'Первый', 'Второй'], 'перетаскивание не сработало вовсе');

  await w.rerender();
  assert.deepEqual(w.names(), ['Третий', 'Первый', 'Второй'],
    'перерисовка вернула порядок демона — рука человека забыта');

  // активность в самом нижнем чате: ни она, ни перерисовка после неё не двигают полку
  await w.emit({ type: 'delta', text: 'привет', chatId: 'c2' });
  await w.emit({ type: 'done', result: 'готово', session_id: 's2', chatId: 'c2' });
  await w.rerender();
  assert.deepEqual(w.names(), ['Третий', 'Первый', 'Второй'],
    'после активности и перерисовки порядок съехал');
});

/* ── 2. Черновики ────────────────────────────────────────────────────────────
 *
 * Поле ввода одно на все чаты. Текст, написанный одному Джарвису, оставался в
 * поле и уезжал в тот, который открыли следующим. Чаты раздают промпты в сессии
 * с доступом к файлам — промах адресатом тут не «неловко», а «ушло не туда и
 * там исполнилось».
 * ────────────────────────────────────────────────────────────────────────────*/

test('черновики двух чатов не смешиваются и переживают переключение', async () => {
  const d = makeDaemon(['Первый', 'Второй']);
  const w = await mountAgentChat({ daemon: d });

  await w.type('промпт для первого');
  await w.open('Второй');
  assert.equal(w.input.value, '', 'чужой черновик приехал в соседний чат — так и промахиваются адресатом');

  await w.type('промпт для второго');
  await w.open('Первый');
  assert.equal(w.input.value, 'промпт для первого', 'свой черновик не вернулся — набранное потеряно');

  await w.open('Второй');
  assert.equal(w.input.value, 'промпт для второго', 'черновик соседа не пережил круга по колонке');
});

test('курсор возвращается туда, где стоял, а не в конец строки', async () => {
  const d = makeDaemon(['Первый', 'Второй']);
  const w = await mountAgentChat({ daemon: d });

  await w.type('начало и хвост', 6); // человек стоит посередине строки
  await w.open('Второй');
  await w.type('другой текст с другим курсором', 20); // курсор соседа сюда приехать не должен
  await w.open('Первый');

  assert.equal(w.input.value, 'начало и хвост');
  assert.equal(w.input.selectionStart, 6, 'курсор уехал в конец — дописывать будут не туда, куда смотрели');
  assert.equal(w.input.selectionEnd, 6);
});

test('черновик чистит только удавшаяся отправка', async () => {
  const d = makeDaemon(['Первый', 'Второй']);
  const w = await mountAgentChat({ daemon: d });

  // отказ приезжает РАЗРЕШЁННЫМ промисом — текст обязан остаться у человека
  d.sendResult = () => ({ ok: false, error: 'claude не найден' });
  await w.type('дорогой промпт', 4);
  await w.send();
  assert.equal(w.input.value, 'дорогой промпт', 'поле пусто, а сообщение не ушло — набирать заново');
  assert.equal(w.input.selectionStart, 4, 'текст вернули, а курсор нет');

  // и упавший мост — тоже не повод стирать
  d.sendResult = () => { throw new Error('мост умер'); };
  await w.send();
  assert.equal(w.input.value, 'дорогой промпт', 'исключение съело набранный текст');

  d.sendResult = (a) => ({ ok: true, chatId: a.chatId });
  await w.send();
  assert.equal(w.input.value, '', 'удавшаяся отправка поле не очистила');
  assert.equal(d.drafts.has('c1'), false, 'отправленное осталось черновиком — строка списка будет врать');
});

/* Черновик набираем во ВТОРОМ чате и уходим в первый: так он лежит на диске и
 * ничем не держится за поле ввода — дальше с ним делают что-то со стороны. */
async function withDraftInSecond() {
  const d = makeDaemon(['Первый', 'Второй']);
  const w = await mountAgentChat({ daemon: d });
  await w.open('Второй');
  await w.type('черновик второго');
  await w.blur(); // уход фокуса дожимает черновик на диск, не дожидаясь полусекунды
  await w.open('Первый');
  assert.equal(d.drafts.get('c2')?.text, 'черновик второго', 'черновика не было — проверять нечего');
  return { d, w };
}

test('скрытие чата черновик оставляет — оно обратимо', async () => {
  const { d, w } = await withDraftInSecond();

  await w.menu('Второй', 'Скрыть');
  assert.deepEqual(w.names(), ['Первый'], 'скрытый чат остался в колонке — скрытие не сработало');
  assert.equal(d.drafts.get('c2')?.text, 'черновик второго',
    'скрытие унесло набранное — обратимость обещана, а текста нет');
});

test('забвение чата уносит и черновик — он был репликой именно в него', async () => {
  const { d, w } = await withDraftInSecond();

  await w.menu('Второй', 'Забыть насовсем');
  const yes = [...w.doc.querySelectorAll('#chats .agbtn')].find((b) => b.textContent === 'Удалить');
  assert.ok(yes, 'вопрос про необратимое удаление не задан');
  await w.hit(yes);

  assert.equal(d.drafts.has('c2'), false, 'разговора нет, а черновик в него остался');
});

/* ── 3. Маршрутизация событий по чатам ───────────────────────────────────────
 *
 * У каждого чата своя лента. Событие помечено chatId, и попасть оно обязано
 * ровно в свой тред — независимо от того, какой чат сейчас на экране.
 * ────────────────────────────────────────────────────────────────────────────*/

const TWO = () => makeDaemon({
  names: ['Джарвис', 'Выборы'],
  history: {
    c1: [{ role: 'user', text: 'это c1' }],
    c2: [{ role: 'user', text: 'это c2' }],
  },
});

test('дельта чужой сессии не протекает в открытый чат', async () => {
  const w = await mountAgentChat({ daemon: TWO() });
  await w.emit({ type: 'delta', text: 'ответ про выборы', chatId: 'c2' });

  assert.doesNotMatch(w.text(), /ответ про выборы/, 'ответ соседнего чата дорисован в открытый');
  assert.deepEqual(w.bubbles(), [], 'в открытой ленте появился чужой пузырь');

  // а в своей ленте он есть — событие не потеряно, а разложено
  await w.open('Выборы');
  assert.match(w.text(), /ответ про выборы/, 'ответ не доехал и в свой чат — событие просто потеряли');
});

test('два ответа в полёте не смешиваются даже вперемежку', async () => {
  const d = TWO();
  const w = await mountAgentChat({ daemon: d });
  await w.say('первый вопрос');
  await w.open('Выборы');
  await w.say('второй вопрос');

  for (const [text, chatId] of [['один-', 'c1'], ['два-', 'c2'], ['один', 'c1'], ['два', 'c2']]) {
    await w.emit({ type: 'delta', text, chatId });
  }
  assert.deepEqual(w.bubbles(), ['два-два'], 'в ленте второго чата чужие куски');

  await w.emit({ type: 'done', result: '', session_id: 's2', chatId: 'c2' });
  await w.open('Джарвис');
  assert.deepEqual(w.bubbles(), ['один-один'], 'ответ первого чата собрался не из своих дельт');
});

test('ошибка уходит в свой тред и не пугает соседний', async () => {
  const d = TWO();
  const w = await mountAgentChat({ daemon: d });
  await w.say('посмотри округ');
  await w.open('Выборы');

  // упал ПЕРВЫЙ чат, а на экране второй
  await w.emit({ type: 'failed', message: 'claude не отозвался', chatId: 'c1' });
  assert.doesNotMatch(w.text(), /claude не отозвался/,
    'чужая ошибка нарисована в открытой ленте — человек чинит не тот чат');
  assert.equal(w.doc.getElementById('send').disabled, false, 'чужая ошибка заперла поле здорового чата');

  // вернулись к себе — ошибка на месте, а не проглочена
  await w.open('Джарвис');
  assert.match(w.text(), /claude не отозвался/, 'ошибка потеряна: чат молча остался без ответа');
});

test('занятость держится за своим чатом, а не за экраном', async () => {
  const d = TWO();
  const w = await mountAgentChat({ daemon: d });
  await w.say('посмотри округ');
  assert.deepEqual(w.busy(), ['Джарвис'], 'занятость чата не читается в списке');

  await w.open('Выборы');
  assert.deepEqual(w.busy(), ['Джарвис'], 'про отвечающий чат забыли, едва ушли из него');
  assert.equal(w.sub(), 'готов', '«думает…» осталось от соседнего разговора');

  await w.emit({ type: 'done', result: 'готово', session_id: 's1', chatId: 'c1' });
  assert.deepEqual(w.busy(), [], 'занятость висит на закончившем разговоре');
});

/* ── 4. Карточки подтверждения ───────────────────────────────────────────────
 *
 * Карточка — согласие человека на действие с эффектом. Он обязан понимать, на
 * что соглашается и ЧТО ИЗ ЭТОГО ВЫШЛО: четыре исхода демона — разные вещи, и
 * выдавать один за другой нельзя.
 * ────────────────────────────────────────────────────────────────────────────*/

const ASK = (over = {}) => ({
  nonce: 'n-1', id: 'sessions.control', class: 'effect',
  card: { kind: 'session', label: 'Выборы', model: 'opus', effort: 'high' },
  ...over,
});

test('четыре исхода демона различимы, и ни один не выдаётся за согласие', async () => {
  const cases = [
    ['rejected', /отклонено/, /разрешено/],
    ['expired', /время вышло[\s\S]*не выполнено/, /отклонено|✓/],
    ['stale', /цель изменилась[\s\S]*не выполнено/, /отклонено|✓/],
    ['approved', /разрешено/, /не выполнено|отклонено/],
  ];
  for (const [outcome, want, nope] of cases) {
    const w = await mountAgentChat({ daemon: makeDaemon(['Джарвис']) });
    await w.confirm(ASK());
    const box = cardOf(w.doc);
    assert.ok(box, outcome + ': карточка не нарисована вовсе');

    await w.humanPress('yes');
    // до слова демона карточка ничего не обещает
    assert.doesNotMatch(box.textContent, /разрешено|отклонено/, outcome + ': исход объявлен раньше демона');

    await w.confirmDone({ nonce: 'n-1', approved: outcome === 'approved', outcome });
    assert.match(box.textContent, want, outcome + ': исход назван не своими словами');
    assert.doesNotMatch(box.textContent, nope, outcome + ': сказано лишнее');
  }
});

test('повторное нажатие не шлёт демону второе решение', async () => {
  const w = await mountAgentChat({ daemon: makeDaemon(['Джарвис']) });
  await w.confirm(ASK());
  const no = cardBtn(w.doc, 'no');

  /* Признаки человека набираем заранее — иначе не пройдёт и первое нажатие, и
   * тест про повтор проверял бы совсем другое. */
  await w.armCard();

  /* Двойной щелчок: два click подряд, до того как решение доехало до демона.
   * Именно так по карточке и попадают — она узкая, а ответ не мгновенный. */
  const yes = cardBtn(w.doc, 'yes');
  yes.dispatchEvent(new w.window.Event('click', { bubbles: true }));
  yes.dispatchEvent(new w.window.Event('click', { bubbles: true }));
  await tick();
  await tick();

  assert.deepEqual(w.invoked('agent_confirm'), [{ nonce: 'n-1', approved: true, armed: true }],
    'демон получил решение дважды — действие с эффектом выполнится два раза');

  // и передумать после отправки уже нельзя: кнопок нет, а нажатие по старой ничего не шлёт
  assert.equal(cardOf(w.doc).querySelectorAll('.cbtn').length, 0, 'кнопки живы у отправленного решения');
  no.dispatchEvent(new w.window.Event('click', { bubbles: true }));
  await tick();
  await tick();
  assert.equal(w.invoked('agent_confirm').length, 1, 'по снятой кнопке ушло второе, противоположное решение');
});

test('«Разрешить» по истёкшему вопросу не превращается в «разрешено»', async () => {
  const d = makeDaemon(['Джарвис']);
  d.confirmResult = () => ({ ok: false }); // нонса в реестре уже нет: гейт истёк, пока карточка ждала
  const w = await mountAgentChat({ daemon: d });
  await w.confirm(ASK());
  await w.humanPress('yes');

  const box = cardOf(w.doc);
  assert.equal(box.querySelectorAll('.cbtn').length, 0, 'кнопки живы у решённого вопроса');
  assert.doesNotMatch(box.textContent, /разрешено/, 'обещано разрешение, которого демон не принял');
  assert.match(box.textContent, /не выполнено/);
});

/* ── 5. Согласие не подделать слепым кликом ──────────────────────────────────
 *
 * Дыра, ради которой headless-прогон и заведён: проверяющий CLI слал
 * синтетические клики в ЖИВОЕ окно, где в этот момент работал человек. Такой
 * клик способен нажать «Разрешить» и согласиться за человека — то есть обойти
 * ровно тот гейт, через который агент спрашивает разрешение.
 *
 * Асимметрия намеренная: ОТКАЗ проходит всегда. Запертое «Отклонить» оставило
 * бы человека наедине с карточкой, которую нечем закрыть.
 * ────────────────────────────────────────────────────────────────────────────*/

test('слепой клик не соглашается за человека и не съедает карточку', async () => {
  const w = await mountAgentChat({ daemon: makeDaemon(['Джарвис']) });
  await w.confirm(ASK());
  await w.blindPress('yes'); // без фокуса, без движений курсора, сразу после появления

  assert.deepEqual(w.invoked('agent_confirm'), [],
    'подброшенный клик согласился за человека — гейт разрешений обойдён');

  const box = cardOf(w.doc);
  assert.ok(box, 'карточка исчезла — вопрос съеден, а решения по нему не было');
  assert.ok(box.querySelector('.cbtn.yes'), 'кнопки сняты: человеку больше нечем согласиться');
  assert.doesNotMatch(box.textContent, /✓|разрешено/, 'слепой клик показан как разрешение');
  assert.match(box.querySelector('.cresult').textContent, /нажатие не принято/,
    'нажали — и ничего: человеку не сказано, почему');
  assert.match(box.querySelector('.cresult').textContent, /нажмите ещё раз/,
    'не сказано, что делать дальше');
});

test('после слепого клика настоящее нажатие человека проходит', async () => {
  const w = await mountAgentChat({ daemon: makeDaemon(['Джарвис']) });
  await w.confirm(ASK());
  await w.blindPress('yes');
  assert.deepEqual(w.invoked('agent_confirm'), [], 'слепой клик прошёл');

  // карточка жива — человек подводит курсор и жмёт сам
  await w.humanPress('yes');
  assert.deepEqual(w.invoked('agent_confirm'), [{ nonce: 'n-1', approved: true, armed: true }],
    'после неудачного слепого клика человек согласиться уже не может');
});

test('отказать слепым кликом можно — иначе карточку нечем закрыть', async () => {
  const w = await mountAgentChat({ daemon: makeDaemon(['Джарвис']) });
  await w.confirm(ASK());
  await w.blindPress('no'); // те же условия, что у не прошедшего согласия

  assert.deepEqual(w.invoked('agent_confirm'), [{ nonce: 'n-1', approved: false, armed: true }],
    'отказ заперт признаками человека — человек остался наедине с карточкой');
  assert.equal(cardOf(w.doc).querySelectorAll('.cbtn').length, 0, 'кнопки живы у отклонённого вопроса');
});

test('окна не хватает: поднятого прямо сейчас окна мало для согласия', async () => {
  const w = await mountAgentChat({ daemon: makeDaemon(['Джарвис']) });
  await w.confirm(ASK());

  // окно подняли и в то же мгновение щёлкнули — почерк синтетики, а не человека
  w.window.dispatchEvent(new w.window.Event('focus'));
  await w.blindPress('yes');

  assert.deepEqual(w.invoked('agent_confirm'), [], 'клик в только что поднятое окно согласился за человека');
  assert.match(cardOf(w.doc).querySelector('.cresult').textContent, /нажатие не принято/);
});

/* Само правило — чистая функция, и проверяется без всякого DOM: синтетические
 * пробы в живом окне запрещены, и правило про них обязано жить по этому же
 * правилу. */
test('правило согласия называет каждую нехватку своими словами', async () => {
  const w = await mountAgentChat({ daemon: makeDaemon(['Джарвис']) });
  const { armWhy, ARM_MS, ARM_SPOTS } = w.arm();
  const spots = (n) => new Set(Array.from({ length: n }, (_, i) => i + ':' + i));
  const now = 10_000_000;
  const old = now - ARM_MS - 1;

  assert.match(armWhy({ born: now, spots: spots(ARM_SPOTS) }, now, old), /только появилась/);
  assert.match(armWhy({ born: old, spots: spots(ARM_SPOTS - 1) }, now, old), /курсор к кнопке не подводили/);
  assert.match(armWhy({ born: old, spots: spots(ARM_SPOTS) }, now, 0), /окно не активно/);
  assert.match(armWhy({ born: old, spots: spots(ARM_SPOTS) }, now, now), /окно только что подняли/);
  assert.equal(armWhy({ born: old, spots: spots(ARM_SPOTS) }, now, old), null,
    'нажатие со всеми признаками человека всё равно не принято');
  assert.match(armWhy(null, now, old), /не отслеживалась/, 'карточка без слежки принята за человеческую');
});
