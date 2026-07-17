import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const html = readFileSync(new URL('./index.html', import.meta.url), 'utf8');
const renderer = readFileSync(new URL('./renderer.js', import.meta.url), 'utf8');

test('session chat is a single transcript feed without the legacy summary mode', () => {
  assert.match(html, /<div class="chatlog" id="chatlog"><\/div>/);

  assert.doesNotMatch(html, /\bchatModeSeg\b/);
  assert.doesNotMatch(html, /\bchatSummary\b/);
  assert.doesNotMatch(html, /\.chatmode-(?:seg|btn)\b/);
  assert.doesNotMatch(html, /\.chatsummary(?:-stub)?\b/);
  assert.doesNotMatch(html, /Саммари сессии — заготовка дизайна/);

  assert.doesNotMatch(renderer, /\bchatModeSeg\b/);
  assert.doesNotMatch(renderer, /\bchatSummaryEl\b/);
  assert.doesNotMatch(renderer, /\bsetChatMode\b/);
});
