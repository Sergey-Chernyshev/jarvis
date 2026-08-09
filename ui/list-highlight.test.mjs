/* Выделение в списке оконного режима.
 *
 * Список там стоит рядом с открытым чатом, и раньше жил по правилам накладки:
 * перерисовка только в виде «список», выделение — только за клавиатурным
 * курсором. Итог: чат открыт, а в списке он никак не отмечен, и весь список
 * заморожен до выхода из чата. Тесты держат три опоры починки. */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const src = readFileSync(new URL('./renderer.js', import.meta.url), 'utf8');
const render = src.slice(src.indexOf('function render()'), src.indexOf('/* ---------- чат сессии ----------'));

test('перерисовку списка запрещает только его невидимость', () => {
  assert.ok(render.includes('if (listEl.hidden) return;'), 'гейт по видимости пропал');
  assert.ok(!render.includes("if (view !== 'list') return;"),
    'вернулась проверка по имени вида — в окне список снова замёрзнет');
});

test('открытый чат ведёт за собой курсор списка', () => {
  assert.ok(/openId.*chatSessionId/.test(render), 'выделение не связано с открытым чатом');
  assert.ok(/findIndex\(\(x\) => x\.id === openId\)/.test(render), 'строка открытого чата не ищется');
});

test('клик по строке ставит курсор на неё', () => {
  assert.ok(render.includes('sel = i; openSession(s);'),
    'клик открывает чат, но курсор остаётся где был');
});
