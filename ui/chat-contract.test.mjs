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

// Слой «Документы», инкремент 2 (спека 2026-07-18 §3.1/§3.3): вьюер документа
// в панели — слайд-овер, безопасный markdown-рендер, открытие с файл-чипа.
test('doc viewer surface exists on top of the summary cards', () => {
  assert.match(html, /id="docWrap"/);
  assert.match(html, /id="docBody"/);
  assert.match(html, /markdown\.js/);

  assert.match(renderer, /\bfunction openDocViewer\(/);
  assert.match(renderer, /JarvisMarkdown\.render\(/);
  assert.match(renderer, /JarvisMarkdown\.isDocPath\(/); // doc-чипы первыми + CTA
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

// Голоса диалога разведены формой и стороной (без второй краски): человек —
// клеверный пузырь у правого края, агент — текст на бумаге слева. Обе стороны
// одинаковым текстом уже были — диалог читался как монолог.
test('user and agent messages are visually distinct voices', () => {
  assert.match(html, /\.msg\.user \.bubble \{[^}]*background: var\(--accent-soft\)/s);
  assert.match(html, /\.msg\.user \{[^}]*align-self: flex-end/s);
  // у агента подложки нет — его голос остаётся текстом на бумаге
  assert.doesNotMatch(html, /\.msg\.assistant \.bubble \{[^}]*background: var\(--accent/s);
});
