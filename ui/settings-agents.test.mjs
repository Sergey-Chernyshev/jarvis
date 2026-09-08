import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const code = readFileSync(new URL('./settings2.js', import.meta.url), 'utf8');
const flush = () => new Promise(resolve => setImmediate(resolve));
const qwen = { id: 'qwen', name: 'Qwen Code', bin: '/tools/qwen', resume: 'qwen --resume {sid}', dangerousFlag: '--yolo' };
const presets = [{ id: 'opencode', name: 'OpenCode', bin: 'opencode', resume: '' }, { id: 'pi', name: 'Pi', bin: 'pi', resume: '' }];

async function fixture({ agents = [], save = async () => ({ ok: true }) } = {}) {
  const { window, document } = parseHTML('<html><head></head><body><div id="root"></div></body></html>');
  const calls = [];
  window.jarvisIcons = { create: () => document.createElement('svg') };
  window.jarvisKeys = { isMac: true };
  window.jarvis = {
    getMeta: async () => ({ version: 'test' }),
    agentsList: async () => ({ ok: true, agents: structuredClone(agents), presets }),
    agentsSave: async next => { calls.push(structuredClone(next)); return save(next); },
  };
  new Function('window', 'document', code)(window, document);
  window.jarvisOpenSettingsPane('agents');
  window.initSettings2(document.getElementById('root'));
  await flush();
  const section = document.querySelector('.s2-custom-agents');
  const button = name => [...section.querySelectorAll('button')].find(button => button.textContent === name);
  const field = name => section.querySelector(`[name="${name}"]`);
  const input = (name, value) => { field(name).value = value; field(name).dispatchEvent(new window.Event('input')); };
  const submit = async () => { section.querySelector('form').dispatchEvent(new window.Event('submit', { cancelable: true })); await flush(); };
  return { window, document, section, calls, button, field, input, submit };
}

test('custom agent settings starts compact with labeled form revealed only on demand', async () => {
  const f = await fixture({ agents: [qwen] });
  assert.equal(f.section.querySelector('form').hidden, true);
  assert.equal(f.section.querySelectorAll('[data-agent-id]').length, 1);
  assert.equal(f.section.querySelector('[data-agent-id] .s2-agent-kind').textContent, 'Терминал');
  assert.equal(f.button('Удалить агента').hidden, true);
  f.button('Добавить агента').click(); await flush();
  assert.equal(f.section.querySelector('form').hidden, false);
  for (const name of ['name', 'bin', 'id', 'resume']) {
    assert.ok(f.section.querySelector(`label[for="${f.field(name).id}"] .s2-field-label`).textContent);
  }
  assert.equal(f.section.querySelector('button[type="submit"]').textContent, 'Добавить агента');
  assert.equal(f.section.querySelector('.s2-agent-advanced').open, false);
  assert.match(f.section.querySelector('.s2-agent-capabilities').textContent, /требуют отдельной интеграции/);
});

test('presets fill editable fields and real form submission creates a safe unique ID', async () => {
  const f = await fixture({ agents: [{ ...qwen, id: 'opencode', bin: 'opencode' }] });
  f.button('Добавить агента').click(); await flush(); f.button('OpenCode').click(); await flush();
  assert.equal(f.field('name').value, 'OpenCode');
  assert.equal(f.field('bin').value, 'opencode');
  assert.equal(f.field('id').value, 'opencode-2');
  await f.submit();
  assert.equal(f.calls.length, 1);
  assert.deepEqual(f.calls[0][1], { id: 'opencode-2', name: 'OpenCode', bin: 'opencode', resume: '', dangerousFlag: '' });
  assert.equal(f.section.querySelector('form').hidden, true);
  assert.equal(f.section.querySelectorAll('[data-agent-id]').length, 2);
});

test('manual duplicate ID never replaces an existing custom agent and opens the invalid field', async () => {
  const f = await fixture({ agents: [qwen] });
  f.button('Добавить агента').click(); await flush();
  f.input('name', 'Other'); f.input('bin', 'other'); f.input('id', 'qwen');
  await f.submit();
  assert.equal(f.calls.length, 0);
  assert.equal(f.section.querySelector('.s2-agent-advanced').open, true);
  assert.equal(f.field('id').getAttribute('aria-invalid'), 'true');
  assert.match(f.section.querySelector('[role="alert"]').textContent, /уже используется/);
  assert.equal(f.section.querySelectorAll('[data-agent-id]').length, 1);
});

