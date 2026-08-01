import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { checkTauriAcl } from './check-tauri-acl.mjs';

const EXPECTED_CSP =
  "default-src 'self'; object-src 'none'; base-uri 'none'; frame-src 'none'; img-src 'self' data:; font-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; connect-src 'self' ipc: http://ipc.localhost";

async function writeJson(root, relative, value) {
  const destination = path.join(root, relative);
  await mkdir(path.dirname(destination), { recursive: true });
  await writeFile(destination, `${JSON.stringify(value, null, 2)}\n`);
}

async function safeFixture() {
  const root = await mkdtemp(path.join(tmpdir(), 'jarvis-tauri-acl-'));
  await writeFile(
    path.join(root, 'package.json'),
    `${JSON.stringify(
      {
        devDependencies: {
          '@tauri-apps/api': '2.11.1',
          esbuild: '0.25.12',
        },
      },
      null,
      2,
    )}\n`,
  );
  await mkdir(path.join(root, 'src-tauri/src'), { recursive: true });
  await writeFile(
    path.join(root, 'src-tauri/src/app_command_inventory.rs'),
    String.raw`macro_rules! with_app_commands {
  ($callback:ident) => {
    $callback! {
      ("state_get", crate::ipc::state_get, ["main"]),
    }
  };
}
`,
  );
  await writeJson(root, 'src-tauri/tauri.conf.json', {
    app: {
      withGlobalTauri: false,
      security: {
        freezePrototype: true,
        capabilities: ['main', 'toast', 'onboarding', 'agent-chat'],
        csp: EXPECTED_CSP,
      },
    },
  });
  for (const identifier of ['main', 'toast', 'onboarding', 'agent-chat']) {
    await writeJson(root, `src-tauri/capabilities/${identifier}.json`, {
      identifier,
      webviews: [identifier],
      permissions: [
        'core:event:allow-listen',
        ...(['main', 'toast'].includes(identifier)
          ? ['clipboard-manager:allow-write-text']
          : []),
        ...(identifier === 'main' ? ['allow-state-get'] : []),
      ],
    });
  }
  return root;
}

async function expectCode(mutate, code) {
  const root = await safeFixture();
  await mutate(root);
  assert.throws(() => checkTauriAcl(root), (error) => {
    assert.equal(error.code, code);
    assert.match(error.message, new RegExp(code));
    return true;
  });
}

test('rejects window-wide capability scope with the stable error', async () => {
  await expectCode(async (root) => {
    const file = path.join(root, 'src-tauri/capabilities/main.json');
    const capability = JSON.parse(await (await import('node:fs/promises')).readFile(file));
    capability.windows = ['main'];
    delete capability.webviews;
    await writeJson(root, 'src-tauri/capabilities/main.json', capability);
  }, 'tauri_acl_window_scope_forbidden');
});

test('rejects wildcard and plugin-prefixed webview labels', async () => {
  await expectCode(async (root) => {
    await writeJson(root, 'src-tauri/capabilities/main.json', {
      identifier: 'main',
      webviews: ['*'],
      permissions: ['core:event:allow-listen', 'allow-state-get'],
    });
  }, 'tauri_acl_webview_wildcard_forbidden');

  await expectCode(async (root) => {
    await writeJson(root, 'src-tauri/capabilities/main.json', {
      identifier: 'main',
      webviews: ['plugin-agent-vm'],
      permissions: ['core:event:allow-listen', 'allow-state-get'],
    });
  }, 'tauri_acl_plugin_webview_forbidden');
});

test('rejects implicit capability and command grant drift', async () => {
  await expectCode(async (root) => {
    await writeJson(root, 'src-tauri/capabilities/extra.json', {
      identifier: 'extra',
      webviews: ['main'],
      permissions: [],
    });
  }, 'tauri_acl_unlisted_capability_forbidden');

  await expectCode(async (root) => {
    await writeJson(root, 'src-tauri/capabilities/main.json', {
      identifier: 'main',
      webviews: ['main'],
      permissions: [
        'core:event:allow-listen',
        'clipboard-manager:allow-write-text',
      ],
    });
  }, 'tauri_acl_command_grant_drift');
});

test('rejects broad core permissions and unsafe Tauri config', async () => {
  await expectCode(async (root) => {
    await writeJson(root, 'src-tauri/capabilities/main.json', {
      identifier: 'main',
      webviews: ['main'],
      permissions: ['core:default', 'allow-state-get'],
    });
  }, 'tauri_acl_core_default_forbidden');

  await expectCode(async (root) => {
    const config = {
      app: {
        withGlobalTauri: true,
        security: {
          capabilities: ['main', 'toast', 'onboarding', 'agent-chat'],
          csp: "default-src 'self'",
        },
      },
    };
    await writeJson(root, 'src-tauri/tauri.conf.json', config);
  }, 'tauri_acl_global_tauri_forbidden');

  await expectCode(async (root) => {
    const config = {
      app: {
        withGlobalTauri: false,
        security: {
          capabilities: ['main', 'toast', 'onboarding', 'agent-chat'],
          csp: null,
        },
      },
    };
    await writeJson(root, 'src-tauri/tauri.conf.json', config);
  }, 'tauri_acl_csp_missing');
});

test('rejects unpinned trusted transport dependencies', async () => {
  await expectCode(async (root) => {
    await writeJson(root, 'package.json', {
      devDependencies: {
        '@tauri-apps/api': '^2.11.1',
        esbuild: '0.25.12',
      },
    });
  }, 'tauri_acl_dependency_pin_invalid');
});
