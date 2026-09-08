// Focused real Chromium regression using the existing isolated terminal harness.
// No native IPC, remote machines, accounts or terminal commands.
const nativeRequire = require;
const source = require('node:fs').readFileSync(require('node:path').join(__dirname,'terminal-workspace.cjs'),'utf8');
const prefix = source.slice(0,source.indexOf('\n(async () => {'));
const run = async () => {
  await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
  const browser = await playwright.chromium.launch({channel:'chrome',headless:true});
  const page = await browser.newPage({viewport:{width:1000,height:740}}), results = [], errors = [];
  page.on('pageerror',error=>errors.push(error.message));
  const check = (name,ok) => {assert.ok(ok,name);results.push(name);};
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    await page.evaluate(() => {
      window.__fixture.view.dispose();document.querySelector('.tw-root').remove();
      const encode = value=>Array.from(new TextEncoder().encode(value));
      let queue=[],cursor=0;
      const bridge={terminalAction:async(id,action,payload)=>{
        if(action==='open')return {ok:true,streamId:'short-fixture',cursor:0,cols:100,rows:24,initial:encode('ONE\r\nTWO\r\n$ SHORT-READY\r\n')};
        if(action==='poll'){await new Promise(resolve=>setTimeout(resolve,100));return {ok:true,cursor,chunks:queue.splice(0)};}
        if(action==='close')return {ok:true};
        throw new Error('Unexpected terminal action '+action);
      }};
      const view=window.JarvisTerminal.create({bridge});document.querySelector('#chat').append(view.root);
      window.__followProbe={view,emit(value){queue.push({seq:++cursor,data:encode(value)});}};
      view.setSession('short-fixture',true,'Изолированная проверка');
    });
    await page.waitForFunction(()=>document.querySelector('.tw-root').dataset.state==='connected');
    const geometry = async prefix => page.evaluate(prefix => {
      const terminal=window.__terminal,buffer=terminal.buffer.active,screen=document.querySelector('.xterm-screen').getBoundingClientRect(),viewport=document.querySelector('.tw-viewport').getBoundingClientRect();
      let row=buffer.baseY+buffer.cursorY;
      if(prefix) for(let i=0;i<buffer.length;i++)if(buffer.getLine(i)?.translateToString(true).includes(prefix))row=i;
      const top=screen.top+(row-buffer.viewportY)*screen.height/terminal.rows,bottom=top+screen.height/terminal.rows;
      return {top,bottom,viewTop:viewport.top,viewBottom:viewport.bottom,scrollTop:document.querySelector('.tw-viewport').scrollTop,viewportY:buffer.viewportY,baseY:buffer.baseY,cursorY:buffer.cursorY};
    },prefix);
    const visible = row => row.top>=row.viewTop-1 && row.bottom<=row.viewBottom+1;
    for(const zoom of [1,.85,1.15]) {
      await page.evaluate(zoom=>document.documentElement.style.zoom=String(zoom),zoom);
      await page.waitForTimeout(150);
      check(`short output visible at zoom ${zoom}`,visible(await geometry('SHORT-READY')));
      check(`cursor visible at zoom ${zoom}`,visible(await geometry()));
    }
    await page.evaluate(()=>{document.documentElement.style.zoom='1';window.__followProbe.emit(Array.from({length:200},(_,i)=>`LONG-${i}`).join('\r\n')+'\r\n$ LONG-READY\r\n');});
    await page.waitForFunction(()=>window.JarvisTerminal.bufferText(window.__terminal.buffer.active).includes('LONG-READY'));
    await page.waitForTimeout(150);check('long output cursor stays visible',visible(await geometry()));
    await page.evaluate(()=>{window.__terminal.clearSelection();window.__terminal.scrollToBottom();});
    await page.locator('.tw-viewport').hover();await page.mouse.wheel(0,-500);await page.waitForTimeout(160);
    check('wheel enters history',(await geometry()).viewportY<(await geometry()).baseY);
    for(let i=0;i<12;i++){await page.mouse.wheel(0,900);await page.waitForTimeout(120);}
    const live=await geometry();check('wheel returns to live buffer',live.viewportY===live.baseY);
    await page.evaluate(()=>window.__followProbe.emit('AFTER-WHEEL\r\n'));await page.waitForTimeout(300);
    check('returning by wheel resumes live follow',visible(await geometry('AFTER-WHEEL')) && visible(await geometry()));
    await page.getByRole('button',{name:'Найти',exact:true}).click();
    await page.getByRole('searchbox',{name:'Найти в выводе терминала'}).fill('LONG-10');
    await page.waitForFunction(()=>window.__terminal.hasSelection());
    const selected=await page.evaluate(()=>window.__terminal.getSelection());
    await page.evaluate(()=>window.__followProbe.emit('SELECTION-MUST-STAY\r\n'));await page.waitForTimeout(300);
    check('search selection survives incoming output',await page.evaluate(value=>window.__terminal.getSelection()===value,selected));
    for(const zoom of [.85,1.15]) {
      await page.evaluate(zoom=>document.documentElement.style.zoom=String(zoom),zoom);
      await page.getByRole('searchbox',{name:'Найти в выводе терминала'}).fill('LONG-20');await page.waitForTimeout(150);
      const selectedRow=await page.evaluate(()=>window.__terminal.getSelectionPosition().start.y);
      const selectionGeometry=await page.evaluate(row=>{const term=window.__terminal,a=document.querySelector('.xterm-screen').getBoundingClientRect(),b=document.querySelector('.tw-viewport').getBoundingClientRect(),top=a.top+(row-term.buffer.active.viewportY)*a.height/term.rows;return {top,bottom:top+a.height/term.rows,viewTop:b.top,viewBottom:b.bottom};},selectedRow);
      check(`search selection visible at zoom ${zoom}`,visible(selectionGeometry));
    }
    check('no JavaScript exceptions',errors.length===0);
    const followOut = path.resolve(__dirname,'../../docs/qa/assets/terminal-native-open');
    fs.mkdirSync(followOut,{recursive:true});
    fs.writeFileSync(path.join(followOut,'follow-browser.json'),JSON.stringify({passed:results.length,results,errors},null,2)+'\n');
    console.log(JSON.stringify({passed:results.length,results,errors},null,2));
  } finally {await page.evaluate(()=>window.__followProbe?.view.dispose()).catch(()=>{});await browser.close();server.close();}
};
new Function('require','__dirname',prefix+'\n('+run.toString()+')().catch(error=>{console.error(error);process.exitCode=1;server.close();});')(nativeRequire,__dirname);
