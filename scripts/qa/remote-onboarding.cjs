// Production renderer + Projects against a strict synthetic bridge; no native actions.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const os = require('node:os');
let playwright;
try { playwright = require('playwright'); } catch { playwright = require(process.env.JARVIS_PLAYWRIGHT_PATH || path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }
const root = path.resolve(__dirname, '../../ui'), out = path.resolve(__dirname, '../../docs/qa/assets/remote-onboarding');
// Reuse the strict chat fixture, including real launch-event delivery behavior.
const base = fs.readFileSync(path.join(__dirname, 'session-workspace.cjs'), 'utf8');
const baseFixture = base.slice(base.indexOf('function bridgeFixture()'), base.indexOf('\nconst records = []'));
const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.woff2': 'font/woff2' };
const server = http.createServer((req, res) => {
  const file = path.resolve(root, '.' + new URL(req.url, 'http://localhost').pathname);
  if (!file.startsWith(root + path.sep)) return res.writeHead(403).end();
  fs.readFile(file, (error, data) => error ? res.writeHead(404).end() : res.writeHead(200, { 'Content-Type': mime[path.extname(file)] || 'application/octet-stream' }).end(data));
});
function connectionsFixture() {
  const original = window.jarvis, f = window.__sessionFixture;
  const state = window.__connectionsFixture = { mode: 'authenticated', probes: [], installs: [], remotes: [] };
  const profile = { proxy: 'teleport.example.test', cluster: 'development', username: 'developer', logins: ['developer', 'root'], authenticated: true, validUntil: '2030-01-01T00:00:00Z' };
  const methods = {
    getSettings: async () => ({ ...await original.getSettings(), launchTerminal: 'terminal-app' }),
    machinesList: async () => [...await original.machinesList(), ...state.remotes.map(r => ({ id: r.name, name: r.name, kind: 'remote', online: r.connected }))],
    remotesList: () => state.remotes, vmStatus: () => ({ ok: true, available: false, generation: 'unknown', vms: [], capabilities: {} }),
    teleportStatus: () => state.mode === 'authenticated' ? { ok: true, available: true, version: '18.10.0', ...profile, profiles: [profile] } : { ok: true, available: true, authenticated: false, proxy: profile.proxy, profiles: [] },
    teleportNodes: () => ({ ok: true, cluster: profile.cluster, logins: profile.logins, nodes: [{ id: 'node-uuid', target: 'node-uuid', name: 'Development VM', hostname: 'development-vm', labels: { environment: 'dev' } }, { id: 'runner-uuid', target: 'runner-uuid', name: 'Build runner', hostname: 'runner-vm', labels: {} }] }),
    teleportLogin: () => { state.mode = 'authenticated'; return { ok: true, started: true }; },
    remotesPreflight: (...args) => { state.probes.push(args); return state.mode === 'error' ? { ok: false, error: 'Teleport certificate expired' } : { ok: true, os: 'Linux', arch: 'x86_64', home: '/home/developer', dir: '/home/developer/.jarvis', tmux: false, curl: true, claude: true, codex: true, systemd: true, nodeSource: 'download', providerSources: [{ agent: 'codex', providerHome: '/home/developer/.codex' }], runtimeSetup: { missing: ['tmux'], automatic: true, command: 'sudo -n apt-get install -y tmux' } }; },
    remotesInstall: cfg => { state.installs.push(cfg); return { ok: true }; },
    remotesSshKey: () => ({ ok: true, publicKey: '', path: '' }), remotesSshAuthorize: () => ({ ok: true }),
    remotesTest: () => ({ ok: true, capabilities: ['launch', 'terminalInput'], sources: [{ agent: 'codex', providerHome: '/home/developer/.codex' }] }),
  };
  window.jarvis = new Proxy({}, { get(_, name) {
    if (!(name in methods)) return original[name];
    return async (...args) => { f.calls.push({ name, args }); return structuredClone(await methods[name](...args)); };
  } });
}
const records = [];
(async () => {
  fs.mkdirSync(out, { recursive: true });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const browser = await playwright.chromium.launch({ channel: 'chrome', headless: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 }, colorScheme: 'dark', reducedMotion: 'reduce' });
  const errors = []; page.on('pageerror', e => errors.push(e.message)); page.setDefaultTimeout(9000);
  const capture = async name => { await page.waitForFunction(() => !document.getAnimations().some(a => a.playState === 'running' && Number.isFinite(a.effect?.getComputedTiming().endTime))); await page.screenshot({ path: path.join(out, name + '.png') }); records.push(name); };
  try {
    await page.route('**/bridge.js', route => route.fulfill({ contentType: 'text/javascript', body: baseFixture + '\nbridgeFixture();\n(' + connectionsFixture.toString() + ')();' }));
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.locator('#tabSettings').click();
    await page.evaluate(() => window.jarvisOpenSettingsPane('remotes'));
    const wizard = page.locator('#s2-rwiz');
    await wizard.getByRole('button', { name: 'Teleport (tsh)', exact: true }).waitFor();
    assert.equal(await page.evaluate(() => window.__sessionFixture.calls.filter(c => c.name.startsWith('teleport')).length), 0);
    await capture('ssh-access-dark');
    await wizard.getByRole('button', { name: 'Teleport (tsh)', exact: true }).click();
    await wizard.getByLabel('Пользователь SSH', { exact: true }).selectOption('developer');
    await wizard.getByLabel('Машина Teleport', { exact: true }).selectOption('node-uuid');
    await capture('teleport-machine-dark');
    await wizard.getByRole('button', { name: 'Проверить машину', exact: true }).click();
    await wizard.getByRole('button', { name: 'Настроить автоматически', exact: true }).waitFor();
    assert.equal(await page.evaluate(() => window.__connectionsFixture.probes[0][0]), 'developer@node-uuid');
    assert.equal(await page.evaluate(() => window.__connectionsFixture.probes[0][2].teleportCluster), 'development');
    await capture('teleport-ready-dark');
    await page.setViewportSize({ width: 720, height: 820 });
    await page.emulateMedia({ colorScheme: 'light' });
    await page.evaluate(() => window.jarvisTheme.adopt({ theme: 'light', mode: 'window' }));
    await capture('teleport-ready-compact');
    const sizes = await wizard.evaluate(n => ({ client: n.clientWidth, scroll: n.scrollWidth }));
    assert.ok(sizes.scroll <= sizes.client + 1, JSON.stringify(sizes));
    await wizard.getByRole('button', { name: 'Настроить автоматически', exact: true }).click();
    await page.waitForFunction(() => window.__connectionsFixture.installs.length === 1);
    const cfg = await page.evaluate(() => window.__connectionsFixture.installs[0]);
    assert.equal(cfg.transport, 'teleport'); assert.equal(cfg.teleportProxy, 'teleport.example.test'); assert.equal(cfg.teleportCluster, 'development');
    await page.evaluate(() => {
      const f = window.__sessionFixture;
      for (const cb of f.events.onRemoteInstallStep || []) cb({ phase: 'Проверка', state: 'done', msg: 'События доставлены' });
    });
    await wizard.locator('#s2-rlog .msg', { hasText: 'События доставлены' }).waitFor();
    assert.equal(await wizard.locator('#s2-rlog .install-phase').last().textContent(), 'Проверка');
    await page.evaluate(cfg => {
      const f = window.__sessionFixture;
      window.__connectionsFixture.remotes = [{ ...cfg, connected: true, version: '0.3.3' }];
      for (const cb of f.events.onRemoteInstallDone || []) cb({ ok: true, name: cfg.name });
    }, cfg);
    await page.waitForFunction(() => document.querySelector('#s2-pane-remotes')?.textContent.includes('подключена'));
    await capture('teleport-installed-compact');
    await wizard.getByRole('button', { name: 'Открыть чаты', exact: true }).click();
    await page.waitForFunction(name => document.querySelector('#newChatMachine')?.value === name, cfg.name);
    records.push('open-chat-selects-installed-machine');
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.reload();
    await page.locator('#tabSettings').click();
    await page.evaluate(() => { window.__connectionsFixture.mode = 'expired'; window.jarvisOpenSettingsPane('remotes'); });
    await wizard.getByRole('button', { name: 'Teleport (tsh)', exact: true }).click();
    await wizard.getByRole('button', { name: 'Войти через Teleport', exact: true }).waitFor();
    await capture('teleport-login-dark');
    assert.equal(await wizard.locator('input[type=password]').count(), 0);
    await wizard.getByRole('button', { name: 'Войти через Teleport', exact: true }).click();
    await wizard.getByLabel('Пользователь SSH', { exact: true }).waitFor();
    assert.equal(await page.evaluate(() => window.__sessionFixture.calls.filter(c => c.name === 'teleportLogin').length), 1);
    assert.equal(await page.evaluate(() => window.__sessionFixture.calls.filter(c => c.name === 'remotesSshAuthorize').length), 0);
    records.push('SSO-polls-until-authenticated-without-SSH-password');
    assert.deepEqual(errors, []);
    assert.deepEqual(await page.evaluate(() => window.__sessionFixture.unknown), []);
    records.push('exact-proxy-cluster-node-no-secrets', 'no-overflow-or-browser-errors', 'install-completion-renders-connected-machine');
    fs.writeFileSync(path.join(out, 'checks.json'), JSON.stringify({ checks: records, errors }, null, 2) + '\n');
    console.log(JSON.stringify({ ok: true, checks: records.length, records }, null, 2));
  } catch (error) {
    await page.screenshot({ path: path.join(out, 'failure.png') });
    console.error(JSON.stringify({ errors, unknown: await page.evaluate(() => window.__sessionFixture?.unknown) })); throw error;
  } finally { await browser.close(); server.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
