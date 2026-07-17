import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('./toast.js', import.meta.url), 'utf8');

class FakeElement {
  constructor() {
    this.children = [];
    this.classList = {
      add() {},
      remove() {},
      toggle() {},
    };
    this.style = { setProperty() {} };
    this.scrollHeight = 100;
  }

  append(...children) { this.children.push(...children); }
  appendChild(child) { this.children.push(child); return child; }
  addEventListener() {}
  getBoundingClientRect() { return { top: 0, bottom: 100 }; }
  querySelector() { return null; }
  remove() {}
  removeChild(child) {
    const index = this.children.indexOf(child);
    if (index >= 0) this.children.splice(index, 1);
    return child;
  }
  setAttribute() {}
  get firstChild() { return this.children[0] || null; }
}

function createHarness() {
  const handlers = {};
  const delays = [];
  const stack = new FakeElement();
  const toast = {
    onHover: (cb) => { handlers.hover = cb; },
    onAdd: (cb) => { handlers.add = cb; },
    onRemove: (cb) => { handlers.remove = cb; },
    onHold: (cb) => { handlers.hold = cb; },
    onExtend: (cb) => { handlers.extend = cb; },
    onUpdate: (cb) => { handlers.update = cb; },
    onVoiceHud: (cb) => { handlers.voiceHud = cb; },
    onAudioState: (cb) => { handlers.audioState = cb; },
    resize() {},
    ready() {},
  };
  const context = {
    clearTimeout() {},
    document: {
      createElement: () => new FakeElement(),
      createElementNS: () => new FakeElement(),
      createTextNode: (text) => ({ text }),
      getElementById: () => stack,
    },
    Map,
    Promise,
    requestAnimationFrame() {},
    Set,
    setTimeout(_callback, delay) {
      delays.push(delay);
      return delays.length;
    },
    window: { toast },
  };

  vm.runInNewContext(source, context, { filename: 'toast.js' });
  return { delays, handlers };
}

function addNotification(ttlMs) {
  const harness = createHarness();
  harness.handlers.add({
    id: `ttl-${ttlMs}`,
    title: 'Готово',
    body: '',
    kind: 'done',
    ttlMs,
  });
  return harness.delays;
}

function extendNotificationAfterVoice(ttlMs) {
  const harness = createHarness();
  const id = `voiced-${ttlMs}`;
  harness.handlers.add({ id, title: 'Готово', body: '', kind: 'done', ttlMs });
  harness.handlers.hold({ id });
  harness.delays.length = 0;
  harness.handlers.extend({ id, ms: 3_500 });
  return harness.delays;
}

test('toast uses the configured 5s and 8s durations from its payload', () => {
  assert.deepEqual(addNotification(5_000), [5_000]);
  assert.deepEqual(addNotification(8_000), [8_000]);
});

test('zero duration keeps the toast visible without scheduling auto-hide', () => {
  assert.deepEqual(addNotification(0), []);
});

test('configured duration is still honored after voice playback', () => {
  assert.deepEqual(extendNotificationAfterVoice(5_000), [5_000]);
  assert.deepEqual(extendNotificationAfterVoice(8_000), [8_000]);
  assert.deepEqual(extendNotificationAfterVoice(0), []);
});
