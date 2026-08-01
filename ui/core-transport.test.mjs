import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';

const root = new URL('../', import.meta.url);
const expectedCsp =
  "default-src 'self'; object-src 'none'; base-uri 'none'; frame-src 'none'; img-src 'self' data:; font-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'self' ipc: http://ipc.localhost";
const trustedDocuments = new Map([
  ['ui/index.html', './bridge.js'],
  ['ui/toast.html', './toast-bridge.js'],
  ['ui/onboarding.html', 'onboarding.js'],
  ['ui/agent-chat.html', 'agent-chat.js'],
]);

async function source(relative) {
  return readFile(new URL(relative, root), 'utf8');
}

function scriptSources(html) {
  return [...html.matchAll(/<script\b([^>]*)>[\s\S]*?<\/script\s*>/gi)].map(
    ([, attributes]) => {
      const src = attributes.match(/\bsrc\s*=\s*(?:"([^"]*)"|'([^']*)')/i);
      return src ? (src[1] ?? src[2]) : null;
    },
  );
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

test('all trusted documents load one transport immediately before its consumer', async () => {
  for (const [document, consumer] of trustedDocuments) {
    const html = await source(document);
    const scripts = scriptSources(html);
    assert.equal(
      scripts.filter((src) => src === './generated/tauri-transport.js').length,
      1,
      `${document} loads the core transport exactly once`,
    );
    assert.equal(
      scripts.filter((src) => src === consumer).length,
      1,
      `${document} loads ${consumer} exactly once`,
    );
    const transportIndex = scripts.indexOf('./generated/tauri-transport.js');
    assert.equal(
      scripts[transportIndex + 1],
      consumer,
      `${document} consumes and deletes the transport without an intervening script`,
    );
    if (document === 'ui/onboarding.html') {
      assert.equal(
        scripts[transportIndex - 1],
        'onboarding-state.js',
        'onboarding state initializes before the transport bootstrap',
      );
    }
  }
});

test('all trusted documents use the same explicit IPC-capable CSP', async () => {
  for (const document of trustedDocuments.keys()) {
    const html = await source(document);
    assert.equal(
      html.match(/http-equiv="Content-Security-Policy"/g)?.length,
      1,
      `${document} has exactly one CSP`,
    );
    assert.match(
      html,
      new RegExp(
        `<meta http-equiv="Content-Security-Policy" content="${expectedCsp.replaceAll(
          /[.*+?^${}()|[\]\\]/g,
          '\\$&',
        )}"\\s*/?>`,
      ),
      document,
    );
  }
});

test('trusted documents do not load remote scripts or assets', async () => {
  const remoteAsset =
    /<(?:script|link|img|iframe|source|audio|video)\b[^>]*\b(?:src|href|srcset)\s*=\s*(?:"(?:https?:)?\/\/|'(?:https?:)?\/\/)/i;
  for (const document of trustedDocuments.keys()) {
    assert.doesNotMatch(await source(document), remoteAsset, document);
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

test('generated transport is committed without an inline sourcemap', async () => {
  const generated = await source('ui/generated/tauri-transport.js');
  assert.match(generated, /__JARVIS_CORE_TRANSPORT__/);
  assert.match(generated, /Object\.freeze/);
  assert.doesNotMatch(generated, /sourceMappingURL/);
});
