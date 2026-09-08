// Real browser, production UI, synthetic profiles; no real account or SSH writes.
const fs=require('node:fs'),path=require('node:path'),http=require('node:http'),os=require('node:os'),vm=require('node:vm'),assert=require('node:assert/strict');
let playwright; try { playwright=require('playwright'); } catch { playwright=require(path.join(os.homedir(),'.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }
const source=fs.readFileSync(path.join(__dirname,'session-workspace.cjs'),'utf8'),context={};
vm.runInNewContext(source.slice(source.indexOf('function bridgeFixture()'),source.indexOf('\nconst records ='))+'\nthis.fixtureSource=bridgeFixture.toString();',context);
function profilesFixture() {
  let config={version:1,entries:[],defaultCodexInstance:'work'};
  const calls=[],health=[];
  const profiles=[{id:'personal',label:'Personal',home:'/fixture/personal/codex-home',canonicalHome:'/fixture/personal/codex-home'},{id:'work',label:'Work',home:'/fixture/work',canonicalHome:'/fixture/work'}];
  const overrides={
    agentInstancesList:async()=>({config,defaultCodexInstance:config.defaultCodexInstance,health,instances:profiles.map(p=>({...p,agent:'codex',machine:'local',enabled:true,exists:true,observedChats:1,cli:'/fixture/bin/codex',...config.entries.find(e=>e.home===p.home)}))}),
    agentInstancesSave:async value=>{calls.push({name:'save',value:structuredClone(value)});config=structuredClone(value);return{};},
    agentInstancesRepair:async ids=>{calls.push({name:'repair',ids});health.push({instanceId:ids[0],rulesInstalled:true,trustStatus:'trusted',errors:[]});return health;},
    remotesList:async()=>[{name:'build-box',sshHost:'hermes@build-box',connected:true,version:'0.3.3',sources:[{id:'remote-personal',label:'VM Personal',agent:'codex',home:'/home/hermes/.codex-personal'}]}],
    remotesPreflight:async(...args)=>{calls.push({name:'preflight',args});return{ok:true,os:'linux',arch:'aarch64',tmux:true,codex:true};},
    vmStatus:async()=>({ok:true,available:true,generation:'modern',vms:[{name:'qa',status:'running',capabilities:{},connection:{canConnect:true,name:'lima-qa',sshHost:'lima-qa',transport:'ssh',sshConfigFile:'/fixture/lima/ssh.config',jarvisDir:'~/.jarvis'}}]}),
  };
  const original=window.jarvis;
  window.jarvis=new Proxy(original,{get:(target,name)=>overrides[name]||target[name]});
  window.__instanceQA={calls};
}
const root=path.resolve(__dirname,'../../ui'),out=path.resolve(__dirname,'../../docs/qa/assets/instances');
const server=http.createServer((req,res)=>{const file=path.resolve(root,'.'+new URL(req.url,'http://localhost').pathname);if(!file.startsWith(root+path.sep)){res.writeHead(403).end();return;}fs.readFile(file,(e,b)=>{if(e)res.writeHead(404).end();else res.writeHead(200,{'Content-Type':({'.html':'text/html','.js':'text/javascript','.css':'text/css','.svg':'image/svg+xml'})[path.extname(file)]||'application/octet-stream'}).end(b);});});
(async()=>{fs.mkdirSync(out,{recursive:true});await new Promise(r=>server.listen(0,'127.0.0.1',r));const browser=await playwright.chromium.launch({channel:'chrome',headless:true});const page=await browser.newPage({viewport:{width:1160,height:860},colorScheme:'dark'});const errors=[],checks=[];page.on('pageerror',e=>errors.push(e.message));page.setDefaultTimeout(8000);
try{
 await page.route('**/bridge.js',route=>route.fulfill({contentType:'text/javascript',body:`(${context.fixtureSource})();(${profilesFixture.toString()})();`}));
 await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);await page.locator('#tabSettings').click();await page.evaluate(()=>window.jarvisOpenSettingsPane('agents'));
 await page.getByRole('region',{name:'Профили Codex'}).waitFor();
 const pane=page.locator('.instance-settings');await pane.locator('.instance-name').filter({hasText:/^Personal$/}).waitFor();
 assert.equal(await pane.locator('.instance-profile').count(),2);assert.equal(await pane.locator('.instance-path:visible').count(),0);checks.push('two profiles displayed separately with technical details collapsed');
 await pane.getByLabel('Профиль Codex по умолчанию').selectOption('personal');await page.waitForFunction(()=>window.__instanceQA.calls.some(c=>c.name==='save'&&c.value.defaultCodexInstance==='personal'));checks.push('default profile persisted');
 await pane.getByRole('button',{name:'Настроить Personal',exact:true}).click();await pane.locator('.instance-profile').first().getByLabel('Название профиля').fill('Личный Codex');await pane.getByRole('button',{name:'Сохранить',exact:true}).first().click();await pane.locator('.instance-name').filter({hasText:/^Личный Codex$/}).waitFor();checks.push('rename retains identity and home');
 await pane.getByRole('button',{name:'Настроить хуки',exact:true}).first().click();await pane.getByText('Разрешено',{exact:true}).waitFor();assert.equal(await page.evaluate(()=>window.__instanceQA.calls.find(c=>c.name==='repair').ids.join(',')),'personal');checks.push('repair targets selected profile');
 await page.screenshot({path:path.join(out,'profiles.png')});
 await page.evaluate(()=>window.jarvisOpenSettingsPane('remotes'));
 const vmArea=page.getByRole('region',{name:'Виртуальные машины'});await vmArea.waitFor();
 assert.equal(await vmArea.locator('#s2-vm-environment').evaluate(n=>n.open),false);
 for(const width of [900,1280]){
  await page.setViewportSize({width,height:860});await vmArea.scrollIntoViewIfNeeded();
  await page.evaluate(async()=>{await document.fonts.ready;await Promise.allSettled(document.getAnimations().filter(a=>Number.isFinite(a.effect?.getComputedTiming().endTime)).map(a=>a.finished));});
  assert.ok(await vmArea.evaluate(n=>n.scrollWidth<=n.clientWidth+1),`VM section overflows at ${width}`);
  await vmArea.screenshot({path:path.join(out,`vm-overview-${width}.png`)});
 }
 checks.push('compact VM inventory fits 900/1280px with runtime details collapsed');
 await page.getByRole('button',{name:'Подключить к Jarvis',exact:true}).click();
 await page.waitForFunction(()=>window.__instanceQA.calls.some(c=>c.name==='preflight'));const probe=await page.evaluate(()=>window.__instanceQA.calls.find(c=>c.name==='preflight').args);assert.equal(probe[2].sshConfigFile,'/fixture/lima/ssh.config');assert.equal(probe[2].sshHost,'lima-qa');checks.push('VM connection retains explicit SSH configuration');
 await page.screenshot({path:path.join(out,'vm-connection.png')});
 await page.locator('#pageHome').click();await page.locator('#tabSessions').click();await page.locator('#newChatProvider').selectOption('codex');await page.locator('#newChatInstance option[value="personal"]').waitFor({state:'attached'});assert.equal(await page.locator('#newChatInstance').inputValue(),'personal');checks.push('new chat uses selected default profile');
 assert.deepEqual(errors,[]);fs.writeFileSync(path.join(out,'report.json'),JSON.stringify({checks,errors},null,2));console.log(JSON.stringify({checks,errors}));
}finally{await browser.close();server.close();}})().catch(e=>{console.error(e);process.exitCode=1;server.close();});
