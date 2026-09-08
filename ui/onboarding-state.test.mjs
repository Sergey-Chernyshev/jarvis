import assert from 'node:assert/strict';
import test from 'node:test';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const State = require('./onboarding-state.js');

function snapshot(overrides = {}) {
  return {
    coreReady: false,
    agents: [],
    transport: [],
    capabilities: [],
    warnings: [],
    proxyConfigured: false,
    job: { state: 'idle', kind: '', tasks: [], steps: [], failures: [] },
    ...overrides,
  };
}

test('missing core resolves to agents repair state', () => {
  const view = State.derive(snapshot());
  assert.equal(view.screen, 'agents');
  assert.equal(view.primaryAction, 'repair');
});

test('ready core opens optional capabilities without pretending models exist', () => {
  const view = State.derive(snapshot({
    coreReady: true,
    capabilities: [{ id: 'whisper-turbo', ready: false }],
  }));
  assert.equal(view.screen, 'capabilities');
  assert.equal(view.readyCapabilities, 0);
  assert.equal(view.totalCapabilities, 1);
});

test('running core or model job always owns the screen', () => {
  const view = State.derive(snapshot({
    job: { state: 'running', kind: 'models', tasks: ['silero'], steps: [], failures: [] },
  }));
  assert.equal(view.screen, 'installing');
  assert.equal(view.primaryAction, 'wait');
});

test('failed job is actionable and retains failures', () => {
  const view = State.derive(snapshot({
    job: { state: 'failed', kind: 'core', tasks: [], steps: [], failures: ['hook missing'] },
  }));
  assert.equal(view.screen, 'degraded');
  assert.deepEqual(view.failures, ['hook missing']);
  assert.equal(view.primaryAction, 'retry');
});

test('selection plan includes explicit models only and expands no UI-only runtime', () => {
  assert.deepEqual(
    State.selectedPlan({ whisper: true, qwen: false, wake: false, silero: true, qwenSize: 'qwen3-1.7b' }),
    ['whisper-turbo', 'silero'],
  );
  assert.deepEqual(State.selectedPlan({}), []);
});

test('cold start is an explicit checking state', () => {
  const view = State.derive(null);
  assert.equal(view.screen, 'checking');
  assert.equal(view.primaryAction, 'wait');
});

test('failure classifier gives broken states a concrete recovery route', () => {
  const failed = (message) => State.derive(snapshot({
    job: { state: 'failed', kind: 'models', tasks: ['silero'], steps: [], failures: [message] },
  })).failureKind;
  assert.equal(failed('proxy: network timeout'), 'network');
  assert.equal(failed('No space left on device'), 'disk');
  assert.equal(failed('hook trust permission denied'), 'permission');
  assert.equal(failed('unexpected model format'), 'unknown');
});

test('ready core distinguishes a warming runtime socket from online', () => {
  const view = State.derive(snapshot({
    coreReady: true,
    transport: [{ id: 'socket', ready: false }],
  }));
  assert.equal(view.runtimeState, 'warming');
});

test('stale native reads cannot regress a terminal job or replace a newer job', () => {
  const done = snapshot({ job: { id: 4, state: 'done' } });
  assert.equal(State.mergeSnapshot(done, snapshot({ job: { id: 4, state: 'running' } })).job.state, 'done');
  assert.equal(State.mergeSnapshot(done, snapshot({ job: { id: 3, state: 'failed' } })).job.id, 4);
  assert.equal(State.mergeSnapshot(done, { id: 5, state: 'running' }).job.id, 5);
  assert.equal(State.mergeSnapshot(done, { id: 5, state: 'running' }).coreReady, false);
});

test('progress exposes only actual current-stage percentages', () => {
  assert.equal(State.stepProgress({ steps: [] }).pct, null);
  assert.equal(State.stepProgress({ steps: [{ state: 'start', phase: 'download' }] }).pct, null);
  assert.equal(State.stepProgress({ steps: [{ state: 'info', pct: 53 }] }).pct, 53);
  assert.equal(State.stepProgress({ steps: [{ state: 'info', pct: NaN }] }).pct, null);
  assert.equal(State.stepProgress({ steps: [{ state: 'done', pct: 100 }] }).pct, null);
  const progress = State.stepProgress({ steps: [{ phase: 'old', state: 'warn' }, { phase: 'new', state: 'info', pct: 17 }] });
  assert.equal(progress.latest.phase, 'new');
  assert.equal(progress.pct, 17);
});

test('unavailable and installed choices cannot enter an installation plan', () => {
  assert.deepEqual(State.selectedPlan({ whisper: true, qwen: true, wake: true, silero: true }, [
    { id: 'whisper-turbo', ready: true, available: true },
    { id: 'qwen3-runtime', ready: false, available: false },
    { id: 'hey_jarvis', ready: false, available: false },
    { id: 'silero', ready: false, available: true },
  ]), ['silero']);
});

test('navigation preserves the chosen step but cannot bypass core readiness or a running job', () => {
  assert.equal(State.navigation('ready', snapshot()), 'agents');
  assert.equal(State.navigation('welcome', snapshot({ job: { state: 'running' } }), true), 'installing');
  assert.equal(State.navigation('capabilities', snapshot({ coreReady: true, job: { state: 'failed' } })), 'degraded');
  assert.equal(State.navigation('capabilities', snapshot({ coreReady: true, job: { state: 'failed' } }), true), 'capabilities');
});

test('explicit voice-only navigation never invents agent readiness', () => {
  const withoutAgents = snapshot();
  assert.equal(State.navigation('capabilities', withoutAgents, false, true), 'capabilities');
  assert.equal(State.navigation('ready', withoutAgents, false, true), 'ready');
  assert.equal(State.derive(withoutAgents).runtimeState, 'offline');
  assert.equal(withoutAgents.coreReady, false);
  assert.equal(State.navigation('ready', snapshot({ job: { state: 'running' } }), false, true), 'installing');
});
