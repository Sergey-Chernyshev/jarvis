import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const html = readFileSync(new URL("./index.html", import.meta.url), "utf8");
const renderer = readFileSync(
  new URL("./renderer.js", import.meta.url),
  "utf8",
);

// Сводки ходов (chatsum v1, возврат по спеке 2026-07-18): чат — одна лента,
// поверх которой живёт режим «Сводка» — тумблер в шапке, карточки .turnsum
// и подписка на chat:summary. Тест стережёт НАЛИЧИЕ этой поверхности.
test("session chat has the turn-summary surface on top of the transcript feed", () => {
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

// Карточка хода — ИИ-разбор результатов, а не список действий
// (спека 2026-07-18-turn-ai-analysis-redesign): финальный ответ агента под
// сворачиваемым блоком; строка команд и tool-лог из карточки убраны.
test("turn card is an AI analysis with a collapsible agent reply, no command line", () => {
  // сворачиваемый «Ответ агента» из card.reply
  assert.match(renderer, /card\.reply/);
  assert.match(renderer, /Ответ агента/);
  assert.match(renderer, /tsum-reply/);
  // фактовые остатки старой карточки в UI не рендерятся
  assert.doesNotMatch(renderer, /card\.docs_digest/);
  assert.doesNotMatch(renderer, /card\.commands/);
  assert.doesNotMatch(renderer, /\btsum-cmds\b/);
});

// Слой «Документы», инкремент 2 (спека 2026-07-18 §3.1/§3.3): вьюер документа
// в панели — слайд-овер, безопасный markdown-рендер, открытие с файл-чипа.
test("doc viewer surface exists on top of the summary cards", () => {
  assert.match(html, /id="docWrap"/);
  assert.match(html, /id="docBody"/);
  assert.match(html, /markdown\.js/);

  assert.match(renderer, /\bfunction openDocViewer\(/);
  assert.match(renderer, /JarvisMarkdown\.render\(/);
  assert.match(renderer, /JarvisMarkdown\.isDocPath\(/); // doc-чипы первыми + CTA
});

// Легаси-заготовка «саммари сессии» (chatModeSeg/chatSummaryEl/setChatMode)
// не возвращается: v1 её заменил тумблером сводок, а не воскресил.
test("legacy summary-mode stub stays absent", () => {
  assert.doesNotMatch(html, /\bchatModeSeg\b/);
  assert.doesNotMatch(html, /\.chatmode-(?:seg|btn)\b/);
  assert.doesNotMatch(html, /Саммари сессии — заготовка дизайна/);

  assert.doesNotMatch(renderer, /\bchatModeSeg\b/);
  assert.doesNotMatch(renderer, /\bchatSummaryEl\b/);
  assert.doesNotMatch(renderer, /\bsetChatMode\b/);
});

// Лента действий не должна заслонять разговор: видно два чипа, остальное —
// за тихой сворачивающейся строкой. Тулзы остаются контекстом, но чат
// читается как чат (просьба владельца 2026-08-05).
test("tool chips collapse behind a quiet toggle instead of flooding the feed", () => {
  assert.match(renderer, /const TOOLS_VISIBLE = \d+/);
  assert.match(renderer, /function paintToolsToggle\(/);
  assert.match(renderer, /function toolsToggle\(/);
  // переключатель именно кнопка с aria-expanded, а не div — доступность
  assert.match(renderer, /aria-expanded/);
  // склонение числа действий: «1 действие», «2 действия», «5 действий»
  assert.match(renderer, /function plural\(/);
  assert.match(html, /\.msg\.tools \.tools-more/);
  assert.match(html, /\.tools-more:focus-visible/);
});

// Сводка — это саммаризация, а не лог: карточка хода не перечисляет тулзы.
test("turn summary prompt asks for the outcome, not a list of actions", () => {
  const turns = readFileSync(
    new URL("../src-tauri/src/turns.rs", import.meta.url),
    "utf8",
  );
  assert.match(turns, /Пиши про СУТЬ и ИТОГ, а не перечисляй действия/);
});
