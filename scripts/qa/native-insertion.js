// Actual production AX identity and CGEvent paste into a dedicated AppKit child.
// The helper restores all clipboard formats if no intervening copy occurred.
const probe = operation => t.invoke('native_smoke_probe', { operation });
const state = async () => { try { return await probe('editor_state'); } catch { return null; } };
const marker = 'Jarvis native paste QA 👋';
await t.step('A dedicated native editor exposes an actual AX field', async () => {
  const permission = await probe('permissions');
  t.assert(permission.accessibility, 'Accessibility is not available; no permission is requested by this fixture');
  await probe('editor_start');
  await t.waitFor(async () => (await state())?.focused);
  const capture = await t.waitFor(async () => { const value=await probe('focus_capture'); return value.available && value; });
  t.evidence('capture', capture);
  const unchanged=await probe('focus_compare');
  t.assert(unchanged.sameElement === true, 'Retained scratch field differs from itself');
});
try {
  await t.step('A different field in the same app refuses a stale paste target', async () => {
    await probe('editor_focus_b');
    await t.waitFor(async () => (await state())?.command === 'focus-b');
    const different=await probe('focus_compare');
    t.assert(different.sameElement === false, 'Two scratch fields compare equal');
    const result=await probe('editor_paste');
    t.assert(result.copied && !result.pasteSent && !result.confirmed && result.error, 'Stale target allowed an unintended paste');
    const fields=await state();t.assert(fields.a === '' && fields.b === '', 'Refused paste changed an editor field');
    t.evidence('staleField',result);
  });
  await t.step('Production CGEvent pastes and AX confirms text in the original field', async () => {
    await probe('editor_focus_a');
    await t.waitFor(async () => (await state())?.command === 'focus-a');
    t.assert((await probe('focus_compare')).sameElement === true, 'Original field identity was lost');
    const result=await probe('editor_paste');
    t.assert(result.pasteSent && result.confirmed && !result.error, `Paste was not confirmed: ${JSON.stringify(result)}`);
    await t.waitFor(async () => (await state())?.a === marker);
    t.assert((await state()).b === '', 'Text reached the wrong field');
    t.evidence('actualInsertion',result);
  });
  await t.step('Repeated insertion is distinguished from text already present', async () => {
    const result=await probe('editor_paste');
    t.assert(result.confirmed && result.pasteSent, 'Second paste was not confirmed');
    await t.waitFor(async () => (await state())?.a === marker + marker);
    t.evidence('repeatInsertion',result);
  });
} finally { await probe('editor_stop'); }
