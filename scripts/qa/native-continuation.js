// Run only with scripts/native-smoke.mjs and its disposable debug app/profile.
// Exercises production WKWebView navigation, rendering and event subscribers.
// After startup every Jarvis bridge method is a fixture or fails closed: no
// real agent, account, terminal input, clipboard, install or remote operation.
const continuationCalls = [], continuationErrors = [], continuationBlocked = [];
const continuationNow = Date.now();
const continuationSource = {
  id: 'fixture-external-codex', title: 'Внешний чат для продолжения',
  project: 'Continuation QA', cwd: '/fixture/native-continuation', agent: 'codex',
  instanceId: 'fixture-personal', instanceLabel: 'Fixture Personal', model: 'gpt-5',
  status: 'idle', controlMode: 'external', tmuxPane: null,
  createdAt: continuationNow, updatedAt: continuationNow,
};
const continuationChild = {
  ...continuationSource, id: 'fixture-managed-child', title: 'Продолжение в Jarvis',
  controlMode: 'managed', tmuxPane: '%fixture-child',
  createdAt: continuationNow + 1, updatedAt: continuationNow + 1,
};
const continuationOriginal = JSON.stringify(continuationSource);
let continuationSessions = [structuredClone(continuationSource)], continuationSerial = 0;
const continuationStreams = new Map(), continuationTerminals = [];
const continuationEncode = text => Array.from(new TextEncoder().encode(text));
window.addEventListener('error', event => continuationErrors.push({ type: 'error', message: event.message, file: event.filename, line: event.lineno }));
window.addEventListener('unhandledrejection', event => continuationErrors.push({ type: 'rejection', message: String(event.reason?.stack || event.reason) }));

const continuationVisible = element => {
  if (!element?.isConnected || element.closest('[hidden]')) return false;
  for (let ancestor = element; ancestor; ancestor = ancestor.parentElement) {
    const style = getComputedStyle(ancestor);
    if (style.display === 'none' || style.visibility === 'hidden') return false;
  }
  const rect = element.getBoundingClientRect();
  return rect.width > 0 && rect.height > 0;
};
const continuationClick = element => {
  t.assert(continuationVisible(element) && !element.disabled, `Control unavailable: ${element?.getAttribute('aria-label') || element?.id}`);
  element.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  const rect = element.getBoundingClientRect();
  const hit = document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2);
  t.assert(hit === element || element.contains(hit), `Control is covered: ${element.getAttribute('aria-label') || element.id}; hit ${hit?.className}`);
  element.click();
};
const continuationSettle = async () => {
  await t.waitFor(() => {
    const animations = document.getAnimations().filter(animation => animation.playState === 'running' && Number.isFinite(animation.effect?.getComputedTiming().endTime));
    if (animations.length && !document.hasFocus()) {
      animations.forEach(animation => animation.finish());
      t.evidence('snapshotPolicy', 'Finite animations finished while occluded for static geometry only; no native motion claim.');
      return true;
    }
    return animations.length === 0;
  });
};
const continuationToggle = () => document.querySelector('.sw-chat-context button[aria-label="Терминал"]');
const continuationTerminal = () => continuationTerminals.at(-1);
const continuationText = () => {
  const terminal = continuationTerminal();
  return terminal ? window.JarvisTerminal.bufferText(terminal.buffer.active) : '';
};
const continuationSnapshot = async name => {
  await continuationSettle();
  t.evidence(name, {
    route: document.documentElement.dataset.view,
    activeSession: chatSessionId,
    geometry: t.geometry('#panel,#sessionFrame,#chat,#chatlog,.chatinput,.sw-capability,.sw-chat-context,.sw-continue,.sw-continue-status,.tw-root,.tw-viewport,.xterm-screen'),
    calls: continuationCalls.slice(), errors: continuationErrors.slice(), blocked: continuationBlocked.slice(),
  });
  if (t.screenshot) await t.screenshot(name);
};
const continuationAssertTerminalFits = marker => {
  const root = document.querySelector('.tw-root'), viewport = document.querySelector('.tw-viewport');
  const terminal = continuationTerminal(), screen = document.querySelector('.xterm-screen');
  t.assert(continuationVisible(root) && continuationVisible(viewport) && !!terminal, 'Terminal is not visible');
  const rootRect = root.getBoundingClientRect(), viewportRect = viewport.getBoundingClientRect(), screenRect = screen.getBoundingClientRect();
  let markerRow = -1;
  for (let row = 0; row < terminal.buffer.active.length; row++) {
    if (terminal.buffer.active.getLine(row)?.translateToString(true).includes(marker)) markerRow = row;
  }
  const rowHeight = screenRect.height / terminal.rows;
  const markerTop = screenRect.top + (markerRow - terminal.buffer.active.viewportY) * rowHeight;
  const markerBottom = markerTop + rowHeight;
  t.evidence(`terminal-fit-${marker}`, { root: rootRect.toJSON(), viewport: viewportRect.toJSON(), markerRow, markerTop, markerBottom });
  t.assert(rootRect.height >= 160 && viewportRect.height >= 60, 'Terminal viewport collapsed');
  t.assert(rootRect.left >= -1 && rootRect.right <= innerWidth + 1 && rootRect.bottom <= innerHeight + 1, 'Terminal escapes the window');
  t.assert(markerRow >= 0 && markerTop >= viewportRect.top - 1 && markerBottom <= viewportRect.bottom + 1, 'Terminal text is clipped above or below the viewport');
  t.assert(document.documentElement.scrollWidth <= innerWidth + 1, 'Continuation causes horizontal document overflow');
};

