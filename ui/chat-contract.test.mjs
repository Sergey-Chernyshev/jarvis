import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const html = readFileSync(new URL('./index.html', import.meta.url), 'utf8');
const renderer = readFileSync(new URL('./renderer.js', import.meta.url), 'utf8');

// Сводки ходов (chatsum v1, возврат по спеке 2026-07-18): чат — одна лента,
// поверх которой живёт режим «Сводка» — тумблер в шапке, карточки .turnsum
// и подписка на chat:summary. Тест стережёт НАЛИЧИЕ этой поверхности.
test('session chat has the turn-summary surface on top of the transcript feed', () => {
  // тумблер «Сводка/Лента» в шапке чата + стили карточек и режима
  assert.match(html, /id="sumToggle"/);
  assert.match(html, /\.turnsum\b/);
  assert.match(html, /#chatlog\.sum \.turn\.done \.turnraw/);

  // рендерер: группировка ленты в ходы, карточки, тумблер, событие демона
  assert.match(renderer, /\bfunction startTurn\(/);
  assert.match(renderer, /\bfunction applyCard\(/);
  assert.match(renderer, /\bsummaryModeOn\b/);
  assert.match(renderer, /getElementById\('sumToggle'\)/);
  assert.match(renderer, /onChatSummary\(/);
});

// Легаси-заготовка «саммари сессии» (chatModeSeg/chatSummaryEl/setChatMode)
// не возвращается: v1 её заменил тумблером сводок, а не воскресил.
test('legacy summary-mode stub stays absent', () => {
  assert.doesNotMatch(html, /\bchatModeSeg\b/);
  assert.doesNotMatch(html, /\.chatmode-(?:seg|btn)\b/);
  assert.doesNotMatch(html, /Саммари сессии — заготовка дизайна/);

  assert.doesNotMatch(renderer, /\bchatModeSeg\b/);
  assert.doesNotMatch(renderer, /\bchatSummaryEl\b/);
  assert.doesNotMatch(renderer, /\bsetChatMode\b/);
});
