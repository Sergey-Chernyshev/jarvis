import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

function boot(stop) {
  const { window, document } = parseHTML('<html><body><div id="stack"></div></body></html>');
  const events = {};
  let finishStatus;
  window.toast = new Proxy({
    meetingStop: stop,
    meetingStatus: () => new Promise(resolve => { finishStatus = resolve; }),
    resize: () => {},
    audioState: async () => ({}),
  }, { get(target, key) {
    if (key in target) return target[key];
    if (key.startsWith('on')) return fn => { events[key] = fn; };
    return () => Promise.resolve();
  } });
  window.jarvisIcons = { create: () => document.createElement('i') };
  new Function('window', 'document', 'requestAnimationFrame', 'setInterval', 'setTimeout', 'clearTimeout',
    readFileSync(new URL('./toast.js', import.meta.url), 'utf8'))(
      window, document, fn => fn(), () => 0, fn => { fn(); return 0; }, () => {});
  return { document, window, events, finishStatus };
}
const recording = { id: 'meeting-1', title: 'Встреча', status: 'recording', startedAt: Date.now(), durationMs: 1000 };

test('late Stop result cannot resurrect a completed meeting HUD', async () => {
  let finishStop;
  const h = boot(() => new Promise(resolve => { finishStop = resolve; }));
  h.events.onMeetingChanged(recording);
  h.document.querySelector('.meeting-hud .cont').click();
  h.events.onMeetingChanged({ ...recording, status: 'ready' });
  finishStop({ ...recording, status: 'transcribing' });
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(h.document.querySelector('.meeting-hud'), null);
});

test('Stop remains disabled across recording updates and only sends once', async () => {
  let calls = 0;
  const h = boot(() => { calls++; return new Promise(() => {}); });
  h.events.onMeetingChanged(recording);
  h.document.querySelector('.meeting-hud .cont').click();
  h.events.onMeetingChanged({ ...recording, durationMs: 2000 });
  const button = h.document.querySelector('.meeting-hud .cont');
  assert.equal(button.disabled, true);
  button.click();
  assert.equal(calls, 1);
});

test('late empty initial status does not dismiss an active recording', async () => {
  const h = boot(async () => ({}));
  h.events.onMeetingChanged(recording);
  h.finishStatus(null);
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.ok(h.document.querySelector('.meeting-hud'));
});
