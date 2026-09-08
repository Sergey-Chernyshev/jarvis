// A real AppKit fullscreen Space, owned by our disposable editor, remains active
// throughout production HUD events, native resizing, phase changes, and hiding.
// Production CGEvent inserts only a fixed marker into the owned editor. Its
// helper restores the clipboard if no intervening copy occurred. No user window
// is controlled; no microphone or display capture is used.
const probe = operation => t.invoke('native_smoke_probe', { operation });
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const state = async () => { try { return await probe('editor_state'); } catch { return null; } };
const permissionsBefore = await probe('permissions');
const requestedPolicy = (await t.invoke('settings_get')).nativeSmokeFullscreenPolicy || 'accessory';
let editor;
let observingActivation = false;
const phases = [];
const transitionFrames = [];
const insertions = [];
const marker = 'Jarvis native paste QA 👋';
const nativeActionTimeline = [];
const markAction = (operation, cycle) => {
  nativeActionTimeline.push({ operation, cycle, timestampMs:Date.now() });
  t.evidence('nativeActionTimeline', nativeActionTimeline);
};
const assertFullscreen = value => {
  t.assert(value?.fullscreen && value.fullscreenEntered && value.fullscreenTransition === '',
    `Owned editor lost its actual fullscreen lifecycle: ${JSON.stringify(value)}`);
  t.assert(value.foregroundPid === editor.pid && value.focused && value.key && value.onActiveSpace,
    `HUD displaced the active fullscreen editor: ${JSON.stringify(value)}`);
  t.assert(value.windowServerFrameStable, 'WindowServer moved the owned fullscreen surface away from its active display');
  t.assert(value.violations.length === 0,
    `The independent 40 ms native observer saw a focus or Space change: ${JSON.stringify(value.violations)}`);
};
const assertHUD = async phase => {
  const windows = await probe('window_properties');
  const toast = windows.toast;
  const observed = await t.waitFor(async () => {
    const value = await state();
    assertFullscreen(value);
    return value.ownedOnScreenWindows.some(w => w.number === toast.windowNumber) && value;
  });
  const overlay = observed.ownedOnScreenWindows.find(w => w.number === toast.windowNumber);
  const fullscreenWindow = observed.ownedOnScreenWindows.find(w => w.number === observed.windowNumber);
  t.assert(fullscreenWindow, 'Fullscreen editor is absent from WindowServer on-screen windows');
  t.assert(toast.visible && toast.onActiveSpace && !toast.key && !toast.canBecomeKey,
    `HUD is not visible without focus in the actual fullscreen Space: ${JSON.stringify(toast)}`);
  t.assert(toast.isPanel && toast.nonactivatingPanel && typeof toast.nativeClass === 'string' && toast.nativeClass.length > 0,
    `HUD is not hosted in an actual native nonactivating NSPanel: ${JSON.stringify(toast)}`);
  t.assert(overlay.alpha > 0 && overlay.layer > fullscreenWindow.layer &&
    overlay.frontToBackOrder < fullscreenWindow.frontToBackOrder,
    'WindowServer does not place the HUD above the visible fullscreen editor');
  const b = overlay.bounds, f = fullscreenWindow.bounds;
  t.assert(b.Width > 0 && b.Height > 0 && b.X >= f.X - 2 && b.Y >= f.Y - 2 &&
    b.X + b.Width <= f.X + f.Width + 2 && b.Y + b.Height <= f.Y + f.Height + 2,
    'WindowServer placed the HUD outside the physically visible fullscreen editor');
  const identity = await probe('focus_compare');
  t.assert(identity.available && identity.sameElement && identity.ownsForeground,
    `The original real AX input lost focus during ${phase}: ${JSON.stringify(identity)}`);
  return { phase, editor:observed, toast, overlay, fullscreenWindow, identity };
};

