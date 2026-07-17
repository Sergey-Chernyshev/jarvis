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