test('editing preserves the stable ID and hidden launch flags and retains draft after a failed save', async () => {
  let fail = true;
  const f = await fixture({ agents: [qwen], save: async () => fail ? { ok: false, error: 'Cannot write settings' } : { ok: true } });
  f.button('Изменить').click(); await flush();
  assert.equal(f.field('id').readOnly, true);
  assert.equal(f.button('Удалить агента').hidden, false);
  f.input('name', 'Qwen updated'); f.input('bin', '/new/qwen');
  await f.submit();
  assert.equal(f.section.querySelector('form').hidden, false);
  assert.equal(f.field('name').value, 'Qwen updated');
  assert.equal(f.section.querySelector('[data-agent-id] .dt').textContent, 'Qwen Code');
  assert.match(f.section.querySelector('[role="alert"]').textContent, /Cannot write settings/);
  fail = false; await f.submit();
  assert.equal(f.calls.at(-1)[0].id, 'qwen');
  assert.equal(f.calls.at(-1)[0].dangerousFlag, '--yolo');
  assert.equal(f.section.querySelector('[data-agent-id] .dt').textContent, 'Qwen updated');
});

test('failed deletion does not leak into a later save and cancellation changes nothing', async () => {
  let fail = true;
  const f = await fixture({ agents: [qwen], save: async () => fail ? { ok: false, error: 'Offline' } : { ok: true } });
  f.button('Изменить').click(); await flush(); f.button('Удалить агента').click(); await flush();
  assert.equal(f.section.querySelectorAll('[data-agent-id]').length, 1);
  f.button('Отмена').click(); await flush();
  f.button('Добавить агента').click(); await flush(); f.button('Pi').click(); await flush();
  fail = false; await f.submit();
  assert.deepEqual(f.calls.at(-1).map(agent => agent.id), ['qwen', 'pi']);
  assert.deepEqual(f.calls.at(-1)[0], qwen);
});

test('blank required fields and malformed resume are blocked locally; missing CLI is a visible warning', async () => {
  const f = await fixture({ save: async () => ({ ok: true, missing: ['new-agent'] }) });
  f.button('Добавить агента').click(); await flush();
  await f.submit(); assert.equal(f.field('name').getAttribute('aria-invalid'), 'true');
  f.input('name', 'New'); await f.submit(); assert.equal(f.field('bin').getAttribute('aria-invalid'), 'true');
  f.input('bin', 'new-agent'); f.input('resume', 'new-agent --resume');
  await f.submit(); assert.equal(f.field('resume').getAttribute('aria-invalid'), 'true'); assert.equal(f.calls.length, 0);
  f.input('resume', 'new-agent --resume {sid}'); await f.submit();
  assert.equal(f.section.querySelector('.s2-agent-status').dataset.kind, 'warning');
  assert.match(f.section.querySelector('.s2-agent-status').textContent, /Программа не найдена/);
});

test('repeated submit during save cannot duplicate writes and errors re-enable the form', async () => {
  let resolveSave;
  const f = await fixture({ save: () => new Promise(resolve => { resolveSave = resolve; }) });
  f.button('Добавить агента').click(); await flush(); f.button('Pi').click(); await flush();
  await f.submit(); await f.submit();
  assert.equal(f.calls.length, 1);
  assert.equal(f.field('name').disabled, true);
  resolveSave({ ok: false, error: 'Store unavailable' }); await flush();
  assert.equal(f.field('name').disabled, false);
  assert.equal(f.section.querySelector('form').hidden, false);
});


test('an incomplete registry entry stays editable instead of crashing the entire settings pane', async () => {
  const f = await fixture({ agents: [{ id: 'opencode', name: 'OpenCode' }] });
  const row = f.section.querySelector('[data-agent-id="opencode"]');
  assert.match(row.textContent, /Программа не указана/);
  f.button('Изменить').click(); await flush();
  assert.equal(f.field('name').value, 'OpenCode');
  assert.equal(f.field('bin').value, '');
  f.input('bin', 'opencode'); await f.submit();
  assert.equal(f.calls.at(-1)[0].bin, 'opencode');
});
