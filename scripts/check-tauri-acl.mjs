import { readdirSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const CORE_WEBVIEWS = ['main', 'toast', 'onboarding', 'agent-chat'];
const CORE_WEBVIEW_SET = new Set(CORE_WEBVIEWS);
const EXPECTED_CSP =
  "default-src 'self'; object-src 'none'; base-uri 'none'; frame-src 'none'; img-src 'self' data:; font-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; connect-src 'self' ipc: http://ipc.localhost";
const SUPPORT_PERMISSIONS = new Map([
  [
    'main',
    new Set([
      'core:event:allow-listen',
      'clipboard-manager:allow-write-text',
    ]),
  ],
  [
    'toast',
    new Set([
      'core:event:allow-listen',
      'clipboard-manager:allow-write-text',
    ]),
  ],
  ['onboarding', new Set(['core:event:allow-listen'])],
  ['agent-chat', new Set(['core:event:allow-listen'])],
]);

export class TauriAclError extends Error {
  constructor(code, detail = '') {
    super(detail ? `${code}: ${detail}` : code);
    this.name = 'TauriAclError';
    this.code = code;
  }
}

function fail(code, detail) {
  throw new TauriAclError(code, detail);
}

function readJson(root, relative) {
  const file = path.join(root, relative);
  try {
    return JSON.parse(readFileSync(file, 'utf8'));
  } catch (error) {
    fail('tauri_acl_json_invalid', `${relative}: ${error.message}`);
  }
}

function parseInventory(root) {
  const relative = 'src-tauri/src/app_command_inventory.rs';
  const source = readFileSync(path.join(root, relative), 'utf8');
  const row =
    /\(\s*"([a-z][a-z0-9_]*)"\s*,\s*crate::(?:ipc|onboarding)::([a-z][a-z0-9_]*)\s*,\s*\[([^\]]*)\]\s*\)/g;
  const label = /"([^"]+)"/g;
  const inventory = new Map();

  for (const match of source.matchAll(row)) {
    const [, command, handler, rawLabels] = match;
    if (command !== handler || inventory.has(command)) {
      fail('tauri_acl_inventory_invalid', command);
    }
    const webviews = new Set(
      [...rawLabels.matchAll(label)].map((entry) => entry[1]),
    );
    if (
      webviews.size !== [...rawLabels.matchAll(label)].length ||
      [...webviews].some((webview) => !CORE_WEBVIEW_SET.has(webview))
    ) {
      fail('tauri_acl_inventory_invalid', command);
    }
    inventory.set(command, webviews);
  }

  if (inventory.size === 0) {
    fail('tauri_acl_inventory_invalid', 'empty command inventory');
  }
  return inventory;
}

function sameSet(left, right) {
  return (
    left.size === right.size && [...left].every((entry) => right.has(entry))
  );
}

function readCapabilities(root, enabled) {
  const directory = path.join(root, 'src-tauri/capabilities');
  const capabilities = new Map();

  for (const filename of readdirSync(directory).filter((entry) =>
    entry.endsWith('.json'),
  )) {
    const capability = readJson(
      root,
      path.join('src-tauri/capabilities', filename),
    );
    const identifier = capability.identifier;
    if (typeof identifier !== 'string') {
      fail('tauri_acl_unlisted_capability_forbidden', filename);
    }
    if (!enabled.has(identifier)) {
      fail('tauri_acl_unlisted_capability_forbidden', identifier);
    }
    if (capabilities.has(identifier)) {
      fail('tauri_acl_capability_duplicate', identifier);
    }
    if (Object.hasOwn(capability, 'windows')) {
      fail('tauri_acl_window_scope_forbidden', identifier);
    }
    if (!Array.isArray(capability.webviews)) {
      fail('tauri_acl_webview_scope_missing', identifier);
    }
    for (const webview of capability.webviews) {
      if (
        typeof webview !== 'string' ||
        webview.includes('*') ||
        webview.includes('?')
      ) {
        fail('tauri_acl_webview_wildcard_forbidden', identifier);
      }
      if (webview.startsWith('plugin-')) {
        fail('tauri_acl_plugin_webview_forbidden', webview);
      }
    }
    if (
      capability.webviews.length !== 1 ||
      capability.webviews[0] !== identifier
    ) {
      fail('tauri_acl_webview_scope_mismatch', identifier);
    }
    if (!Array.isArray(capability.permissions)) {
      fail('tauri_acl_permissions_missing', identifier);
    }
    if (capability.permissions.includes('core:default')) {
      fail('tauri_acl_core_default_forbidden', identifier);
    }
    capabilities.set(identifier, capability);
  }

  if (!sameSet(new Set(capabilities.keys()), enabled)) {
    fail('tauri_acl_unlisted_capability_forbidden', 'enabled/file drift');
  }
  return capabilities;
}

