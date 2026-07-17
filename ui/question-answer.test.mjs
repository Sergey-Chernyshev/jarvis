import assert from 'node:assert/strict';
import test from 'node:test';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const QA = require('./question-answer.js');

// --- поле доступно только для Claude (у codex-пикера строки Other нет) ---

test('custom answer is hidden for codex sessions only', () => {
  assert.equal(QA.customAllowed('codex'), false);
  assert.equal(QA.customAllowed('claude'), true);
  assert.equal(QA.customAllowed(undefined), true); // без метки = claude (back-compat)
});

// --- нормализация текста ---

test('blank input is not a custom answer', () => {
  assert.equal(QA.normalizeText(''), null);
  assert.equal(QA.normalizeText('   '), null);
  assert.equal(QA.normalizeText(null), null);
  assert.equal(QA.normalizeText('  да  '), 'да');
});

// --- выбор по вопросу (commitRow) ---

test('single-select without custom keeps the highlighted option', () => {
  const res = QA.commitRow({ multiSelect: false, chosen: new Set(), sel: 1, text: '' });
  assert.deepEqual(res, { row: [2], text: null });
});

test('single-select custom overrides the option — it is the Other choice', () => {
  const res = QA.commitRow({ multiSelect: false, chosen: new Set(), sel: 1, text: 'свой ответ' });
  assert.deepEqual(res, { row: [], text: 'свой ответ' });
});

test('multi-select combines toggles with custom text', () => {
  const res = QA.commitRow({ multiSelect: true, chosen: new Set([3, 1]), sel: 0, text: 'и ещё' });
  assert.deepEqual(res, { row: [1, 3], text: 'и ещё' });
});

test('multi-select with custom only is a valid answer', () => {
  const res = QA.commitRow({ multiSelect: true, chosen: new Set(), sel: 0, text: 'только текст' });
  assert.deepEqual(res, { row: [], text: 'только текст' });
});

test('multi-select without toggles and text is rejected', () => {
  assert.equal(QA.commitRow({ multiSelect: true, chosen: new Set(), sel: 0, text: '  ' }), null);
});

// --- payload (back-compat со старым контрактом) ---

test('payload without customs is byte-for-byte the legacy contract', () => {
  assert.deepEqual(QA.buildPayload([[2], [1]], [null, null]), { answers: [[2], [1]] });
  assert.deepEqual(QA.buildPayload([[1]], []), { answers: [[1]] });
});

test('payload carries texts aligned to questions when any custom present', () => {
  assert.deepEqual(
    QA.buildPayload([[], [1, 3]], ['свой ответ', null]),
    { answers: [[], [1, 3]], texts: ['свой ответ', null] },
  );
});
