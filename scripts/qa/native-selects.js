// Runs inside the real WKWebView with its production bridge and real catalogs.
// Only new-task draft controls change. No task, hook repair, profile save,
// microphone capture, remote connection, or model execution is requested.
const visible = element => {
  if (!element || !element.isConnected || element.closest('[hidden]')) return false;
  for (let current = element; current; current = current.parentElement) {
    const style = getComputedStyle(current);
    if (style.display === 'none' || style.visibility === 'hidden') return false;
  }
  const rect = element.getBoundingClientRect();
  return rect.width > 0 && rect.height > 0;
};
const sendKey = (element, key) => element.dispatchEvent(new KeyboardEvent('keydown', {
  key, bubbles: true, cancelable: true,
}));
const click = element => {
  t.assert(visible(element) && !element.disabled, 'Expected a visible, enabled control');
  element.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  const rect = element.getBoundingClientRect();
  const hit = document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2);
  t.assert(hit === element || element.contains(hit), `Control is covered: ${element.getAttribute('aria-label') || element.textContent}`);
  element.click();
};
const triggerFor = async select => t.waitFor(() => [...document.querySelectorAll('button[role="combobox"]')]
  .find(button => visible(button) && button.getAttribute('aria-label') === select.getAttribute('aria-label')));
const menuFor = async trigger => t.waitFor(() => {
  const controlled = document.getElementById(trigger.getAttribute('aria-controls'));
  return trigger.getAttribute('aria-expanded') === 'true' && visible(controlled) && controlled;
});
const selectedLabel = select => select.selectedOptions[0]?.label || select.selectedOptions[0]?.textContent || '';
const assertSelection = (select, trigger) => {
  t.assert(trigger.textContent.includes(selectedLabel(select)), 'Designed control differs from the underlying selected value');
  t.assert(trigger.type === 'button', 'Combobox must not submit its surrounding form');
  t.assert(select.getAttribute('aria-hidden') === 'true', 'Native select creates a duplicate accessible control');
};
const closeMenu = async trigger => {
  sendKey(document.activeElement?.isConnected ? document.activeElement : trigger, 'Escape');
  await t.waitFor(() => trigger.getAttribute('aria-expanded') === 'false');
  t.assert(document.activeElement === trigger, 'Escape did not restore focus to the select trigger');
};
const home = async () => {
  for (let count = 0; document.documentElement.dataset.view !== 'home' && count < 6; count++) {
    const previous = document.documentElement.dataset.view;
    sendKey(window, 'Escape');
    await t.waitFor(() => document.documentElement.dataset.view !== previous);
  }
  await t.waitFor(() => visible(document.getElementById('launcher')));
};
const snapshot = async name => {
  await t.waitFor(() => {
    const running = document.getAnimations().filter(animation => animation.playState === 'running' && Number.isFinite(animation.effect?.getComputedTiming().endTime));
    if (!document.hasFocus() && running.length) {
      running.forEach(animation => animation.finish());
      t.evidence('snapshotPolicy', 'Finite animations finished for static layout snapshots while occluded; this is not a motion assertion.');
      return true;
    }
    return !running.length;
  });
  t.assert(document.documentElement.scrollWidth <= innerWidth + 1, 'Document overflows horizontally');
  await t.screenshot(name);
};
let provider, providerTrigger, model, modelTrigger, realProfile;

await t.step('New-task provider uses a designed, accessible popup above the composer', async () => {
  await home(); click(document.getElementById('tabSessions'));
  provider = await t.waitFor(() => document.getElementById('newChatProvider'));
  providerTrigger = await triggerFor(provider);
  t.assert(providerTrigger.getBoundingClientRect().height >= 28, 'Provider trigger is compressed in WKWebView');
  assertSelection(provider, providerTrigger);
  click(providerTrigger);
  const menu = await menuFor(providerTrigger);
  t.assert(menu.getAttribute('role') === 'listbox', 'Popup does not expose listbox semantics');
  t.assert(!provider.closest('form').contains(menu), 'Popup remains inside the composer clipping ancestor');
  const box = menu.getBoundingClientRect();
  t.assert(box.left >= 0 && box.top >= 0 && box.right <= innerWidth + 1 && box.bottom <= innerHeight + 1, 'Provider popup escapes the viewport');
  t.assert([...menu.querySelectorAll('[role="option"]')].some(option => option.getAttribute('aria-selected') === 'true'), 'Current option is not announced');
  await snapshot('native-select-provider-dark');
  const codex = [...menu.querySelectorAll('[role="option"]')].find(option => option.textContent.trim() === 'Codex');
  t.assert(codex, 'Codex option is absent'); click(codex);
  await t.waitFor(() => provider.value === 'codex' && providerTrigger.getAttribute('aria-expanded') === 'false');
  assertSelection(provider, providerTrigger);
});

