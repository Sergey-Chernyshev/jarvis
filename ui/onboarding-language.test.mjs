/* Язык первого запуска: экран «готово» и экран отказа.
 *
 * Установка прошла, «Система готова» — а в уже открытом терминале, где claude
 * запущен, ничего не появляется: хуки снимаются снапшотом на старте сессии, а
 * PATH-блок не виден текущему шеллу. CLI это печатает, README тоже — тот, кто
 * ставит из DMG, не видит ни того, ни другого.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const HERE = new URL('./', import.meta.url);
const read = (name) => readFileSync(new URL(name, HERE), 'utf8');

async function boot(patch) {
  const { window, document } = parseHTML(read('onboarding.html'));
  const snapshot = {
    coreReady: true,
    agents: [{ id: 'claude', label: 'Claude Code', ready: true, available: true, detail: '' }],
    transport: [{ id: 'socket', label: 'socket', ready: true, available: true, detail: '' }],
    capabilities: [],
    warnings: [],
    proxyConfigured: false,
    job: { state: 'idle', kind: '', tasks: [], steps: [], failures: [] },
    ...patch,
  };
  window.__TAURI__ = {
    core: { invoke: async (cmd) => (cmd === 'onboarding_get' ? snapshot : { ok: true }) },
    event: { listen: async () => () => {} },
  };
  window.requestAnimationFrame = (cb) => setTimeout(() => cb(0), 0);
  for (const name of ['onboarding-state.js', 'onboarding.js']) {
    const fn = new Function(
      'window', 'document', 'globalThis', 'navigator', 'setTimeout', 'requestAnimationFrame', 'setInterval', 'clearInterval',
      read(name),
    );
    fn(window, document, window, { userAgent: 'Macintosh; Mac OS X' }, setTimeout, window.requestAnimationFrame, () => 0, () => {});
  }
  for (let i = 0; i < 6; i++) await new Promise((r) => setTimeout(r, 0));
  if (document.querySelector('.rail-step[data-screen="ready"]') && !document.getElementById('content').textContent.includes('Загрузка остановилась')) document.querySelector('.rail-step[data-screen="ready"]').dispatchEvent(new window.Event('click', { bubbles: true }));
  return { doc: document };
}

test('экран «готово» говорит перезапустить открытые сессии и шелл', async () => {
  const { doc } = await boot({});
  const text = doc.getElementById('content').textContent;
  assert.ok(/перезапусти/i.test(text), 'про перезапуск сессий ни слова: ' + text);
  assert.ok(text.includes('exec zsh'), 'про PATH в текущем шелле ни слова');
  // счётчик внизу — по-русски и со склонением, а не «1 agents · 0 local modules»
  assert.ok(text.includes('1 агент'), 'счётчик агентов не по-русски: ' + text);
  assert.equal(/agents|local modules|online\b/.test(text), false, 'английские подписи остались');
});

test('экран отказа говорит, что произошло и что нажать — без внутренних слов', async () => {
  const { doc } = await boot({
    job: { state: 'failed', kind: 'models', tasks: ['silero'], steps: [], failures: ['dns lookup failed'] },
  });
  const text = doc.getElementById('content').textContent;
  assert.ok(text.includes('Загрузка остановилась'), 'причина не названа: ' + text);
  assert.ok(doc.getElementById('primary').textContent.includes('Повторить'), 'не сказано, что нажать');
  for (const jargon of ['идемпотент', 'runtime', 'Recovery', 'operational logs', 'trust for hooks']) {
    assert.equal(text.includes(jargon), false, 'внутреннее слово осталось: ' + jargon);
  }
});
