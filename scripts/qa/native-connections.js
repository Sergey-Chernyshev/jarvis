// Real WKWebView + production bridge, launched only via native-smoke.mjs in
// its disposable profile. Reads inventory and edits an unsaved SSH draft.
// No remote probe, node installation, VM action, hook repair, login or save.
const visible = element => {
  if (!element?.isConnected || element.closest('[hidden]')) return false;
  for (let current = element; current; current = current.parentElement) {
    const style = getComputedStyle(current);
    if (style.display === 'none' || style.visibility === 'hidden') return false;
  }
  const rect = element.getBoundingClientRect();
  return rect.width > 0 && rect.height > 0;
};
const click = element => {
  t.assert(visible(element) && !element.disabled, 'Control must be visible and enabled');
  element.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  const rect = element.getBoundingClientRect();
  const hit = document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2);
  t.assert(hit === element || element.contains(hit), 'Another surface covers the control');
  element.click();
};
const key = (element, value, modifiers = {}) => element.dispatchEvent(new KeyboardEvent('keydown', { key: value, bubbles: true, cancelable: true, ...modifiers }));
const button = (root, label) => [...root.querySelectorAll('button')].find(element => visible(element) && (element.textContent.trim() === label || element.getAttribute('aria-label') === label));
const snapshot = async name => {
  await t.waitFor(() => {
    const running = document.getAnimations().filter(animation => animation.playState === 'running' && Number.isFinite(animation.effect?.getComputedTiming().endTime));
    if (!document.hasFocus() && running.length) {
      running.forEach(animation => animation.finish());
      t.evidence('snapshotPolicy', 'Finite animations finished for static snapshots while occluded; no motion assertion.');
      return true;
    }
    return !running.length;
  });
  t.assert(document.documentElement.scrollWidth <= innerWidth + 1, 'Document overflows horizontally');
  await t.screenshot(name);
};
let pane, editor, grid;
const bindVisibleModule = () => {
  const current = document.querySelector('#machines2 #s2-pane-remotes');
  if (!visible(current)) return false;
  pane = current; grid = current.querySelector('.connection-grid'); editor = current.querySelector('#s2-ssh-setup');
  return visible(grid);
};
await t.step('Machine inventory renders real bridge data in the designed workspace', async () => {
  // native-smoke begins at DOMContentLoaded, before the native initial show
  // and its route delivery have necessarily completed. Retry actual navigation
  // only at this initial boundary; never interact with a retained hidden pane.
  let startupRetries = 0;
  for (let attempt = 0; attempt < 3; attempt++) {
    for (let backs = 0; document.documentElement.dataset.view !== 'home' && document.documentElement.dataset.view !== 'machines' && backs < 6; backs++) {
      const previous = document.documentElement.dataset.view; key(window, 'Escape');
      await t.waitFor(() => document.documentElement.dataset.view !== previous);
    }
    if (document.documentElement.dataset.view !== 'machines') {
      const machines = await t.waitFor(() => { const node = document.getElementById('tabMachines'); return visible(node) && node; });
      click(machines);
    }
    const result = await t.waitFor(() => {
      const workspace = document.querySelector('#machines2 #s2-pane-remotes .connection-workspace');
      if (visible(workspace)) return { workspace };
      if (document.documentElement.dataset.view === 'home' && visible(document.getElementById('tabMachines'))) return { retry: true };
      return false;
    });
    if (result.workspace) {
      // A bounded stability check separates initial show/route delivery from
      // the behavior under test. Later interactions never retry navigation.
      await new Promise(resolve => setTimeout(resolve, 350));
      if (visible(result.workspace) && document.documentElement.dataset.view === 'machines') {
        pane = result.workspace.closest('.connections-pane'); break;
      }
    }
    startupRetries++;
  }
  t.assert(visible(pane), 'Native Machines route did not become stably visible after initial show');
  t.evidence('startupNavigationRetries', startupRetries);
  grid = pane.querySelector('.connection-grid'); editor = pane.querySelector('#s2-ssh-setup');
  t.assert(!document.querySelector('#machines2 .sidebar, #machines2 .snav'), 'Standalone Machines contains the Settings sidebar');
  const remotes = await window.jarvis.remotesList();
  const expected = Array.isArray(remotes) ? remotes : remotes.remotes || [];
  t.assert(grid.querySelectorAll('[data-machine-name]').length === expected.length, 'Inventory differs from real registered connections');
  t.assert(getComputedStyle(pane.querySelector('.connection-workspace')).display === 'grid', 'Designed connection CSS did not load');
  t.assert(!visible(editor), 'Add editor obscures the inventory on entry');
  t.assert(grid.querySelectorAll('button button').length === 0, 'Inventory contains nested action buttons');
  t.evidence('inventory', { remoteCount: expected.length, vmCount: grid.querySelectorAll('[data-vm-name]').length });
  const first = grid.querySelector('button.connection-card');
  if (first) {
    click(first);
    t.assert(pane.querySelector('.connection-detail').textContent.includes(first.getAttribute('aria-label')), 'Native card did not select its inspector');
    const settings = button(pane.querySelector('.connection-detail'), 'Настройки');
    if (settings) { click(settings); click(button(pane.querySelector('.connection-detail'), 'Обзор')); }
  }
  await snapshot('native-connections-inventory');
  window.jarvisTheme.adopt({ theme: 'light', mode: 'window' });
  await snapshot('native-connections-inventory-light');
  window.jarvisTheme.adopt({ theme: 'dark', mode: 'window' });
});

