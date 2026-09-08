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

const request = { requestId: 'rpc-4', revision: 2, transport: 'tmux', questions: [
  { id: 'storage', options: [{ id: 'local', label: 'Local' }, { id: 'vm', label: 'VM' }] },
  { id: 'notes', options: [], customAllowed: true },
] };

test('payload binds every answer to the exact request, revision and option identities', () => {
  assert.deepEqual(QA.buildPayload([[2], []], ['', 'Строка 1\nСтрока 2'], request, 'send-12'), {
    requestId: 'rpc-4', revision: 2, submissionId: 'send-12', answers: [
      { questionId: 'storage', optionIds: ['vm'], text: null },
      { questionId: 'notes', optionIds: [], text: 'Строка 1\nСтрока 2' },
    ],
  });
});

test('custom is a per-question capability, including Codex notes and text-only questions', () => {
  assert.equal(QA.customAllowed('codex', { customAllowed: true }), true);
  assert.equal(QA.customAllowed('claude', { customAllowed: false }), false);
  assert.equal(QA.customAllowed('claude', {}, { fromScreen: true }), false);
  assert.deepEqual(QA.commitRow({ multiSelect: false, sel: 0, chosen: new Set(), text: 'Почему', customMode: 'notes', optionCount: 2 }), { row: [1], text: 'Почему' });
  assert.deepEqual(QA.commitRow({ multiSelect: false, sel: 0, chosen: new Set(), text: 'Свободный ответ', optionCount: 0 }), { row: [], text: 'Свободный ответ' });
  assert.equal(QA.commitRow({ multiSelect: false, sel: 0, chosen: new Set(), text: '', optionCount: 0 }), null);
});

test('drafts survive back/reopen but never leak into another request or owning session', () => {
  const store = QA.createDraftStore();
  const first = store.get('vm:chat', request);
  first.texts[1] = '  первая строка\nвторая  '; first.index = 1; first.selections[0] = 1;
  assert.equal(store.get('vm:chat', request), first);
  assert.equal(store.get('vm:chat', request).texts[1], '  первая строка\nвторая  ');
  assert.deepEqual(store.get('chat', request).texts, ['', '']);
  assert.deepEqual(store.get('vm:chat', { ...request, revision: 3 }).texts, ['', '']);
  store.delete('vm:chat', request);
  assert.notEqual(store.get('vm:chat', request).submissionId, first.submissionId);
});
