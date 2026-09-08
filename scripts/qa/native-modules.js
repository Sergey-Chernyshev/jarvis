// Executes inside the actual WKWebView, using the real bridge in a disposable
// profile. No live microphone, agent launch, install, or permission is triggered.
const visible = element => !!element && !element.closest('[hidden]') &&
  getComputedStyle(element).display !== 'none' && element.getBoundingClientRect().height > 0;
const click = selector => {
  const element = document.querySelector(selector);
  t.assert(visible(element), `Control is not visible: ${selector}`);
  element.scrollIntoView({ block: 'nearest' });
  const rect = element.getBoundingClientRect();
  const hit = document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2);
  t.assert(hit === element || element.contains(hit), `Control is covered: ${selector}`);
  element.click();
};
const key = (value, options = {}) => window.dispatchEvent(new KeyboardEvent('keydown', { key: value, bubbles: true, cancelable: true, ...options }));
const settleVisuals = async () => {
  await t.waitFor(() => {
    const running = document.getAnimations().filter(animation => animation.playState === 'running' && Number.isFinite(animation.effect?.getComputedTiming().endTime));
    // WebKit suspends its compositor when the user returns to another app.
    // These are static layout snapshots; motion is exercised separately in
    // the foreground browser. Never wait forever for an occluded animation.
    if (!document.hasFocus() && running.length) {
      running.forEach(animation => animation.finish());
      t.evidence('occludedAnimationPolicy', 'Finite animations finished for static WKWebView snapshots while app is unfocused; this is not a native motion assertion.');
      return true;
    }
    return running.length === 0;
  });
};
const snapshot = async name => {
  await settleVisuals();
  t.evidence(name, t.geometry('#panel,#pageNavigation,#launcher,#content,#machines2,#settings2,.snav,.detail,.dtitle'));
  if (t.screenshot) await t.screenshot(name);
};
const assertLayout = host => {
  const box = host.getBoundingClientRect();
  t.evidence(`layout-${host.id}`, { rect: box.toJSON(), viewport: { width: innerWidth, height: innerHeight }, transform: getComputedStyle(host).transform });
  t.assert(box.width > 100 && box.height > 60, `Collapsed module ${host.id}`);
  t.assert(document.documentElement.scrollWidth <= innerWidth + 1, 'Document overflows horizontally');
  t.assert(box.right <= innerWidth + 1 && box.bottom <= innerHeight + 1, `Module escapes viewport: ${host.id}`);
};
async function home() {
  if (document.documentElement.dataset.view !== 'home') click('#pageHome');
  await t.waitFor(() => visible(document.querySelector('#launcher')));
  const query = document.querySelector('#query'); query.value = '';
  query.dispatchEvent(new Event('input', { bubbles: true }));
}

await t.step('Root is a usable list of nine modules', async () => {
  await t.waitFor(() => document.documentElement.dataset.view === 'home');
  // Initial settings synchronization can still change the route after the
  // launcher first appears. Finish it before testing navigation.
  await Promise.all([initialStateReady, initialSettingsReady]);
  await home();
  t.assert(document.querySelectorAll('#launcher [data-module]').length === 9, 'Missing module');
  assertLayout(document.querySelector('#launcher'));
  await snapshot('home-dark');
});

await t.step('Native input-device discovery responds without microphone capture', async () => {
  const started = performance.now();
  const result = await window.jarvis.sttInputDevices();
  t.assert(Array.isArray(result.devices), 'Invalid device list');
  t.assert(performance.now() - started < 5000, 'Device discovery exceeded its UI deadline');
  t.evidence('devices', { count: result.devices.length, elapsedMs: performance.now() - started, error: result.error });
});

await t.step('Root search and Escape restore navigation state', async () => {
  const query = document.querySelector('#query'); query.value = 'встречи';
  query.dispatchEvent(new Event('input', { bubbles: true })); key('Enter');
  await t.waitFor(() => visible(document.querySelector('#meetings')));
  click('#pageCommands');
  const input = document.querySelector('#commandQuery'); input.value = 'настройки';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  click('.command-result');
  await t.waitFor(() => visible(document.querySelector('#settings')));
  key('Escape'); await t.waitFor(() => visible(document.querySelector('#meetings')));
  key('Escape'); await t.waitFor(() => visible(document.querySelector('#launcher')));
  t.assert(query.value === 'встречи', 'Root search was lost on back');
  key('Escape'); t.assert(query.value === '', 'Escape did not clear root search');
});

for (const [tab, module] of [
  ['tabSessions', 'list'], ['tabHistory', 'history'], ['tabMachines', 'machines'], ['tabVoice', 'voicehist'],
  ['tabMeetings', 'meetings'], ['tabLoops', 'loops'], ['tabBundle', 'bundlePane'],
  ['tabStats', 'stats'], ['tabSettings', 'settings'],
]) {
  await t.step(`Module ${module} opens, lays out, and goes back`, async () => {
    await home(); click(`#${tab}`);
    const host = document.getElementById(module === 'list' ? 'chatWelcome' : module);
    await t.waitFor(() => visible(host) && host.textContent.trim().length > 0);
    if (module === 'machines') {
      await t.waitFor(() => visible(host.querySelector('.connection-workspace')) && !host.querySelector('.skel'), 20000);
      t.assert(!host.querySelector('.snav') && !visible(document.querySelector('#settings')), 'Machines opened inside Settings');
      t.assert(document.documentElement.dataset.view === 'machines', 'Machines route did not stay active');
    }
    await settleVisuals();
    assertLayout(host);
    await snapshot(`module-${module}`);
    click('#pageBack');
    await t.waitFor(() => visible(document.querySelector('#launcher')));
  });
}

