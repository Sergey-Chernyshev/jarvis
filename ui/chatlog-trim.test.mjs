/* Ограничение ленты чата.
 *
 * Лента росла всю сессию: у агента, работающего часами, накапливались десятки
 * тысяч узлов, и каждое добавление пересчитывало вёрстку по всей куче — чат
 * тем медленнее, чем дольше на него смотришь. Тест держит два свойства:
 * лента ограничена сверху и срез никогда не уносит текущий ход. */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const src = readFileSync(new URL('./renderer.js', import.meta.url), 'utf8');
const body = src.slice(src.indexOf('function trimChatlog()'), src.indexOf('function appendChatItems'));

/** Минимальный DOM: только то, чем пользуется срез. */
function fakeLog() {
  const kids = [];
  return {
    kids,
    get childElementCount() { return kids.length; },
    get firstElementChild() { return kids[0] || null; },
    add(node) { const n = node || { remove() { kids.splice(kids.indexOf(n), 1); } }; kids.push(n); return n; },
  };
}

/** Выполняем настоящий код среза поверх подставного лога. */
function runTrim(chatlogEl, curTurn, max) {
  const fn = new Function('chatlogEl', 'curTurn', 'CHATLOG_MAX_BLOCKS', `${body}; trimChatlog();`);
  fn(chatlogEl, curTurn, max);
}

test('лента не растёт бесконечно', () => {
  const log = fakeLog();
  for (let i = 0; i < 50; i++) log.add();
  runTrim(log, null, 10);
  assert.equal(log.childElementCount, 10);
});

test('срез не уносит ход, в который сейчас пишут', () => {
  const log = fakeLog();
  for (let i = 0; i < 5; i++) log.add();
  const live = log.kids[0]; // текущий ход оказался самым старым
  runTrim(log, { wrap: live }, 1);
  assert.ok(log.kids.includes(live), 'текущий ход унесли — следующие реплики пропали бы молча');
});

test('короткая лента не трогается', () => {
  const log = fakeLog();
  for (let i = 0; i < 3; i++) log.add();
  runTrim(log, null, 400);
  assert.equal(log.childElementCount, 3);
});