function checkDependencyPins(root) {
  const packageJson = readJson(root, 'package.json');
  const dependencies = packageJson.devDependencies ?? {};
  if (
    dependencies['@tauri-apps/api'] !== '2.11.1' ||
    dependencies.esbuild !== '0.25.12'
  ) {
    fail('tauri_acl_dependency_pin_invalid');
  }
}

function checkConfig(root) {
  const config = readJson(root, 'src-tauri/tauri.conf.json');
  const app = config.app ?? {};
  const security = app.security ?? {};

  if (app.withGlobalTauri !== false) {
    fail('tauri_acl_global_tauri_forbidden');
  }
  if (security.csp == null) {
    fail('tauri_acl_csp_missing');
  }
  if (security.csp !== EXPECTED_CSP) {
    fail('tauri_acl_csp_invalid');
  }
  if (security.freezePrototype !== true) {
    fail('tauri_acl_freeze_prototype_missing');
  }
  if (
    !Array.isArray(security.capabilities) ||
    security.capabilities.some((identifier) => typeof identifier !== 'string')
  ) {
    fail('tauri_acl_capability_list_invalid');
  }

  const enabled = new Set(security.capabilities);
  if (
    enabled.size !== security.capabilities.length ||
    !sameSet(enabled, CORE_WEBVIEW_SET)
  ) {
    fail('tauri_acl_capability_list_invalid');
  }
  return enabled;
}

function checkGrants(inventory, capabilities) {
  const actual = new Map();
  for (const command of inventory.keys()) actual.set(command, new Set());

  for (const [identifier, capability] of capabilities) {
    const support = new Set();
    for (const permission of capability.permissions) {
      if (typeof permission !== 'string') {
        fail('tauri_acl_permissions_missing', identifier);
      }
      if (!permission.includes(':') && permission.startsWith('allow-')) {
        const command = permission.slice('allow-'.length).replaceAll('-', '_');
        if (!actual.has(command)) {
          fail('tauri_acl_command_grant_drift', permission);
        }
        actual.get(command).add(identifier);
      } else {
        support.add(permission);
      }
    }
    if (!sameSet(support, SUPPORT_PERMISSIONS.get(identifier))) {
      fail('tauri_acl_support_permission_drift', identifier);
    }
  }

  for (const [command, expectedWebviews] of inventory) {
    if (!sameSet(actual.get(command), expectedWebviews)) {
      fail('tauri_acl_command_grant_drift', command);
    }
  }
}

function checkNoGlobalTauri(root) {
  const uiRoot = path.join(root, 'ui');
  for (const filename of [
    'bridge.js',
    'toast-bridge.js',
    'onboarding.js',
    'agent-chat.js',
  ]) {
    const source = readFileSync(path.join(uiRoot, filename), 'utf8');
    if (source.includes('window.__TAURI__')) {
      fail('tauri_acl_global_tauri_reference_forbidden', filename);
    }
  }
}

export function checkTauriAcl(root = process.cwd()) {
  const repositoryRoot = path.resolve(root);
  checkDependencyPins(repositoryRoot);
  const enabled = checkConfig(repositoryRoot);
  const inventory = parseInventory(repositoryRoot);
  const capabilities = readCapabilities(repositoryRoot, enabled);
  checkGrants(inventory, capabilities);
  checkNoGlobalTauri(repositoryRoot);
  return Object.freeze({
    commands: inventory.size,
    capabilities: capabilities.size,
  });
}

const isCli =
  process.argv[1] &&
  fileURLToPath(import.meta.url) === path.resolve(process.argv[1]);
if (isCli) {
  const rootIndex = process.argv.indexOf('--root');
  const root = rootIndex === -1 ? process.cwd() : process.argv[rootIndex + 1];
  if (!root) {
    fail('tauri_acl_root_missing');
  }
  try {
    const result = checkTauriAcl(root);
    process.stdout.write(
      `tauri_acl_ok commands=${result.commands} capabilities=${result.capabilities}\n`,
    );
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
