import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const read = (name) => readFileSync(new URL(name, import.meta.url), 'utf8');
const flush = () => new Promise((resolve) => setImmediate(resolve));
function fixture({ selected = 'auto', claude = true, codex = true, subscribe, confirmResult = { ok: true } } = {}) {
  const { window, document } = parseHTML(read('./agent-chat.html'));
  const calls = [], listeners = {};
  const provider = document.getElementById('provider');
  // linkedom has a read-only select.value; the browser implements both setter
  // and selectedOptions, so provide those standard DOM surfaces in this test.
  let value = 'auto';
  Object.defineProperty(provider, 'value', { get: () => value, set: (next) => { value = next; } });
  Object.defineProperty(provider, 'selectedOptions', { get: () => [...provider.options].filter((o) => o.value === value) });
  window.__TAURI__ = {
    core: { invoke: async (name, args) => {
      calls.push({ name, args });
      if (name === 'agent_hosts') return { ok: true, selected, providers: [
        { id: 'auto', label: 'Авто', available: claude || codex },
        { id: 'claude', label: 'Claude', available: claude },
        { id: 'codex', label: 'Codex', available: codex },
      ] };
      if (name === 'agent_send') return { ok: true, provider: args.provider === 'auto' ? 'claude' : args.provider };
      if (name === 'agent_confirm') return confirmResult;
      return { ok: true };
    } },
    event: { listen: (name, callback) => {
      listeners[name] = callback;
      return subscribe || Promise.resolve(() => {});
    } },
  };
  new Function('window', 'document', read('./agent-chat.js'))(window, document);
  return { document, window, calls, provider, emit: (payload) => listeners['agent:event']({ payload }),
    confirm: (payload) => listeners['agent:confirm']({ payload }),
    send: (text) => { document.getElementById('input').value = text; document.getElementById('send').click(); } };
}

test('saved explicit Codex selection works with Claude installed and resets cross-provider resume', async () => {
  const f = fixture({ selected: 'codex' });
  await flush();
  assert.equal(f.provider.value, 'codex');
  f.send('hello');
  await flush();
  assert.equal(f.calls.find((c) => c.name === 'agent_send').args.provider, 'codex');
  assert.equal(f.provider.disabled, true);
  f.emit({ type: 'init', session_id: 'codex-session' });
  f.emit({ type: 'done', session_id: 'codex-session' });
  f.provider.value = 'claude';
  f.provider.dispatchEvent(new f.window.Event('change'));
  await flush();
  f.send('fresh conversation');
  await flush();
  const last = f.calls.filter((c) => c.name === 'agent_send').at(-1).args;
  assert.equal(last.sessionId, null);
  assert.equal(last.provider, 'claude');
  assert.deepEqual(f.calls.find((c) => c.name === 'settings_set').args.patch, { agentProvider: 'claude' });
});

test('missing CLIs are explicitly unavailable and cannot accept a send', async () => {
  const f = fixture({ claude: false, codex: false });
  await flush();
  assert.equal(f.document.getElementById('send').disabled, true);
  assert.ok([...f.provider.options].every((option) => option.disabled));
  f.send('test');
  assert.equal(f.calls.some((call) => call.name === 'agent_send'), false);
});

test('sending waits until stream listeners are installed', async () => {
  let subscribed;
  const f = fixture({ subscribe: new Promise((resolve) => { subscribed = resolve; }) });
  await flush();
  f.send('early');
  assert.equal(f.calls.some((call) => call.name === 'agent_send'), false);
  subscribed(() => {});
  await flush();
  f.send('ready');
  await flush();
  assert.equal(f.calls.filter((call) => call.name === 'agent_send').length, 1);
});

test('provider failure unlocks selection so the user can choose another installed CLI', async () => {
  const f = fixture();
  await flush();
  f.send('hello');
  await flush();
  f.emit({ type: 'failed', message: 'OAuth session expired' });
  assert.equal(f.provider.disabled, false);
  assert.equal(f.document.getElementById('send').disabled, false);
  assert.match(f.document.getElementById('msgs').textContent, /OAuth session expired/);
});

test('confirmation buttons are keyboard controls and never report approval for an expired request', async () => {
  const f = fixture({ confirmResult: { ok: false } });
  await flush();
  f.confirm({ nonce: 'expired', id: 'sessions.reply', card: {} });
  const yes = f.document.querySelector('button.cbtn.yes');
  assert.ok(yes, 'semantic button supports Tab and Enter');
  yes.click();
  await flush();
  assert.match(f.document.querySelector('.cresult').textContent, /завершён или истёк/);
  assert.doesNotMatch(f.document.querySelector('.cresult').textContent, /разрешено/);
});