await t.step('Machines shortcut and Escape preserve the previous Settings pane', async () => {
  await home(); click('#tabSettings');
  await t.waitFor(() => visible(document.querySelector('.snav [data-pane="about"]')));
  click('.snav [data-pane="about"]');
  await t.waitFor(() => visible(document.querySelector('#s2-pane-about')) && !document.querySelector('#s2-pane-about .skel'));
  key('8', { metaKey: true });
  await t.waitFor(() => document.documentElement.dataset.view === 'machines' && visible(document.querySelector('#machines2 .connection-workspace')));
  const search = document.querySelector('#machines2 .connection-search input');
  search.value = 'native-missing-machine';
  search.dispatchEvent(new Event('input', { bubbles: true }));
  key('Escape');
  t.assert(search.value === '' && document.documentElement.dataset.view === 'machines', 'First Escape did not clear the machine filter');
  key('Escape');
  await t.waitFor(() => document.documentElement.dataset.view === 'settings' && visible(document.querySelector('#s2-pane-about')));
  t.assert(!visible(document.querySelector('#machines')), 'Machines remained visible after going back');
  await home();
});

await home(); click('#tabSettings');
await t.waitFor(() => document.querySelector('.snav [data-pane]'));
await settleVisuals();
const panes = [...document.querySelectorAll('.snav [data-pane]')].map(element => element.dataset.pane);
for (const pane of panes) {
  await t.step(`Settings ${pane} settles and remains navigable`, async () => {
    click(`.snav [data-pane="${pane}"]`);
    const host = document.getElementById(`s2-pane-${pane}`);
    await t.waitFor(() => visible(host) && !host.querySelector('.skel') && host.querySelector('.dtitle'), 20000);
    t.assert(!host.querySelector('[role="alert"]'), `Settings ${pane} failed: ${host.innerText}`);
    assertLayout(document.querySelector('#settings2'));
    t.evidence(`settings-${pane}-content`, host.innerText);
    await snapshot(`settings-${pane}`);
  });
}

await t.step('A settings toggle persists through actual IPC and reopening', async () => {
  click('.snav [data-pane="general"]');
  const row = [...document.querySelectorAll('#s2-pane-general .drow')].find(node => node.textContent.includes('Режим логов'));
  const toggle = row.querySelector('input'); const before = toggle.checked;
  toggle.click();
  await t.waitFor(async () => (await window.jarvis.getSettings()).diagnostics === !before);
  // Persisted state can be observable before the saving request finishes its
  // own IPC response. Await that UI boundary rather than racing its handler.
  await t.waitFor(() => !toggle.disabled && toggle.getAttribute('aria-busy') !== 'true', 5000);
  t.assert(toggle.checked === !before && !toggle.disabled, 'Toggle did not settle after saving');
  await home(); click('#tabSettings');
  await t.waitFor(() => document.querySelector('#s2-pane-general input'));
  const reopened = [...document.querySelectorAll('#s2-pane-general .drow')].find(node => node.textContent.includes('Режим логов')).querySelector('input');
  t.assert(reopened.checked === !before, 'Setting reverted after reopening');
});

await t.step('Missing speech model reports its real backend failure', async () => {
  click('.snav [data-pane="stt"]');
  await t.waitFor(() => document.querySelector('#s2-pane-stt .cselect'));
  const picker = document.querySelector('#s2-pane-stt .cselect');
  const previous = picker.querySelector('.cval').textContent;
  picker.querySelector('.cstrigger').click();
  const missing = picker.querySelector('[data-value="qwen3-0.6b"]');
  t.assert(!!missing, 'Test engine is absent from picker'); missing.click();
  await t.waitFor(() => document.querySelector('#settings-save-error'));
  t.assert(picker.querySelector('.cval').textContent === previous, 'Failed engine looked selected');
  t.evidence('missing-model-error', document.querySelector('#settings-save-error').textContent);
  await snapshot('settings-real-model-error');
});

await t.step('Settings search opens the actual parameter and scopes errors', async () => {
  click('.snav [data-pane="about"]');
  t.assert(!document.querySelector('#settings-save-error'), 'Speech error leaked into About');
  const query = document.querySelector('#settingsSearch');
  query.value = 'прокси'; query.dispatchEvent(new Event('input', { bubbles: true }));
  const proxy = [...document.querySelectorAll('.settings-result')].find(row => row.textContent.includes('Egress-прокси'));
  t.assert(proxy, 'Proxy setting is absent from search'); proxy.click();
  await t.waitFor(() => document.activeElement?.querySelector('.dt')?.textContent === 'Egress-прокси');
  t.assert(query.value === '', 'Search query did not clear after selecting its parameter');
  await snapshot('settings-search-target');
});

await t.step('Light appearance and root restore', async () => {
  await home(); window.jarvisTheme.adopt({ theme: 'light' });
  await t.waitFor(() => document.documentElement.dataset.theme === 'light');
  assertLayout(document.querySelector('#launcher'));
  await snapshot('home-light');
});
