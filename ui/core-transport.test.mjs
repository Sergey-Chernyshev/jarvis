import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';

const root = new URL('../', import.meta.url);

async function source(relative) {
  return readFile(new URL(relative, root), 'utf8');
}

test('bridge initializes from the explicit core transport without touching window.__TAURI__', async () => {
  const invoked = [];
  const listened = [];
  const sandbox = {
    navigator: {},
    __JARVIS_CORE_TRANSPORT__: Object.freeze({
      invoke(command, payload) {
        invoked.push([command, payload]);
        return Promise.resolve(null);
      },
      listen(event, callback) {
        listened.push([event, callback]);
        return Promise.resolve(() => {});
      },
    }),
  };
  sandbox.window = sandbox;
  Object.defineProperty(sandbox, '__TAURI__', {
    configurable: true,
    get() {
      throw new Error('window.__TAURI__ must not be read');
    },
  });

  vm.runInNewContext(await source('ui/bridge.js'), sandbox, {
    filename: 'ui/bridge.js',
  });

  assert.equal(typeof sandbox.jarvis.getState, 'function');
  assert.equal(Object.isFrozen(sandbox.jarvis), true);
  assert.equal('__JARVIS_CORE_TRANSPORT__' in sandbox, false);
  await sandbox.jarvis.getState();
  assert.deepEqual(invoked, [['state_get', undefined]]);
  assert.deepEqual(listened, []);
});

test('all trusted documents load the deterministic transport before their bridge', async () => {
  const documents = new Map([
    ['ui/index.html', 'bridge.js'],
    ['ui/toast.html', 'toast-bridge.js'],
    ['ui/onboarding.html', 'onboarding.js'],
    ['ui/agent-chat.html', 'agent-chat.js'],
  ]);

  for (const [document, bridge] of documents) {
    const html = await source(document);
    const transportIndex = html.indexOf('generated/tauri-transport.js');
    const bridgeIndex = html.indexOf(bridge);
    assert.notEqual(transportIndex, -1, `${document} loads the core transport`);
    assert.notEqual(bridgeIndex, -1, `${document} loads ${bridge}`);
    assert.ok(
      transportIndex < bridgeIndex,
      `${document} loads the core transport before ${bridge}`,
    );
  }
});

test('production UI contains no global Tauri fallback', async () => {
  for (const file of [
    'ui/bridge.js',
    'ui/toast-bridge.js',
    'ui/onboarding.js',
    'ui/agent-chat.js',
  ]) {
    assert.doesNotMatch(await source(file), /__TAURI__/, file);
  }
});

test('generated transport is committed and byte-stable across builds', async () => {
  const generated = await source('ui/generated/tauri-transport.js');
  assert.match(generated, /__JARVIS_CORE_TRANSPORT__/);
  assert.match(generated, /Object\.freeze/);
  assert.doesNotMatch(generated, /sourceMappingURL/);
});
