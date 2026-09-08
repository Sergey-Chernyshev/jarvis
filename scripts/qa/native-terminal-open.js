// Actual production page in WKWebView, in native-smoke's disposable app/profile.
// Only the terminal/session bridge methods are replaced with synthetic data.
// No account changes, SSH, shell processes, terminal input or clipboard writes.
const probeCalls = [], probeErrors = [], probeNow = Date.now();
const probeSessions = [
  { id:'native-terminal-local', title:'Проверка терминала WebKit', project:'Terminal QA', cwd:'/fixture/native-terminal', agent:'claude', model:'Sonnet', status:'idle', tmuxPane:'%fixture-local', createdAt:probeNow, updatedAt:probeNow },
  { id:'native-fixture:terminal', title:'Проверка удалённого терминала', project:'Terminal QA', cwd:'/fixture/native-terminal', remote:'native-fixture', agent:'codex', model:'GPT-5', status:'working', tmuxPane:'%fixture-remote', createdAt:probeNow - 1000, updatedAt:probeNow },
];
const probeEncode = value => Array.from(new TextEncoder().encode(value));
const probeStreams = new Map();
let probeSerial = 0;
window.addEventListener('error', event => probeErrors.push({type:'error', message:event.message, file:event.filename, line:event.lineno}));
window.addEventListener('unhandledrejection', event => probeErrors.push({type:'rejection', message:String(event.reason?.stack || event.reason)}));
const probeVisible = element => {
  if (!element?.isConnected || element.closest('[hidden]')) return false;
  for (let ancestor = element; ancestor; ancestor = ancestor.parentElement) {
    const style = getComputedStyle(ancestor);
    if (style.display === 'none' || style.visibility === 'hidden') return false;
  }
  const rect = element.getBoundingClientRect();
  return rect.width > 0 && rect.height > 0;
};
const probeClick = element => {
  t.assert(probeVisible(element) && !element.disabled, `Control unavailable: ${element?.getAttribute('aria-label') || element?.id}`);
  element.scrollIntoView({block:'nearest', inline:'nearest'});
  const rect = element.getBoundingClientRect();
  const hit = document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2);
  t.assert(hit === element || element.contains(hit), `Control is covered: ${element.getAttribute('aria-label') || element.id}; hit ${hit?.className}`);
  element.click();
};
const probeSettle = async () => {
  await t.waitFor(() => {
    const running = document.getAnimations().filter(animation => animation.playState === 'running' && Number.isFinite(animation.effect?.getComputedTiming().endTime));
    if (running.length && !document.hasFocus()) {
      running.forEach(animation => animation.finish());
      t.evidence('snapshotPolicy', 'Finite animations finished while occluded for static geometry only; no claim about native animation smoothness.');
      return true;
    }
    return !running.length;
  });
};
const probeSnapshot = async name => {
  await probeSettle();
  t.evidence(name, {
    visibility:{hidden:document.hidden, state:document.visibilityState, focused:document.hasFocus()},
    controls:t.geometry('#panel,#sessionFrame,#chat,#chatlog,.sw-chat-context,.sw-terminal,.tw-head,.tw-viewport,.tw-screen,.xterm,.xterm-screen,.tw-footer'),
    toggle:[...document.querySelectorAll('button')].filter(element => element.getAttribute('aria-label') === 'Терминал').map(element => ({hidden:element.hidden, disabled:element.disabled, expanded:element.getAttribute('aria-expanded'), rect:element.getBoundingClientRect().toJSON()})),
    calls:probeCalls.slice(), errors:probeErrors.slice(),
  });
  await t.screenshot(name);
};
const probeTerminalButton = () => document.querySelector('.sw-chat-context button[aria-label="Терминал"]');
const probeTerminalText = () => window.JarvisTerminal.bufferText(window.__nativeProbeTerminal.buffer.active);
const probeOpenChat = async id => {
  // The main native window is a quick surface: its sidebar is intentionally
  // hidden even at desktop widths. Use the visible recent-chat row there.
  if (!probeVisible(document.querySelector('#sessionSidebar')) && document.documentElement.dataset.view === 'chat') {
    window.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true}));
    await t.waitFor(() => document.documentElement.dataset.view === 'list');
    await probeSettle();
  }
  const row = await t.waitFor(() => [...document.querySelectorAll(`[data-session-id="${id}"]`)].find(probeVisible));
  probeClick(row);
  await t.waitFor(() => document.documentElement.dataset.view === 'chat' && document.querySelector(`#sessionSidebar [data-session-id="${id}"]`)?.getAttribute('aria-current') === 'page');
  await probeSettle();
  await t.waitFor(() => probeVisible(probeTerminalButton()) && !probeTerminalButton().disabled);
};

