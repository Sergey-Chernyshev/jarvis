// Opens the actual onboarding window and takes its own WKWebView snapshot.
// Does not click install, permissions, microphone, or external-account actions.
await t.step('Native onboarding opens at its intended size', async () => {
  await t.invoke('onboarding_open');
  const windows = await t.waitFor(async () => {
    const state = await t.invoke('native_smoke_probe', { operation: 'window_properties' });
    return state.onboarding?.visible && state;
  });
  const frame = windows.onboarding.frame;
  t.assert(frame.width === 560 && frame.height === 660, 'Onboarding native size differs from its layout');
  t.evidence('nativeWindows', windows);
  t.evidence('onboardingStatus', await t.invoke('onboarding_get'));
  // This separate native window runs its own initialization. Its screenshot is
  // a visual-review artifact, not a claim that every wizard action was tested.
  await new Promise(resolve => setTimeout(resolve, 1000));
  await t.screenshot('native-onboarding-welcome', 'onboarding');
});