try {
  await t.step('The owned AppKit editor enters a real fullscreen Space', async () => {
    t.assert(permissionsBefore.accessibility,
      'This real AX regression requires existing Accessibility authorization; the fixture does not request it');
    t.assert(['accessory', 'regular'].includes(requestedPolicy), 'Unknown fixture activation policy');
    const policy = await probe(`app_policy_${requestedPolicy}`);
    t.evidence('jarvisActivationPolicy', policy);
    // AppKit may return false when the requested policy is already in effect.
    // The actual observed policy, not the setter's change flag, is the contract.
    t.assert(policy.policy === requestedPolicy,
      `Native activation policy was not applied: ${JSON.stringify(policy)}`);
    await probe('app_activation_observe');
    observingActivation = true;
    editor = await probe('editor_start');
    await t.waitFor(async () => (await state())?.focused);
    await probe('editor_enter_fullscreen');
    const entered = await t.waitFor(async () => {
      const value = await state();
      return value?.fullscreen && value.fullscreenEntered && !value.fullscreenTransition &&
        value.focused && value.key && value.onActiveSpace && value;
    });
    t.assert(entered.fullscreenEvents.includes('will-enter') && entered.fullscreenEvents.includes('did-enter'),
      'No genuine NSWindow fullscreen transition was observed');
    // AppKit excludes the hardware camera housing on notched displays unless
    // the application opts into drawing below it. Match actual native bounds,
    // never a percentage threshold that a maximized ordinary window could pass.
    const matchesFrame = candidate => candidate && ['x', 'y', 'width', 'height']
      .every(key => Math.abs(entered.frame[key] - candidate[key]) <= 2);
    t.assert(matchesFrame(entered.screenFrame) || matchesFrame(entered.screenSafeAreaFrame),
      'Editor window did not fill its native display or native notch-safe area');
    // did-enter can precede the WindowServer's final Space-animation frame.
    // Begin observation only after the intentional entry is physically settled.
    let stableSince = 0;
    await t.waitFor(async () => {
      const value = await state();
      if (!value?.windowServerFrameStable || !value.focused || !value.key) {
        stableSince = 0; return false;
      }
      if (!stableSince) stableSince = Date.now();
      return Date.now() - stableSince >= 500;
    });
    const focus = await probe('focus_capture');
    t.assert(focus.available && focus.ownsForeground, 'Could not retain the fullscreen editor AX field');
    t.evidence('jarvisActivationBaseline', await probe('app_activation_checkpoint'));
    await probe('editor_monitor_on');
    await t.waitFor(async () => (await state())?.monitoring);
    assertFullscreen(await state());
    t.evidence('fullscreenBaseline', { editor:await state(), focus, permissionsBefore });
  });

  for (let cycle = 0; cycle < 2; cycle++) {
    for (const phase of ['listening', 'analyzing', 'empty']) {
      await t.step(`Fullscreen cycle ${cycle + 1}: production HUD ${phase} preserves the Space and input`, async () => {
        const before = await probe('toast_render_state');
        // This is the system accessibility preference shared by both native
        // WKWebViews. The fixture never disables it or alters toast animations.
        const reducedMotion = !!window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
        const transition = { cycle:cycle + 1, phase, reducedMotion, before, frames:[] };
        transitionFrames.push(transition);
        t.evidence('transitionFrames', transitionFrames);
        markAction(`toast_${phase}`, cycle + 1);
        await probe(`toast_${phase}`);
        const until = Date.now() + 10000;
        const needsIntermediateMotion = !reducedMotion && (phase === 'listening' || phase === 'empty');
        let sawIntermediateMotion = false;
        let rendered;
        do {
          assertFullscreen(await state());
          const value = await probe('toast_render_state');
          transition.frames.push({ timestampMs:Date.now(), ...value });
          if (value.phase === phase && (value.finiteAnimations > 0 || (value.opacity > 0 && value.opacity < 1)))
            sawIntermediateMotion = true;
          if (value.phase === phase && value.opacity === 1 && value.finiteAnimations === 0 &&
            (!needsIntermediateMotion || sawIntermediateMotion)) {
            rendered = value;
            break;
          }
          await pause(35);
        } while (Date.now() < until);
        if (needsIntermediateMotion)
          t.assert(sawIntermediateMotion, `No intermediate native WKWebView animation frame was observed for ${phase}`);
        t.assert(rendered, `Native WKWebView ${phase} animation did not settle`);
        if (phase === 'empty' && !reducedMotion) {
          const fromWidth = before.rect?.width, toWidth = rendered.rect?.width;
          t.assert(Number.isFinite(fromWidth) && Number.isFinite(toWidth) && Math.abs(toWidth - fromWidth) > 2,
            'The compact HUD and its result card do not have distinct native rendered widths');
          const minWidth = Math.min(fromWidth, toWidth), maxWidth = Math.max(fromWidth, toWidth);
          t.assert(transition.frames.some(frame => frame.phase === 'empty' &&
            frame.rect?.width > minWidth + 0.5 && frame.rect?.width < maxWidth - 0.5),
            'The native HUD jumped between compact and result widths without an intermediate rendered frame');
        }
        // Do not finish or disable animations: the actual visible WKWebView
        // must complete its own transition while the main app is in the back.
        for (let sample = 0; sample < 10; sample++) {
          assertFullscreen(await state());
          await pause(80);
        }
        const proof = await assertHUD(phase);
        phases.push({ cycle:cycle + 1, rendered, intermediateMotionObserved:sawIntermediateMotion,
          reducedMotion, ...proof });
        t.evidence('fullscreenPhases', phases);
        if (cycle === 0 && phase === 'empty') await t.screenshot('fullscreen-empty-hud', 'toast');
      });
    }
    await t.step(`Fullscreen cycle ${cycle + 1}: production paste reaches the original field while the HUD remains visible`, async () => {
      await assertHUD('empty before insertion');
      const before = await state();
      t.assert(before.a === marker.repeat(cycle) && before.b === '',
        'The owned editor contains unexpected text before insertion');
      markAction('editor_paste', cycle + 1);
      const outcome = await probe('editor_paste');
      t.assert(outcome.copied && outcome.pasteSent && outcome.confirmed && !outcome.error,
        `Production insertion was not confirmed through AX: ${JSON.stringify(outcome)}`);
      const actual = await t.waitFor(async () => {
        const value = await state();
        assertFullscreen(value);
        t.assert(value.b === '', 'Production paste reached the second field');
        return value.a === marker.repeat(cycle + 1) && value;
      });
      const proof = await assertHUD('empty after insertion');
      const rendered = await probe('toast_render_state');
      t.assert(rendered.phase === 'empty' && rendered.opacity === 1,
        'The HUD stopped being visible during the insertion regression');
      insertions.push({ cycle:cycle + 1, outcome, actualText:actual.a, otherField:actual.b,
        rendered, ...proof });
      t.evidence('fullscreenInsertions', insertions);
    });
    await t.step(`Fullscreen cycle ${cycle + 1}: dismissing the HUD keeps the original Space`, async () => {
      markAction('toast_dismiss', cycle + 1);
      await probe('toast_dismiss');
      await t.waitFor(async () => {
        assertFullscreen(await state());
        return !(await probe('window_properties')).toast?.visible;
      });
      await pause(300);
      assertFullscreen(await state());
    });
  }
  const monitored = await state();
  t.assert(monitored.monitoredSamples >= 100, 'Too few independent native samples covered the HUD lifecycle');
  t.assert(monitored.fullscreenEvents.filter(event => event === 'did-enter').length === 1 &&
    !monitored.fullscreenEvents.includes('will-exit'), 'The helper unexpectedly left or re-entered fullscreen');
  t.evidence('fullscreenMonitoring', monitored);
  t.evidence('scope', {
    provenance:'Actual NSWindow toggleFullScreen delegate lifecycle, NSWorkspace foreground PID, retained production AX field, and WindowServer on-screen order/bounds; 40 ms independent native sampling includes fullscreen surface movement',
    capture:'Only the owned toast WKWebView is snapshotted; no display pixels or user window metadata is captured',
    trigger:'Synthetic listening → analyzing → empty → production CGEvent paste into the retained original AX field → dismiss; two consecutive cycles through the production HUD event, DOM, resize IPC, and native show path',
    insertion:'Fixed synthetic marker only, exact original-field content checked after each paste, second field remains empty, clipboard restored by the owned helper if no intervening copy occurs',
    excluded:['microphone capture', 'global hotkey callback', 'user applications', 'multiple physical displays']
  });
} catch (error) {
  if (editor && (await state())?.monitoring) {
    // Keep the owned observer running after the first failed assertion so a
    // WindowServer slide can finish and publish its delayed Space/activation
    // notifications. Do not dismiss, refocus, or repair anything in this gap.
    const observation = { failure:String(error), startedAtMs:Date.now(), states:[] };
    const until = observation.startedAtMs + 1800;
    do {
      const value = await state();
      if (!value) break;
      observation.states.push(value);
      await pause(150);
    } while (Date.now() < until);
    observation.finishedAtMs = Date.now();
    t.evidence('fullscreenAfterFailure', observation);
  }
  throw error;
} finally {
  if (observingActivation) {
    try {
      t.evidence('jarvisActivationEvents', await probe('app_activation_events'));
      await probe('app_activation_stop');
    } catch (error) {
      // Diagnostics must never prevent the owned fullscreen editor's cleanup.
      t.evidence('jarvisActivationObservationError', String(error));
    }
  }
  if (editor) {
    const last = await state();
    if (last) t.evidence('fullscreenBeforeCleanup', last);
    try {
      await probe('toast_dismiss');
      await probe('editor_monitor_off');
      await t.waitFor(async () => !(await state())?.monitoring);
      if ((await state())?.fullscreen) {
        await probe('editor_exit_fullscreen');
        await t.waitFor(async () => {
          const value = await state();
          return value && !value.fullscreen && !value.fullscreenTransition && value.fullscreenEvents.includes('did-exit');
        });
      }
      t.evidence('fullscreenCleanup', await state());
    } finally { await probe('editor_stop'); }
  }
}

await t.step('The fullscreen regression never changes native permissions', async () => {
  const after = await probe('permissions');
  t.assert(after.microphone === permissionsBefore.microphone && after.accessibility === permissionsBefore.accessibility,
    'Authorization changed during a synthetic HUD regression');
  t.evidence('permissionsAfter', after);
});