try {
  await t.step('Isolated WebKit page receives one external chat and a strict synthetic bridge', async () => {
    await t.waitFor(() => window.jarvisSessionWorkspace && document.documentElement.dataset.view === 'home');
    await Promise.all([initialStateReady, initialSettingsReady]);
    t.assert(window.Terminal && window.JarvisTerminal, 'Production terminal assets are missing');
    const VendorTerminal = window.Terminal;
    window.Terminal = class extends VendorTerminal {
      constructor(...args) { super(...args); continuationTerminals.push(this); }
    };
    // Preserve the object captured by production modules, replacing every
    // function so an accidental unsupported operation cannot reach native IPC.
    for (const name of Object.keys(window.jarvis)) {
      if (typeof window.jarvis[name] !== 'function') continue;
      window.jarvis[name] = async () => {
        continuationBlocked.push(name);
        throw new Error(`Unexpected bridge operation in continuation fixture: ${name}`);
      };
    }
    Object.assign(window.jarvis, {
      getState: async () => structuredClone(continuationSessions),
      getSettings: async () => ({ mode: 'window', theme: 'dark', projects: [], voice: { mute: true }, wake: { enabled: false } }),
      getUsage: async () => ({ total: { api: 0, plan: 0 }, official: null }),
      winIsFullscreen: async () => false,
      machinesList: async () => [{ id: 'local', name: 'Этот компьютер', kind: 'local', online: true }],
      agentInstancesList: async () => ({
        config: { entries: [] }, health: [], defaultCodexInstance: 'fixture-personal',
        instances: [{ id: 'fixture-personal', label: 'Fixture Personal', models: [{ value: 'gpt-5', label: 'GPT-5' }] }],
      }),
      agentsList: async () => ({ ok: true, agents: [] }),
      remotesList: async () => [], getHistory: async () => [],
      projectsList: async () => ({ ok: true, projects: [], warnings: [] }),
      getCommands: async () => [], getPrompts: async () => [],
      getSessionUsage: async id => {
        t.assert(continuationSessions.some(session => session.id === id), 'Usage requested for a non-fixture session');
        return { tok: 0, cost: 0, requests: 0, source: 'native-fixture', instanceLabel: 'Fixture Personal' };
      },
      openChat: async id => {
        t.assert(continuationSessions.some(session => session.id === id), 'Chat requested for a non-fixture session');
        continuationCalls.push({ action: 'chat-open', id });
        return { ok: true, project: 'Continuation QA', items: [
          { role: 'user', text: 'Изолированная история внешнего чата.', ts: continuationNow },
          { role: 'assistant', text: id === continuationSource.id ? 'EXTERNAL-HISTORY: исходный чат доступен только для чтения.' : 'MANAGED-HISTORY: отдельное продолжение готово.', ts: continuationNow + 1 },
        ], spans: [], cards: {}, llm: false };
      },
      closeChat: async () => ({ ok: true }),
      continueSession: async id => {
        t.assert(id === continuationSource.id, 'Continuation requested for the wrong source');
        continuationCalls.push({ action: 'continue', id });
        return { ok: true, launchId: 'fixture-launch', terminalId: 'fixture-launch' };
      },
      terminalAction: async (id, action, payload = {}) => {
        continuationCalls.push({ action, id, streamId: payload.streamId });
        t.assert(['fixture-launch', continuationChild.id].includes(id), 'Terminal must never target the external source or a real session');
        if (action === 'open') {
          const streamId = `continuation-stream-${++continuationSerial}`;
          continuationStreams.set(streamId, id);
          const marker = id === 'fixture-launch' ? 'STARTUP-READY' : 'MANAGED-READY';
          return { ok: true, streamId, cursor: 0, cols: 100, rows: 24, connection: { canInput: true }, initial: continuationEncode(`SESSION:${id}\r\nТестовый терминал — реальные команды не запускаются.\r\n$ ${marker}\r\n`) };
        }
        t.assert(continuationStreams.get(payload.streamId) === id, 'Terminal stream belongs to another session');
        if (action === 'poll') {
          await new Promise(resolve => setTimeout(resolve, 150));
          return { ok: true, cursor: 0, chunks: [], closed: !continuationStreams.has(payload.streamId) };
        }
        if (action === 'close') { continuationStreams.delete(payload.streamId); return { ok: true }; }
        throw new Error(`Fixture must not send terminal input or commands: ${action}`);
      },
      reportError: async (place, message) => { continuationErrors.push({ type: 'reported', place, message: String(message) }); },
    });
    await window.__TAURI__.event.emit('state', structuredClone(continuationSessions));
    continuationClick(document.getElementById('tabSessions'));
    await continuationSettle();
    const row = await t.waitFor(() => [...document.querySelectorAll(`[data-session-id="${continuationSource.id}"]`)].find(continuationVisible));
    continuationClick(row);
    await t.waitFor(() => document.documentElement.dataset.view === 'chat' && chatSessionId === continuationSource.id && continuationVisible(document.querySelector('.sw-continue')));
    t.assert(!continuationVisible(document.querySelector('.chatinput')), 'External chat exposes an unusable composer');
    t.assert(!continuationVisible(continuationToggle()), 'External source exposes a terminal before continuation');
    t.assert(!continuationCalls.some(call => call.action === 'open'), 'Terminal opened without explicit continuation');
    await continuationSnapshot('continuation-external-before');
  });

  await t.step('Continue exposes the launch terminal while the source composer stays hidden', async () => {
    continuationClick(document.querySelector('.sw-continue'));
    await t.waitFor(() => continuationVisible(continuationToggle()) && !continuationToggle().disabled && continuationToggle().textContent.includes('Терминал запуска'));
    t.assert(!continuationVisible(document.querySelector('.chatinput')), 'Startup exposes the external source composer');
    t.assert(!continuationVisible(document.querySelector('.tw-root')), 'Startup terminal expanded without its explicit toggle');
    continuationClick(continuationToggle());
    await t.waitFor(() => continuationVisible(document.querySelector('.tw-root')) && document.querySelector('.tw-root').dataset.state === 'connected' && continuationText().includes('STARTUP-READY'));
    t.assert(chatSessionId === continuationSource.id, 'Continuation switched away before its child exists');
    t.assert(!continuationVisible(document.querySelector('.chatinput')), 'Startup exposes the external source composer');
    t.assert(continuationCalls.filter(call => call.action === 'continue').length === 1, 'Continuation launched more than once');
    t.assert(continuationCalls.some(call => call.action === 'open' && call.id === 'fixture-launch'), 'Launch terminal ID was not used');
    await continuationSettle(); continuationAssertTerminalFits('STARTUP-READY');
    await continuationSnapshot('continuation-startup-terminal');
  });

  await t.step('Ready event waits for the exact child state, then opens its writable chat', async () => {
    await window.__TAURI__.event.emit('session:launch-task', { launchId: 'fixture-launch', status: 'ready', sessionId: continuationChild.id });
    await t.waitFor(() => document.querySelector('.sw-continue')?.textContent.includes('Открыть продолжение'));
    t.assert(!continuationCalls.some(call => call.action === 'chat-open' && call.id === continuationChild.id), 'Ready event opened a child absent from state');
    continuationSessions = [structuredClone(continuationSource), structuredClone(continuationChild)];
    await window.__TAURI__.event.emit('state', structuredClone(continuationSessions));
    await t.waitFor(() => chatSessionId === continuationChild.id && continuationVisible(document.querySelector('.chatinput')) && !document.querySelector('#reply').disabled);
    t.assert(document.querySelector('#chat').dataset.capability === 'ready', 'Child did not become managed');
    t.assert(continuationVisible(continuationToggle()) && !continuationToggle().disabled, 'Managed child has no available terminal');
    const reply = document.querySelector('#reply'); reply.value = 'Проверка черновика без отправки';
    reply.dispatchEvent(new Event('input', { bubbles: true }));
    t.assert(!document.querySelector('#chatSend').disabled, 'Managed composer cannot prepare a message');
    reply.value = ''; reply.dispatchEvent(new Event('input', { bubbles: true }));
    if (continuationToggle().getAttribute('aria-expanded') !== 'true') continuationClick(continuationToggle());
    await t.waitFor(() => continuationVisible(document.querySelector('.tw-root')) && document.querySelector('.tw-root').dataset.state === 'connected' && continuationText().includes('MANAGED-READY'));
    t.assert(!continuationText().includes('STARTUP-READY'), 'Child terminal leaks the startup buffer');
    await continuationSettle(); continuationAssertTerminalFits('MANAGED-READY');
    await continuationSnapshot('continuation-managed-ready');
  });

  await t.step('External source remains separate and no real bridge operation escaped the fixture', async () => {
    const original = state.find(session => session.id === continuationSource.id);
    t.assert(JSON.stringify(original) === continuationOriginal, 'Continuation mutated the original external session');
    t.assert(window.jarvisSessionWorkspace.capabilities(original).readOnly, 'Original source became writable');
    t.assert(state.some(session => session.id === continuationChild.id && session.tmuxPane === '%fixture-child'), 'Managed child is missing from state');
    t.assert(continuationCalls.filter(call => call.action === 'continue').length === 1, 'Duplicate continuation request');
    t.assert(continuationCalls.every(call => ['chat-open', 'continue', 'open', 'poll', 'close'].includes(call.action)), 'Unexpected terminal action');
    t.assert(!continuationCalls.some(call => ['open', 'poll', 'close'].includes(call.action) && call.id === continuationSource.id), 'Terminal attached to the external source');
    if (continuationToggle().getAttribute('aria-expanded') === 'true') continuationClick(continuationToggle());
    await t.waitFor(() => continuationStreams.size === 0);
    t.assert(continuationBlocked.length === 0, 'Unexpected bridge calls: ' + JSON.stringify(continuationBlocked));
    t.assert(continuationErrors.length === 0, 'WebKit errors: ' + JSON.stringify(continuationErrors));
    t.evidence('finalCalls', continuationCalls);
    t.evidence('originalSource', original);
    t.evidence('blockedBridgeCalls', continuationBlocked);
  });
} catch (error) {
  t.evidence('continuationFailure', String(error?.stack || error));
  await continuationSnapshot('continuation-native-failure').catch(snapshotError => t.evidence('snapshotFailure', String(snapshotError)));
  throw error;
}
