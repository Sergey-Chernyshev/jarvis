// Production chat UI in Chromium, synthetic sessions and bridge only.
const fs=require('node:fs'),path=require('node:path'),http=require('node:http'),os=require('node:os'),vm=require('node:vm'),assert=require('node:assert/strict');
let playwright; try { playwright=require('playwright'); } catch { playwright=require(path.join(os.homedir(),'.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }
const source=fs.readFileSync(path.join(__dirname,'session-workspace.cjs'),'utf8'),context={};
vm.runInNewContext(source.slice(source.indexOf('function bridgeFixture()'),source.indexOf('\nconst records ='))+'\nthis.fixtureSource=bridgeFixture.toString();',context);
function capabilityFixture() {
  const f=window.__sessionFixture, calls=[];let held=null;
  const overrides={
    agentInstancesList:async()=>({defaultCodexInstance:'personal',instances:[{id:'personal',label:'Personal',agent:'codex',enabled:true,models:[{value:'gpt-6-astra',label:'GPT-6 Astra'},{value:'gpt-5.6-sol',label:'GPT-5.6 Sol'}]}]}),
    remotesList:async()=>[{name:'build-box',connected:true,sources:[{id:'vm-personal',label:'VM Personal',agent:'codex'}]}],
    setModel:async(id,model)=>{
      calls.push({name:'setModel',id,model});
      if(window.__capQA.rejectModel){window.__capQA.rejectModel=false;return {ok:false,error:'Модель недоступна для профиля'};}
      if(window.__capQA.holdModel) await new Promise(resolve=>held=resolve);
      f.emitState(f.sessions().map(s=>s.id===id?{...s,model}:s));return {ok:true,model};
    },
    setEffort:async(id,effort)=>{calls.push({name:'setEffort',id,effort});return{ok:true};},
    launchSession:async(...args)=>{calls.push({name:'launchSession',args});return{ok:true,launchId:'fixture-launch',cwd:args[0]};},
  };
  const original=window.jarvis;window.jarvis=new Proxy(original,{get:(target,name)=>overrides[name]||target[name]});
  window.__capQA={calls,rejectModel:false,holdModel:false,resolve:()=>{held?.();held=null;},set(patch){f.emitState(f.sessions().map(s=>s.id==='local-chat'?{...s,...patch}:s));}};
}
const root=path.resolve(__dirname,'../../ui'),out=path.resolve(__dirname,'../../docs/qa/assets/chat-capabilities');
const server=http.createServer((req,res)=>{const file=path.resolve(root,'.'+new URL(req.url,'http://localhost').pathname);if(!file.startsWith(root+path.sep)){res.writeHead(403).end();return;}fs.readFile(file,(e,b)=>{if(e)res.writeHead(404).end();else res.writeHead(200,{'Content-Type':({'.html':'text/html','.js':'text/javascript','.css':'text/css','.svg':'image/svg+xml'})[path.extname(file)]||'application/octet-stream'}).end(b);});});
(async()=>{fs.mkdirSync(out,{recursive:true});await new Promise(r=>server.listen(0,'127.0.0.1',r));const browser=await playwright.chromium.launch({channel:'chrome',headless:true});const page=await browser.newPage({viewport:{width:1180,height:820},colorScheme:'dark',reducedMotion:'reduce'});const errors=[],checks=[];page.on('pageerror',e=>errors.push(e.message));page.setDefaultTimeout(8000);
try{
 await page.route('**/bridge.js',route=>route.fulfill({contentType:'text/javascript',body:`(${context.fixtureSource})();(${capabilityFixture.toString()})();`}));
 await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);await page.locator('#tabSessions').click();
 await page.locator('#sessionSidebar [data-session-id="local-chat"]').click();
 const model=page.getByRole('combobox',{name:'Модель текущего чата',exact:true});
 await model.waitFor();assert.equal(await model.inputValue(),'sonnet');
 await model.selectOption('opus');await page.waitForFunction(()=>window.__capQA.calls.some(c=>c.name==='setModel'&&c.model==='opus'));
 await page.locator('.sw-control-status').filter({hasText:'Настройка применена'}).waitFor();assert.equal(await model.inputValue(),'opus');checks.push('managed model selector applies exact model and displays confirmation');
 await page.evaluate(()=>window.__capQA.rejectModel=true);await model.selectOption('sonnet');await page.locator('.sw-control-status').filter({hasText:'Модель недоступна'}).waitFor();assert.equal(await model.inputValue(),'opus');checks.push('failed model selection restores actual model');
 await page.evaluate(()=>window.__capQA.holdModel=true);await model.selectOption('sonnet');await page.waitForFunction(()=>document.querySelector('.sw-control-status').textContent.includes('Применяем'));
 await page.evaluate(()=>window.__capQA.set({detail:'Same-session snapshot during change'}));assert.equal(await model.isDisabled(),true);await page.evaluate(()=>{window.__capQA.holdModel=false;window.__capQA.resolve();});await page.waitForFunction(()=>!document.querySelector('[aria-label="Модель текущего чата"]').disabled);checks.push('state snapshots cannot reopen an in-flight model change');
 await page.evaluate(()=>window.__capQA.set({status:'working'}));assert.equal(await model.isVisible(),false);await page.locator('.sw-capability').filter({hasText:'после ответа'}).waitFor();checks.push('working state explains when model controls return');
 await page.evaluate(()=>window.__capQA.set({status:'idle',agent:'codex',model:'gpt-5.6-sol',instanceId:'personal',instanceLabel:'Personal'}));await model.waitFor();await page.waitForFunction(()=>[...document.querySelector('[aria-label="Модель текущего чата"]').options].some(o=>o.value==='gpt-6-astra'));
 assert.equal(await page.getByRole('combobox',{name:'Глубина рассуждений текущего чата',exact:true}).isVisible(),false);checks.push('Codex uses account model catalog and hides unsupported standalone effort');
 for (const remote of [null,'build-box']) {
   await page.evaluate(remote=>window.__capQA.set({controlMode:'external',tmuxPane:'%1',remote}),remote);
   await page.locator('.sw-capability').filter({hasText:'Только просмотр'}).waitFor();assert.equal(await page.locator('#chat .chatinput').isVisible(),false);assert.equal(await page.locator('#reply').isDisabled(),true);assert.equal(await model.isVisible(),false);
   await page.evaluate(async()=>{document.querySelector('#reply').value='Do not send this synthetic draft';await sendReplyNow();});
   assert.equal(await page.evaluate(()=>window.__sessionFixture.calls.filter(c=>c.name==='sendReply').length),0);
 }
 checks.push('local and remote external sessions are read-only despite stale pane metadata');await page.waitForFunction(()=>!document.querySelector('.toast'));await page.screenshot({path:path.join(out,'readonly-dark.png')});
 await page.evaluate(()=>window.__capQA.set({controlMode:'tmux',remote:'offline-box',tmuxPane:'%1'}));await page.locator('.sw-capability').filter({hasText:'Нет связи'}).waitFor();assert.equal(await page.locator('#chat .chatinput').isVisible(),true);assert.equal(await page.locator('#reply').isDisabled(),false);await page.locator('#reply').fill('Черновик до подключения');assert.equal(await page.locator('#chatSend').isDisabled(),true);checks.push('disconnected managed session keeps editable draft and disables delivery');
 await page.evaluate(()=>window.__capQA.set({remote:null,tmuxPane:null}));await page.locator('.sw-capability').filter({hasText:'Терминал недоступен'}).waitFor();assert.equal(await page.locator('#chat .chatinput').isVisible(),false);checks.push('missing terminal is distinct from external observation');
 await page.getByRole('button',{name:'Новый чат',exact:true}).click();await page.locator('#newChatProvider').selectOption('codex');await page.waitForFunction(()=>[...document.querySelector('#newChatModel').options].some(o=>o.value==='gpt-6-astra'));
 await page.locator('#newChatModel').selectOption('gpt-6-astra');await page.locator('#newChatDirectory').fill('/fixture/project');await page.locator('#newChatPrompt').fill('Synthetic model launch');await page.getByRole('button',{name:'Начать задачу',exact:true}).click();await page.waitForFunction(()=>window.__capQA.calls.some(c=>c.name==='launchSession'));
 const launch=await page.evaluate(()=>window.__capQA.calls.find(c=>c.name==='launchSession').args);assert.equal(launch[4].model,'gpt-6-astra');assert.equal(launch[4].instanceId,'personal');checks.push('new task passes chosen model with its selected source');
 await page.screenshot({path:path.join(out,'new-task-model.png')});assert.deepEqual(errors,[]);fs.writeFileSync(path.join(out,'report.json'),JSON.stringify({checks,errors},null,2));console.log(JSON.stringify({checks,errors}));
}finally{await browser.close();server.close();}})().catch(e=>{console.error(e);process.exitCode=1;server.close();});
