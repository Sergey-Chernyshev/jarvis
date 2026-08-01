import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';

import { parseClassicExternalScripts } from '../scripts/check-tauri-acl.mjs';

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
  return parseClassicExternalScripts(html, 'core transport test').map(
    (script) => script.src,
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

test('script scan preserves Unicode indices while folding ASCII tag names only', () => {
  const fixtures = [
    [
      'İ<script src="./first.js"></script>',
      ['./first.js'],
    ],
    [
      '<script src="./first.js"></script>İ<script src="./second.js"></script>',
      ['./first.js', './second.js'],
    ],
    [
      'Привет🙂<ScRiPt SRC = "./first.js"></sCrIpT>世界<script src="./second.js"></script>',
      ['./first.js', './second.js'],
    ],
  ];

  for (const [html, expected] of fixtures) {
    assert.deepEqual(
      parseClassicExternalScripts(html, 'unicode fixture').map(
        (script) => script.src,
      ),
      expected,
    );
  }

  assert.deepEqual(
    parseClassicExternalScripts(
      '<ѕcript src="./lookalike.js"></ѕcript>',
      'unicode lookalike',
    ),
    [],
  );
  assert.throws(
    () =>
      parseClassicExternalScripts(
        'İ<ScRiPt src="./unsafe.js" AsYnC="false"></sCrIpT>',
        'unicode unsafe attribute',
      ),
    (error) =>
      error.code === 'tauri_acl_script_tag_invalid' &&
      error.message.includes('forbidden or duplicate attribute AsYnC'),
  );
});

test('transport and consumer reject execution-changing or malformed attributes', () => {
  const transport = './generated/tauri-transport.js';
  const consumer = './bridge.js';
  const base = `<script src="${transport}"></script>
<script src="${consumer}"></script>`;
  const attributes = [
    ' async',
    ' AsYnC="false"',
    ' defer',
    " DeFeR='defer'",
    ' type="module"',
    " TYPE='text/javascript'",
    ' nomodule',
    ' NoMoDuLe="false"',
    ' integrity="sha256-test"',
  ];

  for (const src of [transport, consumer]) {
    for (const attribute of attributes) {
      const unsafe = base.replace(
        `<script src="${src}">`,
        `<script src="${src}"${attribute}>`,
      );
      assert.throws(
        () => parseClassicExternalScripts(unsafe, src),
        (error) => error.code === 'tauri_acl_script_tag_invalid',
        `${src}${attribute}`,
      );
    }

    for (const tag of [
      `<script src="${src}" SRC="${src}">`,
      `<script src=${src}>`,
      '<script src>',
      `<script src="${src}>`,
    ]) {
      const malformed = base.replace(`<script src="${src}">`, tag);
      assert.throws(
        () => parseClassicExternalScripts(malformed, src),
        (error) => error.code === 'tauri_acl_script_tag_invalid',
        tag,
      );
    }
  }

  const onboardingState = `<script src="onboarding-state.js" defer></script>
<script src="${transport}"></script>
<script src="onboarding.js"></script>`;
  assert.throws(
    () => parseClassicExternalScripts(onboardingState, 'onboarding-state'),
    (error) => error.code === 'tauri_acl_script_tag_invalid',
  );
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
