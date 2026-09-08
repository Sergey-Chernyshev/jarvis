// Real Chromium/xterm, isolated synthetic bridge; no IPC, clipboard, SSH or agents.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const os = require('node:os');
let playwright;
try { playwright = require('playwright'); }
catch { playwright = require(process.env.JARVIS_PLAYWRIGHT_PATH || path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }
const ui = path.resolve(__dirname, '../../ui');
const out = path.resolve(__dirname, '../../docs/qa/assets/terminal-workspace');
const html = `<!doctype html><html lang="ru" data-theme="dark"><meta charset="utf-8"><title>Jarvis terminal · isolated QA</title>
<link rel="stylesheet" href="/vendor/xterm/xterm.css"><link rel="stylesheet" href="/session-workspace.css"><link rel="stylesheet" href="/terminal-workspace.css">
<style>:root {--font:system-ui;--paper:#151920;--paper-2:#202630;--ink:#f2f5fa;--ink-mute:#a6b2c6;--ink-faint:#9aa8bf;--line:#ffffff16;--line-strong:#ffffff30;--accent:#70dfad;--accent-text:#70dfad;--accent-soft:#70dfad18;--success-text:#70dfad;--rose-text:#f29ca8;--warning-text:#edcc8e;--fill-3:#ffffff0c;--surface-2:#303948}
html[data-theme=light] {--paper:#f6f8fb;--paper-2:#fff;--ink:#182234;--ink-mute:#53627a;--ink-faint:#66738a;--line:#18223416;--line-strong:#18223435;--accent:#127454;--accent-text:#127454;--accent-soft:#12745412;--success-text:#127454;--rose-text:#b22948;--warning-text:#916100;--fill-3:#1822340c;--surface-2:#edf1f7}
*{box-sizing:border-box}body{margin:0;padding:24px;background:var(--paper);color:var(--ink);font:14px system-ui}h1{font-size:18px;font-weight:600}#chat{height:calc(100vh - 100px);display:flex;flex-direction:column;position:relative;border:1px solid var(--line-strong);border-radius:12px;overflow:hidden}.fixture-message{flex:1;padding:24px}.tw-root{display:flex;flex-direction:column}button,input{font:inherit}@media(max-width:760px){body{padding:12px}.fixture-message{padding:12px}}</style>
<h1>Jarvis · Терминал проекта</h1><main id="chat"><div class="fixture-message">Проверка длинной истории и выделения<br><small>Изолированная сессия · синтетический вывод</small></div></main>
<script src="/vendor/xterm/xterm.js"></script><script src="/vendor/xterm/addon-search.js"></script><script src="/vendor/xterm/addon-fit.js"></script><script src="/terminal-workspace.js"></script><script>(${fixture.toString()})();</script></html>`;
const server = http.createServer((req, res) => {
  const requested = new URL(req.url, 'http://localhost').pathname;
  if (requested === '/') { res.writeHead(200, {'Content-Type':'text/html'}).end(html); return; }
  const file = path.resolve(ui, '.' + requested);
  if (!file.startsWith(ui + path.sep)) { res.writeHead(403).end(); return; }
  fs.readFile(file, (error, data) => {
    if (error) res.writeHead(404).end();
    else res.writeHead(200, {'Content-Type':file.endsWith('.css') ? 'text/css' : 'text/javascript'}).end(data);
  });
});

function fixture() {
  const calls = [], copies = [], streams = new Map(), toasts = [];
  const encode = text => Array.from(new TextEncoder().encode(text));
  const history = Array.from({length:100000}, (_, i) => `row-${String(i).padStart(6,'0')} | Привет 中文 🐈 é`).join('\r\n') + '\r\n$ готово\r\n';
  let serial = 0, text = history, heldOpen, current, nextInput, heldInput;
  const NativeTerminal = window.Terminal;
  window.Terminal = class extends NativeTerminal { constructor(...args) { super(...args); window.__terminal = this; } };
  const wake = stream => { const pending = stream.pending; stream.pending = null; if (pending) { clearTimeout(pending.timer); pending.resolve(stream.reply || {ok:true,cursor:stream.cursor,chunks:stream.queue.splice(0)}); stream.reply = null; } };
  function open(id, options={}) {
    const streamId = `fixture-${++serial}`, stream = {id, streamId, queue:[],cursor:0}; streams.set(streamId,stream); current = stream;
    const lines=options.historyLines || 2000, initial=lines>=100000 ? text : text.split('\r\n').slice(-lines-32).join('\r\n');
    return {ok:true,streamId,cursor:0,cols:120,rows:32,initial:encode(initial + `SESSION:${id}\r\n`),historyTruncated:lines<100000};
  }
  const bridge = {
    async terminalAction(id, action, payload={}) {
      calls.push({id,action,payload});
      if (action === 'open') {
        if (id === 'slow') return new Promise(resolve => { heldOpen = () => resolve(open(id,payload)); });
        return open(id,payload);
      }
      const stream = streams.get(payload.streamId);
      if (!stream || stream.id !== id) throw new Error(`Unknown stream ${id}/${payload.streamId}`);
      if (action === 'poll') {
        if (stream.queue.length) return {ok:true,cursor:stream.cursor,chunks:stream.queue.splice(0)};
        return new Promise(resolve => { stream.pending = {resolve,timer:setTimeout(() => wake(stream),200)}; });
      }
      if (action === 'close') { stream.reply = {ok:true,closed:true}; wake(stream); streams.delete(payload.streamId); return {ok:true}; }
      if (action === 'input') {
        const next=nextInput;nextInput=null;
        if(next==='hold' || next==='hold-fail')return new Promise(resolve=>{heldInput=()=>resolve(next==='hold-fail' ? {ok:false,error:'QA: доставка ввода не подтверждена'} : {ok:true});});
        if(next==='fail')return {ok:false,error:'QA: доставка ввода не подтверждена'};
        return {ok:true};
      }
      if (action === 'resize') return {ok:true,cols:payload.cols,rows:payload.rows};
      if (action === 'export') return {ok:true,path:'/fixture/Downloads/terminal.txt',truncated:false};
      throw new Error('Unknown terminal action: ' + action);
    },
    async copyText(value) { copies.push(value); return {ok:true}; },
  };
  const view = window.JarvisTerminal.create({bridge,toast:message=>toasts.push(message),openExternal:()=>calls.push({action:'external'})});
  document.querySelector('#chat').append(view.root);
  window.__fixture = {
    calls,copies,toasts,view,
    start:()=>view.setSession('local-fixture',true,'Этот компьютер'),
    emit(value, duplicate=false) { text += value; const chunk={seq:++current.cursor,data:encode(value)}; current.queue.push(chunk); if(duplicate) current.queue.push({...chunk}); wake(current); },
    split(value, at) { text += value; const data=encode(value); current.queue.push({seq:++current.cursor,data:data.slice(0,at)},{seq:++current.cursor,data:data.slice(at)}); wake(current); },
    fail() { current.reply={ok:false,error:'QA: соединение недоступно',needsUpdate:true}; wake(current); },
    releaseOpen() { heldOpen(); heldOpen=null; },
    holdInput() { nextInput='hold'; },
    inputPending:()=>!!heldInput,
    releaseInput() { heldInput();heldInput=null; },
    failInput(hold=false) { nextInput=hold ? 'hold-fail' : 'fail'; },
    tail(id, value, closed) {
      const stream=[...streams.values()].find(item=>item.id===id);
      if(!stream)throw new Error('Missing tail stream '+id);
      text+=value;
      stream.reply={ok:true,cursor:++stream.cursor,chunks:[{seq:stream.cursor,data:encode(value)}],closed,connection:{canInput:false}};
      wake(stream);
    },
    lineIndex(prefix) { const buffer=window.__terminal.buffer.active; for(let i=0;i<buffer.length;i++) if(buffer.getLine(i)?.translateToString(true).startsWith(prefix)) return i; return -1; },
    select(prefix) { const row=this.lineIndex(prefix); if(row<0)throw new Error('Missing line '+prefix); window.__terminal.scrollToLine(row); window.__terminal.selectLines(row,row); return window.__terminal.getSelection(); },
    text:()=>window.JarvisTerminal.bufferText(window.__terminal.buffer.active),
  };
}

(async () => {
  await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
  const browser = await playwright.chromium.launch({channel:'chrome',headless:true});
  const page = await browser.newPage({viewport:{width:1280,height:850},colorScheme:'dark'});
  const errors=[], checks=[], metrics={}; page.on('pageerror',e=>errors.push(e.message)); page.setDefaultTimeout(15000);
  const check=(name,result)=>{assert.ok(result,name);checks.push(name);};
  const waitConnected=()=>page.waitForFunction(()=>document.querySelector('.tw-root').dataset.state==='connected' && window.__fixture.lineIndex('row-099999')>=0,null,{timeout:30000});
  const menu=async label=>{await page.getByLabel('Другие действия терминала',{exact:true}).click();await page.getByRole('button',{name:label,exact:true}).click();};
  fs.mkdirSync(out,{recursive:true});
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    check('no terminal opened while hidden',await page.evaluate(()=>window.__fixture.calls.length===0));
    await page.evaluate(()=>window.__fixture.start()); await waitConnected();
    check('initial attach requests only 2000 history lines',await page.evaluate(()=>window.__fixture.calls.find(c=>c.action==='open').payload.historyLines===2000 && window.__fixture.lineIndex('row-020000')<0));
    check('partial history is explained',await page.locator('.tw-status').innerText().then(value=>value.includes('загрузить больше истории')));
    const started=Date.now();await menu('Загрузить больше истории (до 100 000 строк)');
    await page.waitForFunction(()=>document.querySelector('.tw-root').dataset.state==='connected' && window.__fixture.lineIndex('row-020000')>=0,null,{timeout:30000});metrics.initial100kMs=Date.now()-started;
    check('load-more requests 100000 explicitly',await page.evaluate(()=>window.__fixture.calls.filter(c=>c.action==='open').at(-1).payload.historyLines===100000));
    const counts=await page.evaluate(()=>({lines:window.__terminal.buffer.active.length,dom:document.querySelectorAll('.xterm *').length}));
    check('100k history retained with bounded DOM',counts.lines>=100000 && counts.dom<5000); metrics.bufferLines=counts.lines;metrics.terminalDomNodes=counts.dom;
    metrics.initialGeometry=await page.evaluate(()=>{
      const terminal=window.__terminal, screen=document.querySelector('.xterm-screen').getBoundingClientRect(), viewport=document.querySelector('.tw-viewport').getBoundingClientRect();
      const row=window.__fixture.lineIndex('SESSION:local-fixture')-terminal.buffer.active.viewportY;
      return {screenHeight:screen.height,viewportHeight:viewport.height,promptBottom:screen.y+(row+1)*screen.height/terminal.rows,visibleBottom:viewport.bottom,outerScrollHeight:document.querySelector('.tw-viewport').scrollHeight};
    });
    check('last live prompt is physically visible',metrics.initialGeometry.promptBottom<=metrics.initialGeometry.visibleBottom);
    await page.getByRole('button',{name:'Найти',exact:true}).click();
    await page.getByRole('searchbox',{name:'Найти в выводе терминала'}).fill('row-020000');
    await page.waitForFunction(()=>document.querySelector('.tw-matches').textContent==='1 из 1');
    check('search reaches old history',await page.evaluate(()=>window.__terminal.getSelection()==='row-020000'));
    await page.waitForTimeout(120);
    const matchVisible=await page.evaluate(()=>{const term=window.__terminal,rect=document.querySelector('.xterm-screen').getBoundingClientRect(),view=document.querySelector('.tw-viewport').getBoundingClientRect(),row=window.__fixture.lineIndex('row-020000')-term.buffer.active.viewportY;const top=rect.y+row*rect.height/term.rows;return {top,bottom:top+rect.height/term.rows,viewTop:view.top,viewBottom:view.bottom};});
    check('search match is physically visible '+JSON.stringify(matchVisible),matchVisible.top>=matchVisible.viewTop && matchVisible.bottom<=matchVisible.viewBottom);
    await page.getByRole('searchbox',{name:'Найти в выводе терминала'}).press('Escape');
    const selected=await page.evaluate(()=>window.__fixture.select('row-020000'));
    check('Unicode selection exact',selected==='row-020000 | Привет 中文 🐈 é');
    await page.waitForTimeout(120); // xterm's configured smooth scroll settles.
    const anchor=await page.evaluate(()=>window.__terminal.buffer.active.viewportY);
    await page.evaluate(()=>window.__fixture.emit('LIVE-UNIQUE | новая строка 🐈\r\n',true));
    await page.waitForFunction(()=>window.__fixture.lineIndex('LIVE-UNIQUE')>=0);
    const afterLive=await page.evaluate(()=>({selection:window.__terminal.getSelection(),viewport:window.__terminal.buffer.active.viewportY}));
    check('live output preserves selection and viewport '+JSON.stringify({selected,anchor,afterLive}),afterLive.selection===selected && afterLive.viewport===anchor);
    check('duplicate chunk not rendered twice',await page.evaluate(()=>window.__fixture.text().split('LIVE-UNIQUE').length===2));
    await page.getByRole('button',{name:'Копировать',exact:true}).click();
    check('copy sends exact Unicode to local bridge',await page.evaluate(selected=>window.__fixture.copies.at(-1)===selected,selected));
    await page.evaluate(()=>window.__fixture.view.setSession('local-fixture',true,'Этот компьютер'));
    check('session snapshot does not reset selection',await page.evaluate(selected=>window.__terminal.getSelection()===selected,selected));
    await page.evaluate(()=>window.__fixture.split('SPLIT-UTF8: 🐈 中文 é\r\n',14));
    await page.waitForFunction(()=>window.__fixture.lineIndex('SPLIT-UTF8')>=0);
    check('split UTF-8 preserved',await page.evaluate(()=>window.__fixture.text().includes('SPLIT-UTF8: 🐈 中文 é') && !window.__fixture.text().includes('�')));
    await page.screenshot({path:path.join(out,'history-selected-dark-desktop.png')});
    const drag=await page.evaluate(()=>{window.__terminal.clearSelection();const screen=document.querySelector('.xterm-screen').getBoundingClientRect();return {x:screen.x,y:screen.y,cell:screen.width/window.__terminal.cols,row:screen.height/window.__terminal.rows};});
    await page.mouse.move(drag.x+drag.cell*.1,drag.y+drag.row*.5);await page.mouse.down();await page.mouse.move(drag.x+drag.cell*10.1,drag.y+drag.row*.5,{steps:10});await page.mouse.up();
    check('real mouse drag selects in reading mode',await page.evaluate(()=>window.__terminal.getSelection()==='row-020000'));
    await page.evaluate(()=>window.__fixture.emit('LIVE-MOUSE\r\n'));
    await page.waitForFunction(()=>window.__fixture.lineIndex('LIVE-MOUSE')>=0);
    check('real mouse selection survives live output',await page.evaluate(()=>window.__terminal.getSelection()==='row-020000'));
    await page.mouse.wheel(0,-300);await page.waitForTimeout(120);
    check('real wheel scrolls local history',await page.evaluate(anchor=>window.__terminal.buffer.active.viewportY<anchor,anchor));
    await page.getByRole('button',{name:'К последнему',exact:true}).click();
    await page.locator('.xterm-helper-textarea').focus(); await page.keyboard.type('read-only');
    check('reading mode does not send keyboard input',await page.evaluate(()=>!window.__fixture.calls.some(c=>c.action==='input')));
    await page.setViewportSize({width:1100,height:750}); await page.waitForTimeout(250);
    check('reading mode does not resize remote pane',await page.evaluate(()=>!window.__fixture.calls.some(c=>c.action==='resize')));
    await page.getByRole('button',{name:'Включить ввод',exact:true}).click();
    await page.waitForFunction(()=>window.__fixture.calls.some(c=>c.action==='resize'));
    await page.locator('.xterm-helper-textarea').focus(); await page.keyboard.type('fixture-input');
    await page.waitForFunction(()=>window.__fixture.calls.some(c=>c.action==='input'));
    check('explicit input reaches selected stream',await page.evaluate(()=>window.__fixture.calls.filter(c=>c.action==='input').every(c=>c.id==='local-fixture')));
    const beforeCoalesce=await page.evaluate(()=>window.__fixture.calls.filter(c=>c.action==='input').length);
    await page.evaluate(()=>{window.__fixture.holdInput();window.__terminal.input('HELD-PREFIX');});
    await page.waitForFunction(()=>window.__fixture.inputPending());
    await page.evaluate(()=>{for(const character of '0123456789')window.__terminal.input(character);window.__fixture.releaseInput();});
    await page.waitForFunction(()=>window.__fixture.calls.some(c=>c.action==='input' && new TextDecoder().decode(new Uint8Array(c.payload.data))==='0123456789'));
    const coalesced=await page.evaluate(before=>window.__fixture.calls.filter(c=>c.action==='input').slice(before).map(c=>new TextDecoder().decode(new Uint8Array(c.payload.data))),beforeCoalesce);
    check('rapid raw keys coalesce behind a delayed request in exact order',JSON.stringify(coalesced)===JSON.stringify(['HELD-PREFIX','0123456789']));
    const beforeHeld=await page.evaluate(()=>window.__fixture.calls.filter(c=>c.action==='input').length);
    await page.evaluate(()=>{window.__fixture.holdInput();window.__terminal.input('A'.repeat(20000));window.__terminal.input('OLD-QUEUED');});
    await page.waitForFunction(()=>window.__fixture.inputPending());
    await page.getByRole('button',{name:'Включить ввод',exact:true}).click();
    await page.getByRole('button',{name:'Включить ввод',exact:true}).click();
    await page.evaluate(()=>{window.__fixture.releaseInput();window.__terminal.input('NEW-AFTER');});
    await page.waitForFunction(()=>window.__fixture.calls.some(c=>c.action==='input' && new TextDecoder().decode(new Uint8Array(c.payload.data))==='NEW-AFTER'));
    const heldCalls=await page.evaluate(before=>window.__fixture.calls.filter(c=>c.action==='input').slice(before).map(c=>new TextDecoder().decode(new Uint8Array(c.payload.data))),beforeHeld);
    check('off/on discards old queued input and remaining chunks',heldCalls.length===2 && heldCalls[0]==='A'.repeat(8192) && heldCalls[1]==='NEW-AFTER');
    const beforeFailure=await page.evaluate(()=>window.__fixture.calls.filter(c=>c.action==='input').length);
    await page.evaluate(()=>{window.__fixture.failInput(true);window.__terminal.input('DO-NOT-REPLAY');});
    await page.waitForFunction(()=>window.__fixture.inputPending());
    await page.evaluate(()=>{window.__terminal.input('QUEUED-BEFORE-FAIL');window.__fixture.releaseInput();});
    await page.waitForFunction(()=>document.querySelector('.tw-status').textContent.includes('доставка ввода не подтверждена'));
    check('failed input returns to reading with explicit retry available',await page.getByRole('button',{name:'Включить ввод',exact:true}).evaluate(node=>!node.disabled && node.getAttribute('aria-pressed')==='false'));
    await page.getByRole('button',{name:'Включить ввод',exact:true}).click();
    await page.evaluate(()=>window.__terminal.input('USER-RETRY'));
    await page.waitForFunction(()=>window.__fixture.calls.some(c=>c.action==='input' && new TextDecoder().decode(new Uint8Array(c.payload.data))==='USER-RETRY'));
    const failedCalls=await page.evaluate(before=>window.__fixture.calls.filter(c=>c.action==='input').slice(before).map(c=>new TextDecoder().decode(new Uint8Array(c.payload.data))),beforeFailure);
    check('failed input is not replayed after explicit re-enable',JSON.stringify(failedCalls)===JSON.stringify(['DO-NOT-REPLAY','USER-RETRY']));
    const paste='строка 🐈 中文 é\r\n'.repeat(4000);
    const beforePaste=await page.evaluate(()=>window.__fixture.calls.filter(c=>c.action==='input').length);
    await page.evaluate(value=>{const data=new DataTransfer();data.setData('text/plain',value);document.querySelector('.xterm-helper-textarea').dispatchEvent(new ClipboardEvent('paste',{bubbles:true,cancelable:true,clipboardData:data}));},paste);
    await page.waitForFunction(before=>window.__fixture.calls.filter(c=>c.action==='input').length>before,beforePaste);
    const pasted=await page.evaluate(before=>window.__fixture.calls.filter(c=>c.action==='input').slice(before).map(c=>({paste:c.payload.paste,text:new TextDecoder().decode(new Uint8Array(c.payload.data))})),beforePaste);
    check('multiline Unicode paste is one exact paste payload',pasted.length===1 && pasted[0].paste===true && pasted[0].text===paste);
    await page.getByRole('button',{name:'Развернуть',exact:true}).click();
    await page.waitForTimeout(250);
    check('expanded terminal fits its chat container',await page.locator('.tw-root').evaluate(node=>{const a=node.getBoundingClientRect(),b=node.parentElement.getBoundingClientRect();return Math.abs(a.height-b.height)<4 && Math.abs(a.width-b.width)<4;}));
    await page.evaluate(()=>window.__fixture.select('row-030000'));
    await page.evaluate(()=>window.__fixture.fail());
    await page.waitForFunction(()=>document.querySelector('.tw-root').dataset.state==='error');
    check('offline output remains selectable and input disabled',await page.evaluate(()=>window.__terminal.hasSelection() && document.querySelector('.tw-button[aria-label="Включить ввод"]').disabled));
    await menu('Переподключиться'); await waitConnected();
    check('reconnect restores recent output without replaying input',await page.evaluate(()=>window.__fixture.lineIndex('LIVE-UNIQUE')>=0 && document.querySelector('.tw-button[aria-label="Включить ввод"]').getAttribute('aria-pressed')==='false'));
    await page.evaluate(()=>{window.__fixture.view.setSession('slow',true,'Slow fixture');window.__fixture.view.setSession('remote-fixture',true,'build-box');});
    await waitConnected(); await page.evaluate(()=>window.__fixture.releaseOpen());
    await page.waitForTimeout(250);
    check('late open cannot replace the new session',await page.evaluate(()=>window.__fixture.text().includes('SESSION:remote-fixture') && !window.__fixture.text().includes('SESSION:slow') && window.__fixture.calls.some(c=>c.id==='slow'&&c.action==='close')));
    check('switching session resets expanded control state',await page.getByRole('button',{name:'Развернуть',exact:true}).evaluate(node=>node.getAttribute('aria-pressed')!=='true' && node.querySelector('span').textContent==='Развернуть' && !node.closest('.tw-root').classList.contains('tw-expanded')));
    await page.setViewportSize({width:600,height:700});
    await page.evaluate(()=>document.documentElement.dataset.theme='light');
    await page.getByRole('button',{name:'Найти',exact:true}).click();
    await page.getByRole('searchbox',{name:'Найти в выводе терминала'}).fill('row-099900');
    await page.waitForFunction(()=>document.querySelector('.tw-matches').textContent==='1 из 1');
    await page.screenshot({path:path.join(out,'search-light-narrow.png')});
    check('narrow layout has no page horizontal overflow',await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth));
    await page.getByRole('button',{name:'Закрыть поиск',exact:true}).click();
    await page.getByRole('button',{name:'Включить ввод',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('.tw-button[aria-label="Включить ввод"]').getAttribute('aria-pressed')==='true');
    const beforeTail=await page.evaluate(()=>window.__fixture.calls.filter(c=>c.action==='input').length);
    await page.evaluate(()=>window.__fixture.tail('remote-fixture','TAIL-PAGE-ONE | готово 🐈\r\n',false));
    await page.waitForFunction(()=>window.__fixture.lineIndex('TAIL-PAGE-ONE')>=0);
    check('disappeared session disables input while tail is still open',await page.getByRole('button',{name:'Включить ввод',exact:true}).evaluate(node=>node.disabled && node.getAttribute('aria-pressed')==='false'));
    await page.evaluate(()=>{document.querySelector('.tw-button[aria-label="Включить ввод"]').click();window.__terminal.input('TAIL-MUST-NOT-SEND');});
    await page.evaluate(()=>window.__fixture.tail('remote-fixture','TAIL-PAGE-TWO | финал 中文\r\n',true));
    await page.waitForFunction(()=>window.__fixture.lineIndex('TAIL-PAGE-TWO')>=0);
    check('all tail pages render before final close',await page.evaluate(()=>{const output=window.__fixture.text();return output.includes('TAIL-PAGE-ONE | готово 🐈\nTAIL-PAGE-TWO | финал 中文') && document.querySelector('.tw-root').dataset.state==='closed';}));
    check('tail-only connection cannot re-enable or send input',await page.evaluate(before=>window.__fixture.calls.filter(c=>c.action==='input').length===before && document.querySelector('.tw-button[aria-label="Включить ввод"]').disabled,beforeTail));
    check('no browser exceptions',errors.length===0);
    fs.writeFileSync(path.join(out,'results.json'),JSON.stringify({checks,metrics,errors},null,2)+'\n');
    const staleFailure=path.join(out,'failure.png');if(fs.existsSync(staleFailure))fs.unlinkSync(staleFailure);
    console.log(JSON.stringify({passed:checks.length,metrics,errors},null,2));
  } catch(error) {await page.screenshot({path:path.join(out,'failure.png')});throw error;}
  finally {await page.evaluate(()=>window.__fixture?.view.dispose()).catch(()=>{});await browser.close();server.close();}
})().catch(error=>{console.error(error);process.exitCode=1;server.close();});
