// Real WKWebView/AppKit/AX observations. The fields and HUD are synthetic;
// nothing is pasted, copied, spoken, or recorded. Permission is never requested.
const probe = operation => t.invoke('native_smoke_probe', { operation });
const settle = () => new Promise(resolve => setTimeout(resolve, 150));
const permissionBefore = await probe('permissions');
t.evidence('permissionBefore', permissionBefore);

await t.step('Native device metadata stays responsive without capture', async () => {
  const started = performance.now();
  const result = await window.jarvis.sttInputDevices();
  t.assert(Array.isArray(result.devices), 'Native device result is malformed');
  t.assert(!result.error, `Native discovery failed: ${result.error}`);
  t.assert(performance.now() - started < 5000, 'Native discovery exceeded its deadline');
  t.evidence('discovery', { count: result.devices.length, elapsedMs: performance.now() - started });
});

await t.step('Voice-history edits and deletions survive actual disk round trips', async () => {
  const seeded = await probe('seed_transcript');
  const listed = await t.invoke('transcripts_get');
  t.assert(listed.items.some(item => item.id === seeded.id), 'Synthetic transcript is absent from runtime');
  const before = await probe('transcript_disk');
  t.assert(before.items.some(item => item.id === seeded.id), 'Synthetic transcript was not persisted');
  const edited = await t.invoke('transcript_update', { id: seeded.id, text: '  Синтетическая правка  ' });
  t.assert(edited.ok === true && edited.text === 'Синтетическая правка', 'Edit did not acknowledge persisted text');
  const diskEdited = (await probe('transcript_disk')).items.find(item => item.id === seeded.id);
  t.assert(diskEdited?.text === edited.text && !diskEdited.appliedStyle, 'Disk content or style differs from acknowledged edit');
  const rejected = await t.invoke('transcript_update', { id: seeded.id, text: '   ' });
  t.assert(rejected.ok === false && rejected.error, 'Empty edit claimed success');
  t.assert((await probe('transcript_disk')).items.find(item => item.id === seeded.id)?.text === edited.text,
    'Rejected edit changed disk content');
  const removed = await t.invoke('transcript_delete', { id: seeded.id });
  t.assert(removed.ok === true, 'Delete was not acknowledged');
  t.assert(!(await probe('transcript_disk')).items.some(item => item.id === seeded.id), 'Deleted transcript remains on disk');
  const stale = await t.invoke('transcript_update', { id: seeded.id, text: 'Поздний результат' });
  t.assert(stale.ok === false && stale.error, 'Missing transcript edit claimed success');
  const retry = await t.invoke('transcript_retranscribe', { id: seeded.id });
  t.assert(retry.ok === false && retry.error, 'Missing transcript retranscription claimed success');
  t.evidence('historyRoundTrip', { verified: true, id: seeded.id,
    provenance: 'actual Tauri commands and JSON file in isolated profile; fixed synthetic text only' });
});

await t.step('Synthetic HUD uses a visible nonactivating native window', async () => {
  const before = await probe('window_properties');
  t.evidence('windowsBefore', before);
  await probe('toast_present');
  const after = await t.waitFor(async () => {
    const state = await probe('window_properties');
    return state.toast?.visible && state.toast.frame.height > 10 && state;
  });
  t.evidence('windowsWithToast', after);
  const toast = after.toast;
  t.assert(!toast.key && !toast.canBecomeKey, 'HUD acquired keyboard focus');
  t.assert(after.main.key === before.main.key, 'HUD changed the main window focus');
  t.assert(toast.onActiveSpace, 'HUD is absent from the current Space');
  t.assert(toast.joinsAllSpaces && toast.fullScreenAuxiliary && toast.ignoresCycle,
    'HUD native Space/cycle configuration is incomplete');
  t.assert(toast.level > 0, 'HUD is not above ordinary windows');
  const f = toast.frame, a = toast.screenWorkArea;
  t.assert(a && f.x >= a.x && f.y >= a.y &&
    f.x + f.width <= a.x + a.width + 1 && f.y + f.height <= a.y + a.height + 1,
    'HUD escapes its native display work area');
  t.assert(Math.abs(f.x + f.width / 2 - a.x - a.width / 2) <= 1,
    'HUD is not centered in native display coordinates');
  const rendered = await t.waitFor(async () => {
    const state = await probe('toast_render_state');
    return state.phase === 'error' && state.opacity === 1 && state.finiteAnimations === 0 && state;
  });
  t.evidence('toastRendered', rendered);
  await t.screenshot('native-synthetic-toast', 'toast');
  await probe('toast_dismiss');
});

await t.step('Original field identity is compared through production AX when authorized', async () => {
  const fixture = document.createElement('section');
  fixture.id = 'native-ax-fixture';
  Object.assign(fixture.style, { position: 'fixed', inset: '80px 30px', zIndex: '2147483647',
    padding: '24px', background: '#19212c', color: '#fff', borderRadius: '20px' });
  fixture.innerHTML = '<h2>Изолированная проверка фокуса</h2><p>Два тестовых поля. Вставка и буфер обмена не используются.</p>' +
    '<label>Поле A<textarea id="scratch-a" aria-label="Scratch field A"></textarea></label>' +
    '<label>Поле B<textarea id="scratch-b" aria-label="Scratch field B"></textarea></label>';
  document.body.append(fixture);
  try {
    const a = fixture.querySelector('#scratch-a'), b = fixture.querySelector('#scratch-b');
    a.focus(); await settle();
    t.assert(document.activeElement === a, 'Scratch field A did not receive DOM focus');
    const captured = await probe('focus_capture');
    t.evidence('axCapture', captured);
    if (!captured.available) {
      t.evidence('axVerification', { verified: false,
        reason: 'Production AX unavailable in the isolated bundle; permission remains user-controlled' });
      return;
    }
    const unchanged = await probe('focus_compare');
    t.assert(unchanged.available && unchanged.sameElement === true,
      'Retained original AX field did not compare equal to itself');
    b.focus(); await settle();
    t.assert(document.activeElement === b, 'Scratch field B did not receive DOM focus');
    const changed = await probe('focus_compare');
    t.evidence('axChangedField', changed);
    t.assert(changed.available && changed.sameElement === false,
      'Production AX failed to distinguish two fields in the same process');
    a.focus(); await settle();
    const restored = await probe('focus_compare');
    t.assert(restored.available && restored.sameElement === true,
      'Original retained AX field no longer compares equal after refocus');
    t.evidence('axVerification', { verified: true, provenance: 'real AX with two synthetic WKWebView textareas; no paste' });
    await t.screenshot('native-ax-scratch-fields');
  } finally { fixture.remove(); }
});

await t.step('Read-only journey leaves native authorization unchanged', async () => {
  const after = await probe('permissions');
  t.evidence('permissionAfter', after);
  t.assert(after.microphone === permissionBefore.microphone &&
    after.accessibility === permissionBefore.accessibility, 'Authorization changed during a read-only journey');
});
