// Real registered profiles/transcripts, production IPC, isolated debug data.
// No hook repair, agent reply, account change, or process launch.
const sid = '01a06e5a-e84b-7d72-98d7-f33e6ee938d2';
let personal, current;
const visible = element => {
  if (!element || element.closest('[hidden]')) return false;
  for (let parent = element; parent; parent = parent.parentElement) {
    const style = getComputedStyle(parent);
    if (style.display === 'none' || style.visibility === 'hidden') return false;
  }
  const box = element.getBoundingClientRect();
  return box.width > 0 && box.height > 0;
};
const clickVisible = element => {
  t.assert(visible(element), 'Expected a displayed control');
  t.assert(!element.disabled, 'Control is disabled');
  element.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  const box = element.getBoundingClientRect();
  const hit = document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2);
  t.assert(hit === element || element.contains(hit), `Control is covered: ${element.getAttribute('aria-label') || element.textContent}`);
  element.click();
};
const home = async () => {
  // The chat workspace intentionally hides pageHome. Exercise its real Escape
  // navigation instead of programmatically clicking a hidden breadcrumb.
  for (let step = 0; document.documentElement.dataset.view !== 'home' && step < 6; step++) {
    const previous = document.documentElement.dataset.view;
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    await t.waitFor(() => document.documentElement.dataset.view !== previous);
  }
  await t.waitFor(() => visible(document.getElementById('launcher')));
};
await t.step('Discover real Personal and Work profiles separately', async () => {
  const result = await window.jarvis.agentInstancesList();
  t.assert(Array.isArray(result.instances), 'Registry did not return instances');
  personal = result.instances.find(i => i.home.includes('/personal/codex-home'));
  const work = result.instances.find(i => i.home.endsWith('/.codex'));
  t.assert(personal && work && personal.id !== work.id, 'Expected distinct Personal and Work homes');
  t.evidence('profiles', [personal, work].map(i => ({id:i.id,label:i.label,home:i.home,enabled:i.enabled})));
});
await t.step('Production importer finds this actual large Desktop chat', async () => {
  const started = performance.now(); let passes = 0;
  while (!current && passes++ < 28) {
    await t.invoke('native_smoke_probe', {operation:'codex_observe'});
    current = (await window.jarvis.getState()).find(s => s.providerSessionId === sid && s.instanceId === personal.id);
  }
  t.assert(current, 'Current chat missing after bounded transcript scans');
  t.assert(current.instanceId === personal.id, 'Current chat associated with the wrong account');
  t.assert(current.id !== sid && current.controlMode === 'external', 'Desktop identity/control not isolated');
  t.assert(current.transcript?.endsWith(`${sid}.jsonl`), 'Current chat transcript was replaced by a child fork');
  t.assert(current.status === 'working', `Current running turn is ${current.status}`);
  t.evidence('currentChat', {id:current.id,providerSessionId:current.providerSessionId,instanceId:current.instanceId,
    label:current.instanceLabel,status:current.status,controlMode:current.controlMode,passes,elapsedMs:performance.now()-started});
});
await t.step('Real chat is visible in the native chat list with its profile', async () => {
  await home();
  clickVisible(document.getElementById('tabSessions'));
  const row = await t.waitFor(() => [...document.querySelectorAll('.sw-session')].find(e => e.dataset.sessionId === current.id), 15000);
  t.assert(row.textContent.includes(personal.label), 'Chat does not display its profile');
  t.assert(visible(row), 'Current chat row is hidden');
  clickVisible(row);
  await t.waitFor(() => document.querySelector('.sw-chat-context')?.textContent.includes(personal.label), 15000);
  const context = document.querySelector('.sw-chat-context');
  const capability = document.querySelector('.sw-capability');
  t.assert(visible(context), 'Chat context is hidden');
  t.assert(visible(capability) && /только (просмотр|чтение)/i.test(capability.textContent), 'External Desktop chat does not explain read-only access');
  const send = document.getElementById('chatSend'), model = document.querySelector('[aria-label="Модель текущего чата"]');
  t.assert(!visible(send) || send.disabled, 'External Desktop chat exposes an enabled Send control');
  t.assert(!visible(model) || model.disabled, 'External Desktop chat exposes an enabled model picker');
  t.assert(document.documentElement.scrollWidth <= innerWidth+1, 'Chat overflows horizontally');
});
await t.step('New task exposes the selected profile’s actual model catalog without launching an agent', async () => {
  await home(); clickVisible(document.getElementById('tabSessions'));
  const provider = await t.waitFor(() => document.getElementById('newChatProvider'));
  t.assert(visible(provider) && !provider.disabled, 'New task provider is unavailable');
  provider.value = 'codex'; provider.dispatchEvent(new Event('change', { bubbles: true }));
  const profile = await t.waitFor(() => {
    const field = document.getElementById('newChatInstance');
    return visible(field) && !field.disabled && [...field.options].some(option => option.value === personal.id) && field;
  });
  profile.value = personal.id; profile.dispatchEvent(new Event('change', { bubbles: true }));
  const models = await t.waitFor(() => {
    const field = document.getElementById('newChatModel');
    return visible(field) && !field.disabled && field.options.length > 1 && field;
  });
  const catalog = personal.models || [];
  t.assert(catalog.length > 0, 'The actual profile did not expose a model catalog');
  const option = [...models.options].find(option => catalog.some(model => model.value === option.value));
  t.assert(option, 'Model picker does not contain a model from the actual profile catalog');
  t.assert(models.getBoundingClientRect().height >= 28, 'WebKit compressed the model selector');
  models.value = option.value; models.dispatchEvent(new Event('change', { bubbles: true }));
  t.assert(models.value === option.value && profile.value === personal.id, 'Selecting a model changed the profile or lost its selection');
  t.assert(!document.getElementById('newChatPrompt').value, 'Test must never enter a task or launch an agent');
  t.evidence('nativeModelSelection', { instanceId: profile.value, model: models.value, catalogSize: catalog.length });
});
await t.step('Profile settings render in WKWebView and respond', async () => {
  await home();
  clickVisible(document.getElementById('tabSettings'));
  window.jarvisOpenSettingsPane('agents');
  const profile = await t.waitFor(() => [...document.querySelectorAll('.instance-profile')].find(element => element.dataset.instanceId === personal.id));
  t.assert(visible(profile), 'Personal profile is hidden');
  t.assert(profile.querySelector('.instance-name')?.textContent === personal.label, 'Personal label missing from compact profile row');
  t.assert(document.querySelectorAll('.instance-profile').length >= 2, 'Distinct profile rows missing');
  const defaultSelect = document.querySelector('.instance-select');
  t.assert(visible(defaultSelect) && defaultSelect.getBoundingClientRect().height >= 36, 'WebKit ignored profile selector sizing');
  const name = profile.querySelector('input[aria-label="Название профиля"]');
  t.assert(!visible(name), 'Technical editing form should start collapsed');
  const disclosure = profile.querySelector('.instance-disclosure');
  t.assert(disclosure.getAttribute('aria-expanded') === 'false', 'Profile disclosure state is incorrect');
  clickVisible(disclosure);
  await t.waitFor(() => visible(name));
  t.assert(disclosure.getAttribute('aria-expanded') === 'true' && name.value === personal.label && !name.disabled, 'Opened profile does not expose the current editable name');
  t.assert(profile.querySelector('.instance-facts')?.textContent.includes(personal.canonicalHome || personal.home), 'Technical profile details have the wrong home');
  const tracking = profile.querySelector('input[data-instance-id]');
  t.assert(visible(tracking) && tracking.checked === personal.enabled, 'Tracking switch does not reflect real settings');
  clickVisible(disclosure);
  t.assert(!visible(name) && disclosure.getAttribute('aria-expanded') === 'false', 'Profile details did not collapse');
  t.assert(document.documentElement.scrollWidth <= innerWidth+1, 'Settings overflow horizontally');
  // A background WebKit compositor may suspend finite entry animations.
  // Finish them only for the static snapshot, never as a motion assertion.
  await t.waitFor(() => {
    const running = document.getAnimations().filter(a => a.playState === 'running' && Number.isFinite(a.effect?.getComputedTiming().endTime));
    if (!document.hasFocus() && running.length) {
      running.forEach(a => a.finish());
      t.evidence('snapshotPolicy', 'Finite animations finished while occluded for a static layout snapshot; motion is tested separately.');
      return true;
    }
    return !running.length;
  });
  const panel = document.getElementById('s2-pane-agents');
  t.assert(panel.getBoundingClientRect().width > 100 && getComputedStyle(panel).display !== 'none', 'Profile panel is hidden');
  await t.screenshot('native-profiles');
});
