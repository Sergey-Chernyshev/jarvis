import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const source = readFileSync(new URL('./toast.js', import.meta.url), 'utf8');
const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
function boot(overrides = {}) {
  const { window, document } = parseHTML('<html><body><div id="stack"></div></body></html>');
  const events = {}, calls = [], timers = [];
  const api = {
    resize: async () => {}, audioState: async () => null, meetingStatus: async () => null,
    systemAccessibilitySettings: async () => { calls.push(['settings']); return { ok: true }; },
    dictationCancelInsertion: async id => { calls.push(['cancel', id]); return { ok: true, cancelled: true }; },
    copy: async text => { calls.push(['copy', text]); },
    voiceAbort: () => calls.push(['abort']), openVoiceHistory: () => calls.push(['history']), ...overrides,
  };
  window.toast = new Proxy(api, { get(target, name) {
    if (name in target) return target[name];
    if (name.startsWith('on')) return callback => { events[name] = callback; };
    return () => Promise.resolve();
  } });
  new Function('window', 'document', 'setTimeout', 'clearTimeout', 'setInterval', source)(window, document,
    (fn, ms) => { const timer = { fn, ms }; timers.push(timer); return timer; }, timer => { if (timer) timer.cleared = true; }, () => 0);
  return { document, calls, timers, send: payload => events.onVoiceHud(payload) };
}
const heard = {
  id: 'voice-hud', phase: 'heard', title: 'Услышал', body: 'Сокращённый…',
  full: 'Проверь Jarvis.\n\nНе меняй 12,50 RUB.', rawText: 'проверь джарвис новый абзац не меняй 12,50 RUB',
  formatted: true, copied: true, inserted: false, pasteSent: false,
  insertionBlocked: 'accessibility', insertionAttemptId: 21,
  insertionError: 'Для автоматической вставки нужен Универсальный доступ.',
};

test('permission region precedes full transcript and does not masquerade as recognized speech', async () => {
  const h = boot(); h.send(heard); await flush();
  const regions = [...h.document.querySelectorAll('.hud-permission, .hud-transcript')];
  assert.equal(regions[0].className, 'hud-permission');
  assert.equal(regions[1].textContent, heard.full);
  assert.equal(h.document.querySelector('.hud-transcript-label').textContent, 'Отформатировано');
  assert.equal(h.timers.some(timer => timer.ms === 5000), false, 'permission recovery stays available');
  assert.equal(h.document.querySelector('.hud-raw-text').textContent, heard.rawText);
});

test('permission button opens settings only; cancellation preserves both copy paths', async () => {
  const h = boot(); h.send(heard); await flush();
  h.document.querySelector('.hud-permission-allow').click(); await flush();
  assert.deepEqual(h.calls, [['settings']]);
  h.document.querySelector('.hud-permission-cancel').click(); await flush();
  assert.deepEqual(h.calls[1], ['cancel', 21]);
  assert.equal(h.document.querySelector('.hud-transcript').textContent, heard.full);
  assert.equal(h.document.querySelector('.hud-permission-title').textContent, 'Автовставка отменена');
  [...h.document.querySelectorAll('button')].find(button => button.textContent === 'Копировать').click();
  h.document.querySelector('.hud-raw-copy').click(); await flush();
  assert.deepEqual(h.calls.slice(2), [['copy', heard.full], ['copy', heard.rawText]]);
});

test('late cancellation acknowledgement cannot alter the next dictation', async () => {
  let finish;
  const h = boot({ dictationCancelInsertion: () => new Promise(resolve => { finish = resolve; }) });
  h.send(heard); await flush(); h.document.querySelector('.hud-permission-cancel').click();
  h.send({ ...heard, insertionAttemptId: 22, body: 'Следующая', full: 'Следующая', rawText: 'Следующая' });
  finish({ ok: true, cancelled: true }); await flush();
  assert.equal(h.document.querySelector('.voice').dataset.attemptId, '22');
  assert.equal(h.document.querySelector('.hud-permission-title').textContent, 'Для вставки нужен Универсальный доступ');
  assert.equal(h.document.querySelector('.hud-transcript').textContent, 'Следующая');
});

test('failed cancellation is visible and unsafe transcript markup stays text', async () => {
  const h = boot({ dictationCancelInsertion: async () => ({ ok: false }) });
  h.send({ ...heard, full: '<script>steal()</script>' }); await flush();
  h.document.querySelector('.hud-permission-cancel').click(); await flush();
  assert.match(h.document.querySelector('.hud-permission-help').textContent, /Не удалось подтвердить отмену/);
  assert.equal(h.document.querySelector('script'), null);
  assert.equal(h.document.querySelector('.hud-transcript').textContent, '<script>steal()</script>');
});
