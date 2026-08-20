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
    },
  };
  const fn = new Function('window', 'document', 'globalThis', read('agent-chat.js'));
  fn(window, document, window);
  await tick();
  const emit = async (payload) => { await handlers['agent:event']({ payload }); await tick(); };
  const say = async (text) => {
    document.getElementById('input').value = text;
    document.getElementById('send').dispatchEvent(new window.Event('click', { bubbles: true }));
    await tick();
  };
  const sent = () => calls.filter(([c]) => c === 'agent_send').map(([, a]) => a);
  return { window, doc: document, calls, emit, say, sent };
}

const text = (doc) => doc.getElementById('msgs').textContent;

test('после перезапуска окна разговор продолжается, а не начинается заново', async () => {
  const { doc, say, sent } = await boot({ sessionId: 's-42' });

  assert.equal(doc.getElementById('tag').hidden, false, 'нет метки продолжения — человек гадает');
  assert.match(text(doc), /Продолжаю прошлый разговор/);

  await say('привет');
  assert.deepEqual(sent(), [{ message: 'привет', sessionId: 's-42' }]);
});

test('без сохранённого разговора окно молчит про продолжение', async () => {
  const { doc, say, sent } = await boot({ sessionId: null });
  assert.equal(doc.getElementById('tag').hidden, true);
  assert.doesNotMatch(text(doc), /Продолжаю прошлый разговор/);
  await say('привет');
  assert.deepEqual(sent(), [{ message: 'привет', sessionId: null }]);
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

test('свежий id из потока перекрывает восстановленный', async () => {
  const { emit, say, sent } = await boot({ sessionId: null });
  await say('привет');
  await emit({ type: 'init', tools: [], model: 'claude-sonnet-4-5', session_id: 's-new' });
  await emit({ type: 'done', result: 'ок', session_id: 's-new' });
  await say('второе');
  assert.equal(sent()[1].sessionId, 's-new');
});