await t.step('Cmd8 and navigation Back return to the standalone Machines module', async () => {
  key(window, 'Escape');
  await t.waitFor(() => document.documentElement.dataset.view === 'home');
  key(window, '8', { metaKey: true });
  await t.waitFor(() => document.documentElement.dataset.view === 'machines' && bindVisibleModule());
  key(window, ',', { metaKey: true });
  await t.waitFor(() => document.documentElement.dataset.view === 'settings' && visible(document.querySelector('#settings2 .sidebar')));
  click(document.getElementById('pageBack'));
  await t.waitFor(() => document.documentElement.dataset.view === 'machines' && bindVisibleModule());
  t.assert(document.querySelectorAll('#s2-pane-remotes').length === 1, 'Navigation left duplicate connection controllers');
  t.assert(!document.querySelector('#machines2 .sidebar, #machines2 .snav'), 'Back returned to Settings instead of Machines');
});

await t.step('Machine search is editable and empty results preserve a working Add action', async () => {
  const search = pane.querySelector('[aria-label="Найти машину"]');
  click(search); search.value = 'jarvis-native-qa-no-such-machine'; search.dispatchEvent(new Event('input', { bubbles: true }));
  t.assert(grid.querySelectorAll('[data-machine-name], [data-vm-name]').length === 0, 'Search did not filter native inventory');
  t.assert(visible(button(pane, 'Добавить машину')), 'Search hid Add machine');
  search.value = ''; search.dispatchEvent(new Event('input', { bubbles: true }));
});

await t.step('SSH draft fields are comfortable and survive cancel without a connection request', async () => {
  click(button(pane, 'Добавить машину'));
  await t.waitFor(() => visible(editor));
  let host = editor.querySelector('[aria-label="SSH-хост"]');
  t.assert(visible(host), 'SSH address is not visible');
  t.assert(host.getBoundingClientRect().height >= 36, 'Native address field is compressed');
  click(host); host.value = 'developer@native-fixture.invalid'; host.dispatchEvent(new Event('input', { bubbles: true }));
  await snapshot('native-connections-add-ssh');
  click(button(editor, 'Отмена'));
  t.assert(!visible(editor), 'Cancel did not close the editor');
  click(button(pane, 'Добавить машину'));
  host = editor.querySelector('[aria-label="SSH-хост"]');
  t.assert(host.value === 'developer@native-fixture.invalid', 'Cancelling lost the draft address');
  host.focus(); key(host, 'Escape');
  t.assert(!visible(editor), 'Escape did not close the editor');
  t.assert(document.documentElement.dataset.view === 'machines', 'Escape navigated away from Machines');
});

await t.step('Native machine design fits its scroll area', async () => {
  const workspace = pane.querySelector('.connection-workspace');
  t.assert(pane.scrollWidth <= pane.clientWidth + 1, 'Machines pane overflows horizontally');
  t.assert(workspace.scrollWidth <= workspace.clientWidth + 1, 'Machine workspace overflows horizontally');
  await snapshot('native-connections-complete');
});