await t.step('Real profile and model catalogs populate the designed controls asynchronously', async () => {
  const registry = await window.jarvis.agentInstancesList();
  const profile = await t.waitFor(() => {
    const select = document.getElementById('newChatInstance');
    return select && !select.hidden && !select.disabled && select.options.length && select;
  });
  const profileTrigger = await triggerFor(profile);
  realProfile = registry.instances.find(instance => instance.id === profile.value);
  t.assert(realProfile && realProfile.enabled, 'Draft profile does not match an enabled registered profile');
  assertSelection(profile, profileTrigger);
  model = await t.waitFor(() => {
    const select = document.getElementById('newChatModel');
    return select && !select.hidden && !select.disabled && select.options.length > 1 && select;
  });
  modelTrigger = await triggerFor(model);
  t.assert((realProfile.models || []).some(catalogModel => [...model.options].some(option => option.value === catalogModel.value)), 'Popup models do not come from the actual selected profile');
  assertSelection(model, modelTrigger);
  click(modelTrigger); await menuFor(modelTrigger);
  await snapshot('native-select-model-dark');
  await closeMenu(modelTrigger);
  t.assert(document.documentElement.dataset.view === 'list', 'Escape closed the chat page instead of only the popup');
  t.evidence('realCatalog', { profileLabel: realProfile.label, modelCount: model.options.length });
});

await t.step('Arrow navigation is a draft until Enter commits exactly one selection', async () => {
  const previous = model.value;
  let changes = 0; const onChange = () => changes++;
  model.addEventListener('change', onChange);
  click(modelTrigger); const menu = await menuFor(modelTrigger);
  // Home/End edit the text when a searchable picker focuses its search input.
  // Dispatch to the owning combobox to explicitly exercise option navigation.
  sendKey(modelTrigger, 'End');
  t.assert(model.value === previous && changes === 0, 'Highlight movement committed an unintended model');
  const expected = [...model.options].filter(option => !option.disabled).at(-1)?.value;
  sendKey(document.activeElement, 'Enter');
  await t.waitFor(() => modelTrigger.getAttribute('aria-expanded') === 'false' && model.value === expected);
  t.assert(changes === (expected === previous ? 0 : 1), 'Selection emitted duplicate changes');
  model.removeEventListener('change', onChange);
  assertSelection(model, modelTrigger);
  t.assert(!document.getElementById('newChatPrompt').value, 'Test entered or launched an agent task');
  await t.waitFor(() => !visible(menu));
});

await t.step('Disabled permissions remain unavailable and Escape preserves the draft', async () => {
  const permissions = document.querySelector('select[aria-label="Разрешения новой задачи"]');
  const trigger = await triggerFor(permissions);
  const previous = permissions.value;
  click(trigger); const menu = await menuFor(trigger);
  const plan = [...menu.querySelectorAll('[role="option"]')].find(option => option.textContent.includes('Планирование'));
  t.assert(plan && plan.getAttribute('aria-disabled') === 'true', 'Claude-only planning remains actionable for Codex');
  plan.click();
  t.assert(permissions.value === previous, 'Disabled planning option changed permissions');
  await closeMenu(trigger);
  t.assert(document.documentElement.dataset.view === 'list', 'Escape left the new-task screen');
});

await t.step('Machine popup cancels without altering machine or profile', async () => {
  const machine = document.getElementById('newChatMachine');
  const trigger = await triggerFor(machine);
  const before = { machine: machine.value, profile: document.getElementById('newChatInstance').value };
  click(trigger); await menuFor(trigger); await closeMenu(trigger);
  t.assert(machine.value === before.machine && document.getElementById('newChatInstance').value === before.profile, 'Cancelled machine popup changed task routing');
  assertSelection(machine, trigger);
});

await t.step('Default profile settings popup is readable and cancels without saving', async () => {
  const before = (await window.jarvis.agentInstancesList()).defaultCodexInstance;
  await home(); click(document.getElementById('tabSettings'));
  window.jarvisOpenSettingsPane('agents');
  const select = await t.waitFor(() => document.querySelector('select[aria-label="Профиль Codex по умолчанию"]'));
  const trigger = await triggerFor(select);
  assertSelection(select, trigger);
  t.assert(trigger.getBoundingClientRect().height >= 32, 'Settings trigger has an insufficient hit area');
  const label = [...document.querySelectorAll('label')].find(element => element.htmlFor === select.id);
  t.assert(label, 'Profile setting lost its visible label');
  click(label);
  t.assert(document.activeElement === trigger, 'Visible form label does not focus the designed control');
  if (trigger.getAttribute('aria-expanded') !== 'true') click(trigger);
  await menuFor(trigger); await snapshot('native-select-profile-dark');
  await closeMenu(trigger);
  t.assert(document.documentElement.dataset.view === 'settings', 'First Escape left settings');
  const after = (await window.jarvis.agentInstancesList()).defaultCodexInstance;
  t.assert(before === after && select.value === before, 'Opening or cancelling saved a different default profile');
  window.jarvisTheme.adopt({ theme: 'light' });
  await t.waitFor(() => document.documentElement.dataset.theme === 'light');
  click(trigger); await menuFor(trigger); await snapshot('native-select-profile-light');
  await closeMenu(trigger);
});
