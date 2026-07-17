import { test } from 'node:test';
import assert from 'node:assert/strict';
import DiffView from './diffview.js';

test('пустой дифф — нет строк', () => {
  assert.deepEqual(DiffView.rows([]), []);
  assert.deepEqual(DiffView.rows(null), []);
});

test('удаление и добавление: классы и нумерация гутеров', () => {
  const r = DiffView.rows([
    { old_start: 1, new_start: 1, lines: [
      { t: ' ', s: 'один' },
      { t: '-', s: 'два' },
      { t: '+', s: 'ДВА' },
      { t: ' ', s: 'три' },
    ] },
  ]);
  // контекст «один» = old 1 / new 1
  assert.deepEqual(r[0], { cls: 'diff-ctx', oldNo: 1, newNo: 1, mark: ' ', s: 'один' });
  // удалённая «два» — old 2, new null
  assert.deepEqual(r[1], { cls: 'diff-del', oldNo: 2, newNo: null, mark: '-', s: 'два' });
  // добавленная «ДВА» — old null, new 2
  assert.deepEqual(r[2], { cls: 'diff-add', oldNo: null, newNo: 2, mark: '+', s: 'ДВА' });
  // «три» = old 3 / new 3 (удаление сдвинуло только old, добавление — только new)
  assert.deepEqual(r[3], { cls: 'diff-ctx', oldNo: 3, newNo: 3, mark: ' ', s: 'три' });
});

test('между ханками — полоса пропущенных строк с верным числом', () => {
  const r = DiffView.rows([
    { old_start: 1, new_start: 1, lines: [{ t: ' ', s: 'a' }, { t: '+', s: 'b' }] },
    { old_start: 10, new_start: 11, lines: [{ t: '-', s: 'z' }] },
  ]);
  // после 1-го ханка old-курсор = 2 (одна контекстная строка), след. old 10 → gap 8
  const gap = r.find((x) => x.gap);
  assert.deepEqual(gap, { gap: true, count: 8 });
});

test('склонение числа строк', () => {
  assert.equal(DiffView.plural(1), 'строка');
  assert.equal(DiffView.plural(3), 'строки');
  assert.equal(DiffView.plural(8), 'строк');
  assert.equal(DiffView.plural(21), 'строк'); // грубое склонение — 21 как «строк», ок для полосы
});

test('renderTo строит узлы без innerHTML и экранирует содержимое', () => {
  // минимальный поддельный DOM: проверяем, что текст уходит в textContent,
  // а не парсится как HTML (renderTo не трогает innerHTML вовсе)
  function fakeDoc() {
    const mk = () => ({
      className: '', textContent: '', children: [],
      appendChild(c) { this.children.push(c); return c; },
      ownerDocument: null,
    });
    const doc = { createElement: mk };
    return doc;
  }
  const doc = fakeDoc();
  const root = { ownerDocument: doc, textContent: '', children: [], appendChild(c) { this.children.push(c); return c; } };
  DiffView.renderTo(root, [
    { old_start: 1, new_start: 1, lines: [{ t: '+', s: '<script>alert("x")</script>' }] },
  ]);
  // единственный узел-строка, содержимое в textContent дочернего span, дословно
  const row = root.children[0];
  const sSpan = row.children.find((c) => c.className === 'diff-s');
  assert.equal(sSpan.textContent, '<script>alert("x")</script>');
});
