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

/* Длинное сообщение в ленте.
 *
 * Отчёт исполнителя обрывался ровно на таблице: финального вердикта человек не
 * видел вовсе, и об обрыве никто не сказал. Обрезка была в бэкенде (снята), а
 * здесь тесты держат вторую половину обещания — что лента не подрежет пузырь
 * молча уже на своей стороне. */

import { createRequire } from 'node:module';
import { parseHTML } from 'linkedom';

const require = createRequire(import.meta.url);
const MD = require('./markdown.js');
const html = readFileSync(new URL('./index.html', import.meta.url), 'utf8');

/** Тот самый отчёт из РЕАЛЬНОГО wire.jsonl kimi-сессии (таблица + текст после). */
function realReport() {
  const raw = readFileSync(
    new URL('../src-tauri/tests/fixtures/kimi-wire.jsonl', import.meta.url),
    'utf8',
  );
  let best = '';
  for (const line of raw.split('\n')) {
    if (!line.trim()) continue;
    let v;
    try { v = JSON.parse(line); } catch { continue; }
    const part = v?.event?.part;
    if (part?.type === 'text' && part.text.length > best.length) best = part.text;
  }
  return best;
}

test('длинный отчёт доезжает до конца — и таблица, и текст после неё', () => {
  const text = realReport();
  assert.ok(text.length > 7000, `в фикстуре нет длинного отчёта (${text.length})`);
  const { document } = parseHTML('<div id="root"></div>');
  globalThis.document = document;
  const root = document.createElement('div');
  MD.renderChat(root, text);

  assert.ok(root.querySelector('table'), 'таблица отрисована');
  assert.ok(root.textContent.includes('Три строки:'), 'текст ПОСЛЕ таблицы не потерян');
  // рендер не роняет содержимое: разметка уходит, слова остаются
  assert.ok(root.textContent.length > text.length * 0.8, 'слишком много текста исчезло при рендере');
});

test('пузырь не подрезан вёрсткой — иначе обрыв был бы не виден', () => {
  const bubble = html.slice(html.indexOf('.msg.assistant'), html.indexOf('.chatcut'));
  assert.ok(!/-webkit-line-clamp/.test(bubble), 'line-clamp в пузыре прячет хвост молча');
  assert.ok(!/max-height/.test(bubble), 'max-height в пузыре прячет хвост молча');
});

test('renderer кладёт текст реплики в пузырь целиком', () => {
  const at = src.indexOf('function assistantMsg(');
  const msg = src.slice(at, src.indexOf('\nfunction ', at + 1));
  assert.match(msg, /renderMarkdown\(bubble, it\.text\)/, 'текст обязан уходить в рендер целиком');
  assert.ok(!/it\.text\.slice\(/.test(msg), 'срез текста реплики — молчаливая потеря');
});