try {
  await t.step('Production WebKit page loads terminal assets and receives isolated session state', async () => {
    await t.waitFor(() => window.jarvisSessionWorkspace && document.documentElement.dataset.view === 'home');
    // Wait out renderer startup so its initial mode sync cannot reset the
    // navigation between this probe's first two clicks.
    await Promise.all([initialStateReady, initialSettingsReady]);
    t.evidence('assetState', {
      terminal:typeof window.Terminal, search:typeof window.SearchAddon?.SearchAddon,
      fit:typeof window.FitAddon?.FitAddon, controller:typeof window.JarvisTerminal?.create,
      scriptUrls:[...document.scripts].map(script => script.src).filter(src => /terminal|xterm|session-workspace/.test(src)),
    });
    t.assert(window.Terminal && window.SearchAddon && window.FitAddon && window.JarvisTerminal, 'Terminal assets are absent from the production page');
    const VendorTerminal = window.Terminal;
    window.Terminal = class extends VendorTerminal {
      constructor(...args) { super(...args); window.__nativeProbeTerminal = this; }
    };
    Object.assign(window.jarvis, {
      getState:async () => structuredClone(probeSessions),
      machinesList:async () => [{id:'local',name:'Этот компьютер',kind:'local',online:true},{id:'native-fixture',name:'native-fixture',kind:'remote',online:true}],
      agentInstancesList:async () => ({config:{entries:[]},instances:[],health:[]}),
      remotesList:async () => [{name:'native-fixture',connected:true,sources:[]}],
      getHistory:async () => [], projectsList:async () => ({ok:true,projects:[],warnings:[]}),
      getCommands:async () => [], getPrompts:async () => [],
      getSessionUsage:async () => ({tok:0,cost:0,requests:0,source:'native-fixture'}),
      openChat:async id => {
        t.assert(probeSessions.some(session => session.id === id), 'Unexpected non-fixture session');
        probeCalls.push({action:'chat-open',id});
        return {ok:true,project:'Terminal QA',items:[{role:'user',text:'Изолированная проверка кнопки терминала.',ts:probeNow},{role:'assistant',text:'Синтетический вывод; реальные команды не запускаются.',ts:probeNow + 1}],spans:[],cards:{},llm:false};
      },
      closeChat:async () => ({ok:true}),
      focusTerminal:async () => { throw new Error('External terminal must not be opened in this probe'); },
      terminalAction:async (id, action, payload = {}) => {
        probeCalls.push({id,action,streamId:payload.streamId});
        t.assert(probeSessions.some(session => session.id === id), 'Unexpected non-fixture terminal');
        if (action === 'open') {
          const streamId = `native-probe-${++probeSerial}`; probeStreams.set(streamId,id);
          return {ok:true,streamId,cursor:0,cols:100,rows:24,connection:{canInput:true},initial:probeEncode(`SESSION:${id}\r\nПривет 中文 🐈\r\n$ SYNTHETIC-READY\r\n`)};
        }
        if (action === 'poll') {
          await new Promise(resolve => setTimeout(resolve,150));
          return {ok:true,cursor:0,chunks:[],closed:!probeStreams.has(payload.streamId)};
        }
        if (action === 'close') { probeStreams.delete(payload.streamId); return {ok:true}; }
        throw new Error('Probe must remain in reading mode: ' + action);
      },
    });
    // Uses the same registered state-event callback as the daemon, scoped to
    // this isolated native-smoke application, without changing daemon storage.
    await window.__TAURI__.event.emit('state', structuredClone(probeSessions));
    probeClick(document.getElementById('tabSessions'));
    await t.waitFor(() => [...document.querySelectorAll('[data-session-id="native-terminal-local"]')].some(probeVisible));
    t.assert(!probeCalls.some(call => call.action === 'open'), 'Terminal opens before explicit opt-in');
  });

  await t.step('Real bottom Terminal button reveals and connects the local xterm in WKWebView', async () => {
    await probeOpenChat('native-terminal-local');
    await probeSnapshot('terminal-local-before-click');
    probeClick(probeTerminalButton());
    await t.waitFor(() => probeVisible(document.querySelector('.tw-root')) && document.querySelector('.tw-root').dataset.state === 'connected' && probeTerminalText().includes('SYNTHETIC-READY'));
    t.assert(probeTerminalButton().getAttribute('aria-expanded') === 'true', 'Button does not announce expanded state');
    t.assert(probeTerminalText().includes('Привет 中文 🐈'), 'Terminal loses Unicode in WebKit');
    await probeSnapshot('terminal-local-open');
  });

  await t.step('Terminal viewport and final output fit within the actual chat', async () => {
    const rootBox = document.querySelector('.tw-root').getBoundingClientRect();
    const viewportBox = document.querySelector('.tw-viewport').getBoundingClientRect();
    const screenBox = document.querySelector('.xterm-screen').getBoundingClientRect();
    const terminal = window.__nativeProbeTerminal;
    let finalRow = -1;
    for (let row = 0; row < terminal.buffer.active.length; row++) if (terminal.buffer.active.getLine(row)?.translateToString(true).includes('SYNTHETIC-READY')) finalRow = row;
    const rowHeight = screenBox.height / terminal.rows;
    const finalTop = screenBox.top + (finalRow - terminal.buffer.active.viewportY) * rowHeight;
    const finalBottom = finalTop + rowHeight;
    t.evidence('terminalFit',{root:rootBox.toJSON(),viewport:viewportBox.toJSON(),screen:screenBox.toJSON(),rows:terminal.rows,finalRow,finalTop,finalBottom});
    t.assert(rootBox.height >= 200 && viewportBox.height >= 80 && screenBox.width >= 200, 'Terminal or viewport collapsed');
    t.assert(rootBox.bottom <= innerHeight + 1 && rootBox.left >= 0 && rootBox.right <= innerWidth + 1, 'Terminal escapes the viewport');
    t.assert(finalRow >= 0 && finalTop >= viewportBox.top - 1 && finalBottom <= viewportBox.bottom + 1, 'Final terminal output is clipped above or below the viewport');
    t.assert(document.documentElement.scrollWidth <= innerWidth + 1, 'Terminal causes page horizontal overflow');
  });

  await t.step('Search opens and reaches synthetic terminal output in WKWebView', async () => {
    probeClick(document.querySelector('.tw-button[aria-label="Найти"]'));
    const search = document.querySelector('.tw-find input');
    t.assert(probeVisible(search), 'Search is not visible');
    search.value = 'SYNTHETIC-READY'; search.dispatchEvent(new Event('input',{bubbles:true}));
    await t.waitFor(() => document.querySelector('.tw-matches').textContent === '1 из 1');
    t.assert(window.__nativeProbeTerminal.getSelection() === 'SYNTHETIC-READY', 'Search fails to select its match');
    await probeSnapshot('terminal-native-search');
    probeClick(document.querySelector('.tw-button[aria-label="Закрыть поиск"]'));
  });

  await t.step('Bottom toggle closes, then remote fixture independently opens', async () => {
    probeClick(probeTerminalButton());
    await t.waitFor(() => !probeVisible(document.querySelector('.tw-root')));
    await probeOpenChat('native-fixture:terminal');
    t.assert(!probeVisible(document.querySelector('.tw-root')), 'Remote chat inherits another chat\'s open terminal');
    probeClick(probeTerminalButton());
    await t.waitFor(() => probeVisible(document.querySelector('.tw-root')) && document.querySelector('.tw-root').dataset.state === 'connected' && probeTerminalText().includes('SESSION:native-fixture:terminal'));
    t.assert(!probeTerminalText().includes('SESSION:native-terminal-local'), 'Terminal leaks previous session output');
    await probeSnapshot('terminal-remote-open');
  });

  await t.step('No native terminal actions or JavaScript exceptions escaped the fixture', async () => {
    probeClick(probeTerminalButton());
    t.assert(probeCalls.filter(call => call.action === 'open').length === 2, 'Unexpected repeated terminal opens');
    t.assert(probeCalls.every(call => ['chat-open','open','poll','close'].includes(call.action)), 'Probe sent unexpected terminal action');
    t.assert(probeErrors.length === 0, 'WebKit exceptions: ' + JSON.stringify(probeErrors));
    t.evidence('finalCalls',probeCalls); t.evidence('probeErrors',probeErrors);
  });
} catch (error) {
  t.evidence('probeFailure',String(error?.stack || error));
  await probeSnapshot('terminal-native-failure').catch(snapshotError => t.evidence('snapshotFailure',String(snapshotError)));
  throw error;
}
