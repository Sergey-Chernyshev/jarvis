/* Доставка ответа из тоста — в настоящем DOM окна тостов.
 *
 * Ноутбук проснулся, пана мертва: question_answer отвечает {ok:false}, а
 * карточка исчезала ровно так же, как при успехе, — человек уходил уверенным,
 * что ответил. Ровно для этого случая («сессия оборвалась во сне») и сделана
 * кнопка «Продолжить», то есть отказ там — норма, а не край.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const HERE = new URL('./', import.meta.url);
const read = (name) => readFileSync(new URL(name, HERE), 'utf8');

const settle = async () => { for (let i = 0; i < 6; i++) await new Promise((r) => setTimeout(r, 0)); };

function boot(replies) {
  const { window, document } = parseHTML(read('toast.html'));
  const handlers = {};
  const calls = [];
  const reply = (name) => (...args) => {
    calls.push([name, ...args]);
    const v = replies[name];
    return Promise.resolve(typeof v === 'function' ? v(...args) : (v === undefined ? { ok: true } : v));
  };
  window.toast = {
    onAdd: (cb) => { handlers.add = cb; },
    onUpdate: () => {}, onHover: () => {}, onHold: () => {}, onExtend: () => {},
    onRemove: () => {}, onVoiceHud: () => {}, onAudioState: () => {},
    resize: () => {}, click: () => {},
    answerQuestion: reply('answerQuestion'),
    continueSession: reply('continueSession'),
  };
  window.requestAnimationFrame = (cb) => setTimeout(() => cb(0), 0);
  const fn = new Function(
    'window', 'document', 'setTimeout', 'clearTimeout', 'requestAnimationFrame', 'Promise',
    read('toast.js'),
  );
  fn(window, document, setTimeout, clearTimeout, window.requestAnimationFrame, Promise);
  return { window, doc: document, handlers, calls };
}

const click = (doc, node) => node.dispatchEvent(new doc.defaultView.Event('click', { bubbles: true }));

const QUESTION = {
  id: 'q-s1', sessionId: 's1', kind: 'waiting', title: 'Куда деплоить?', body: '',
  question: { count: 1, options: [{ label: 'На стенд' }, { label: 'В прод' }] },
};

test('мёртвая пана: карточка вопроса остаётся и называет причину', async () => {
  const { doc, handlers } = boot({ answerQuestion: { ok: false, error: 'Пана сессии не отвечает' } });
  handlers.add(QUESTION);
  await settle();

  click(doc, doc.querySelector('.opt'));
  await settle();

  assert.equal(doc.querySelectorAll('.card').length, 1, 'карточка исчезла, будто ответ ушёл');
  assert.equal(doc.querySelectorAll('.card.out').length, 0, 'карточка уезжает с экрана');
  assert.equal(doc.querySelectorAll('.derr').length, 1, 'причина отказа не показана');
  assert.ok(doc.querySelector('.derr').textContent.includes('Пана сессии не отвечает'));
});

test('удачный ответ по-прежнему снимает карточку', async () => {
  const { doc, handlers, calls } = boot({ answerQuestion: { ok: true } });
  handlers.add(QUESTION);
  await settle();

  click(doc, doc.querySelectorAll('.opt')[1]);
  await settle();

  assert.deepEqual(calls[0], ['answerQuestion', 's1', { answers: [[2]] }]);
  assert.equal(doc.querySelectorAll('.derr').length, 0, 'на успехе показали ошибку');
  assert.equal(doc.querySelectorAll('.card.out').length, 1, 'карточка не уехала с экрана');
});

test('«Продолжить» на сессии вне tmux: причина, команда и «Повторить»', async () => {
  const { doc, handlers } = boot({
    continueSession: { ok: false, needsTmux: true, resumeCmd: 'claude --resume abc123' },
  });
  handlers.add({ id: 'w-s1', sessionId: 's1', kind: 'waiting', title: 'checkout-flow', body: 'ждёт' });
  await settle();

  const cont = doc.querySelector('.cont');
  assert.equal(cont.textContent, 'Продолжить');
  click(doc, cont);
  await settle();

  assert.equal(doc.querySelectorAll('.card').length, 1, 'карточка исчезла без доставки');
  const err = doc.querySelector('.derr').textContent;
  assert.ok(err.includes('вне tmux'), 'причина не названа: ' + err);
  assert.ok(err.includes('claude --resume abc123'), 'команда возобновления потеряна: ' + err);
  assert.equal(cont.textContent, 'Повторить', 'повторить нечем');
  assert.equal(cont.disabled, false);
});

/* Мост отвечает отказом промиса (окно тостов пережило перезапуск демона). */
test('упавший вызов моста тоже виден, а не съеден', async () => {
  const { doc, handlers } = boot({ continueSession: () => Promise.reject(new Error('канал закрыт')) });
  handlers.add({ id: 'w-s2', sessionId: 's2', kind: 'limit', title: 'jarvis', body: 'лимит' });
  await settle();

  click(doc, doc.querySelector('.cont'));
  await settle();

  assert.equal(doc.querySelectorAll('.card').length, 1);
  assert.ok(doc.querySelector('.derr').textContent.includes('канал закрыт'));
});
