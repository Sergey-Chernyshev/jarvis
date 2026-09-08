import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
const window = {};
new Function('window', readFileSync(new URL('./navigation.js', import.meta.url), 'utf8'))(window);

test('back returns each visited module and its original search and selection', () => {
  const nav = window.createJarvisNavigation();
  const home = { view: 'home', query: 'проект', moduleSelection: 'module:history' };
  nav.go({ view: 'history' }, home);
  const project = { view: 'history', query: 'api', history: { machine: 'vm', project: '/work/api', selected: 2 } };
  nav.go({ view: 'chat', sessionId: 'one' }, project);
  nav.go({ view: 'settings' }, { view: 'chat', sessionId: 'one', draft: 'Keep this unsent' });
  assert.equal(nav.back().draft, 'Keep this unsent');
  assert.deepEqual(nav.back(), project);
  assert.deepEqual(nav.back(), home);
});

test('refreshing a page does not add phantom Escape steps', () => {
  const nav = window.createJarvisNavigation();
  nav.go({ view: 'meetings' });
  nav.go({ view: 'meetings' });
  nav.go({ view: 'meetings' });
  assert.equal(nav.depth(), 1);
  assert.equal(nav.back().view, 'home');
});

test('different chats have distinct navigation identity', () => {
  const nav = window.createJarvisNavigation();
  nav.go({ view: 'chat', sessionId: 'one' });
  nav.go({ view: 'chat', sessionId: 'two' });
  assert.equal(nav.back().sessionId, 'one');
  assert.equal(nav.back().view, 'home');
});

test('explicit home navigation resets old paths and snapshots cannot mutate history', () => {
  const nav = window.createJarvisNavigation();
  const original = { view: 'home', query: 'voice' };
  nav.go({ view: 'voicehist' }, original);
  original.query = 'changed';
  assert.equal(nav.back().query, 'voice');
  nav.go({ view: 'settings' });
  nav.go({ view: 'home' });
  assert.equal(nav.depth(), 0);
  assert.deepEqual(nav.back(), { view: 'home' });
});
