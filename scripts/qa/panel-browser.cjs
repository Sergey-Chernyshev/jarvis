// Real browser + strict synthetic bridge. Actual macOS/IPC checks are separate.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const os = require('node:os');
let playwright;
try { playwright = require('playwright'); }
catch { playwright = require(process.env.JARVIS_PLAYWRIGHT_PATH || path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }
const root = path.resolve(__dirname, '../../ui'), out = path.resolve(__dirname, '../../docs/qa/assets/panel');
fs.mkdirSync(out, { recursive: true });
const mime = { '.html':'text/html', '.js':'text/javascript', '.css':'text/css', '.svg':'image/svg+xml', '.woff2':'font/woff2' };
const server = http.createServer((req,res) => {
  const file = path.resolve(root, '.' + new URL(req.url, 'http://localhost').pathname);
  if (!file.startsWith(root + path.sep)) { res.writeHead(403).end(); return; }
  fs.readFile(file,(error,data) => { if(error) res.writeHead(404).end(); else res.writeHead(200, { 'Content-Type':mime[path.extname(file)] || 'application/octet-stream' }).end(data); });
});
function bridgeFixture() {
  const events = {}, calls = [], unknown = [], fail = {};
  let saved = { theme:'dark', mode:'window', paint:'clover', diagnostics:false };
  let remotes = [], vm = {ok:true,available:false,version:null,generation:'unknown',capabilities:{start:false,stop:false},vms:[]};
  const usage = { total:{tok:4200,api:0,plan:0.2}, window:{resetInMs:0}, series:[{label:'12:00',tok:4200}], byModel:[], byProject:[], sessions:[], byBilling:[] };
  const history = [{ project:'Jarvis', cwd:'/qa/jarvis', count:1, lastAt:Date.now(), sessions:[{id:'test-chat',agent:'codex',title:'Проверка интерфейса',lastAt:Date.now()}] }];
  const loopDraft = { id:'', name:'', agent:'claude', source:{}, sandbox:{}, exit:{gates:[],critic:{enabled:true,model:'opus'}}, memory:{}, schedule:{wake:'manual'}, limits:{}, sampling:{} };
  const bundleDraft = { id:'', name:'', machine:'local', dir:'', base:'', gates:[], budgetTokens:1000, hands:[{task:''},{task:''}], events:[] };
  const models = [{id:'whisper-turbo',label:'Whisper',kind:'stt',active:true,present:true,available:true,bytes:600000000},{id:'qwen3-0.6b',label:'Qwen',kind:'stt',present:false,available:true,bytes:0}];
  const fixture = {
    getSettings:()=>saved, setSettings:patch=>(saved={...saved,...patch},{ok:true}),
    getState:()=>[], getMeta:()=>({version:'QA',effortLevels:[]}), getPlugins:()=>[], getModels:()=>[], getAgents:()=>[], getCommands:()=>[], getPrompts:()=>[], getLimit:()=>null,
    getUsage:()=>usage, getHistory:()=>history, projectsList:()=>({ok:true,projects:[]}), projectsIconCandidates:()=>({ok:true,candidates:[]}), machinesList:()=>[{id:'local',name:'Этот компьютер',kind:'local',online:true}],
    analyticsReport:()=>({schemaVersion:1,generatedAt:'2026-09-05T12:00:00Z',period:'week',coverage:{filesScanned:0,sessions:0,errors:[],limited:false},summary:{sessions:0,prompts:0,toolCalls:0,toolErrors:0,toolUnknown:0,activeMs:0,wallMs:0,harnessScore:null},sessions:[],projects:[],models:[],outcomes:[],economics:{recorded:0,complete:0,accepted:0,groups:[],comparisons:[]}}),
    hotkeyBindings:()=>({ok:true,bindings:[{action:'panel',label:'Показать Jarvis',accel:'Command+J'}]}),
    winIsFullscreen:()=>false, reportError:()=>null, hidePanel:()=>null,
    sttGet:()=>({engine:'whisper-turbo',engines:['whisper-turbo','qwen3-0.6b'],noiseGate:false}),
    sttInputDevices:()=>({devices:['Микрофон с очень длинным названием USB Audio Interface'],current:null}), modelsGet:()=>({models}),
    sttSetEngine:()=>({ok:false,error:'Сначала установите модель'}), sttSetInputDevice:()=>({ok:true}),
    voiceGet:()=>({speaker:'aidar',speakers:['aidar','xenia'],rate:'medium'}),
    wakeGet:()=>({enabled:false,model_present:false,audio_state:'permission-pending'}),
    integrationGet:()=>({quiet:false,status:{},models:[]}), claudeAuthGet:()=>({connected:false}), serviceGet:()=>({backend:'claude',codexSidecar:true}),
    remotesList:()=>remotes, vmStatus:()=>vm, vmAction:(name,action)=>{const item=vm.vms.find(v=>v.name===name);if(!item)throw new Error('Unknown VM: '+name);if(action==='start')item.status='running';else if(action==='stop')item.status='stopped';else if(action!=='open-config')throw new Error('Unknown VM action: '+action);return {ok:true};},
    agentsList:()=>({ok:true,agents:[],presets:[]}),
    transcriptsGet:()=>({items:[]}), smartPromptsGet:()=>({enabled:false}),
    meetingsList:()=>[], meetingsStatus:()=>null, meetingsSources:()=>[{id:'microphone',label:'Микрофон',available:true}],
    loopsGet:()=>({ok:true,loops:[],templates:[]}), loopsCatalog:()=>({ok:true,models:{},presets:[{id:'test',slot:'source',category:'QA',name:'Тест',command:'true'}]}),
    loopsDraft:()=>({ok:true,item:structuredClone(loopDraft)}), loopsSave:()=>({ok:false,error:'Тестовый диск недоступен'}),
    bundleGet:()=>({ok:true,bundles:[]}), bundleDraft:()=>({ok:true,item:structuredClone(bundleDraft)}),
    bundlePlaces:()=>({ok:true,known:['/qa/project'],home:'/qa'}), bundleBrowse:()=>({ok:true,path:'/qa',parent:'/',dirs:['project']}),
  };
  window.__panelFixture = {events,calls,unknown,fail,saved:()=>saved,machines:(value)=>{remotes=value.remotes;vm=value.vm;}};
  window.jarvis = new Proxy({}, { get(_,name) {
    if (name.startsWith('on')) return callback => { events[name]=callback; return ()=>{}; };
    return async (...args) => {
      calls.push({name,args});
      if (fail[name]) { const error=fail[name]; delete fail[name]; throw new Error(error); }
      if (!(name in fixture)) { unknown.push(name); throw new Error('Unknown fixture method: '+name); }
      return structuredClone(await fixture[name](...args));
    };
  } });
}
const records=[];
(async()=>{
  await new Promise(r=>server.listen(0,'127.0.0.1',r));
  const browser=await playwright.chromium.launch({channel:'chrome',headless:true});
  const page=await browser.newPage({viewport:{width:1120,height:780},colorScheme:'dark'});
  const errors=[]; page.on('pageerror',e=>errors.push(e.message)); page.setDefaultTimeout(7000);
  try {
    await page.route('**/bridge.js',route=>route.fulfill({contentType:'text/javascript',body:'('+bridgeFixture.toString()+')();'}));
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.waitForFunction(()=>document.documentElement.dataset.view==='home');
    const settle=()=>page.waitForFunction(()=>!document.getAnimations().some(a=>a.playState==='running' && Number.isFinite(a.effect?.getComputedTiming().endTime)));
    const open=async id=>{if(await page.locator('#pageHome').isVisible()) await page.locator('#pageHome').click();await page.locator('#'+id).click();await settle();};
    const capture=async name=>{await settle();await page.screenshot({path:path.join(out,name+'.png')});records.push(name);};
    await capture('launcher-dark');
    await open('tabSettings');
    await page.locator('#s2-pane-general .toggle').first().waitFor();
    await page.evaluate(()=>window.__panelFixture.fail.setSettings='Не удалось записать настройки');
    await page.locator('#s2-pane-general .drow').filter({hasText:'Режим логов'}).locator('input').click();
    await page.locator('#settings-save-error').waitFor();
    assert.equal(await page.locator('#s2-pane-general .drow').filter({hasText:'Режим логов'}).locator('input').isChecked(),false);
    records.push('failed-toggle-rolls-back');
    await page.locator('.snav [data-pane="stt"]').click();
    const engine=page.getByRole('button',{name:'Движок распознавания',exact:true});
    await engine.focus(); await page.keyboard.press('ArrowDown'); await page.keyboard.press('ArrowDown'); await page.keyboard.press('Enter');
    await page.locator('#settings-save-error').filter({hasText:'Сначала установите модель'}).waitFor();
    assert.match(await engine.innerText(),/whisper-turbo/);
    await engine.focus(); await page.keyboard.press('ArrowDown'); await page.keyboard.press('Escape');
    assert.equal(await engine.getAttribute('aria-expanded'),'false');
    assert.equal(await page.locator('html').getAttribute('data-view'),'settings');
    records.push('keyboard-picker-error-and-escape');
    await capture('settings-stt-dark');
    await page.locator('.snav [data-pane="about"]').click();
    assert.equal(await page.locator('#settings-save-error').count(), 0);
    await page.locator('#settingsSearch').fill('автозапуск');
    await page.getByRole('button', { name: 'Запускать при старте Основное', exact: true }).click();
    await page.waitForFunction(() => document.activeElement?.querySelector('.dt')?.textContent === 'Запускать при старте');
    await page.locator('#settingsSearch').fill('прокси');
    await capture('settings-search-proxy');
    await page.getByRole('button', { name: 'Egress-прокси Под капотом', exact: true }).click();
    await page.waitForFunction(() => document.activeElement?.querySelector('.dt')?.textContent === 'Egress-прокси');
    await page.locator('#settingsSearch').fill('масштаб');
    await page.keyboard.press('ArrowDown'); await page.keyboard.press('Escape');
    assert.equal(await page.locator('#settingsSearch').inputValue(), '');
    assert.equal(await page.locator('html').getAttribute('data-view'), 'settings');
    records.push('settings-search-parameter-focus-and-scoped-errors');
    await page.locator('.snav [data-pane="look"]').click();
    const scale=page.locator('#s2-pane-look input[type="range"]');
    await scale.focus();
    await page.evaluate(()=>{window.__scaleControl=document.querySelector('#s2-pane-look input[type="range"]');});
    await page.keyboard.press('ArrowRight');
    await page.waitForFunction(()=>window.__panelFixture.saved().scale===1.05);
    assert.equal(await page.evaluate(()=>document.querySelector('#s2-pane-look input[type="range"]')===window.__scaleControl),true);
    assert.equal(await scale.evaluate(node=>document.activeElement===node),true);
    for(const amount of [0.85,1.4,1]) {
      await page.evaluate(amount=>window.jarvisTheme.adopt({scale:amount}),amount); await settle();
      const bounds=await page.locator('#panel').boundingBox();
      assert.ok(Math.abs(bounds.height-780)<2 && bounds.width<=1121,`Scaled panel escapes: ${JSON.stringify(bounds)}`);
    }
    records.push('appearance-slider-retains-focus-and-scale-fits');
    await page.evaluate(()=>window.__panelFixture.machines({
      remotes:[{name:'build-box',sshHost:'developer@build-box',jarvisDir:'~/.jarvis',connected:true,version:'0.3.3'}],
      vm:{ok:true,available:true,version:'0.10.0',generation:'modern',capabilities:{start:true,stop:true},vms:[
        {name:'dev-linux',status:'stopped',registryStatus:'managed',directory:'/work/environments/dev-linux',configPath:'/work/environments/dev-linux/agent-vm.yaml',projects:[{name:'frontend',path:'/work/web',guestPath:'/projects/web'},{name:'backend',path:'/work/api',guestPath:'/projects/api'}],capabilities:{start:true,stop:true,openConfig:true}},
        {name:'review-linux',status:'running',registryStatus:'managed',directory:'/work/environments/review-linux',projects:[],capabilities:{start:true,stop:true,openConfig:false}},
        {name:'unrecognized-linux',status:'unknown',registryStatus:'unmanaged',projects:[],capabilities:{start:false,stop:false,openConfig:false}}
      ]}
    }));
    await page.locator('.snav [data-pane="remotes"]').click();
    await page.locator('[data-vm-name="dev-linux"]').waitFor();
    assert.equal(await page.locator('#s2-ssh-setup').evaluate(n=>n.open),false);
    assert.equal(await page.locator('#s2-vm-details-dev-linux').evaluate(n=>n.open),false);
    assert.equal(await page.locator('[data-vm-name="unrecognized-linux"] button').count(),0);
    await capture('machines-modern-dark-desktop');
    const devVm=page.locator('[data-vm-name="dev-linux"]');
    await page.evaluate(()=>window.__panelFixture.fail.vmAction='VM runtime unavailable');
    await devVm.getByRole('button',{name:'Запустить',exact:true}).click();
    await page.locator('#settings-save-error').filter({hasText:'VM runtime unavailable'}).waitFor();
    assert.equal(await devVm.getByRole('button',{name:'Запустить',exact:true}).isEnabled(),true);
    await devVm.getByRole('button',{name:'Запустить',exact:true}).click();
    await devVm.getByRole('button',{name:'Остановить',exact:true}).waitFor();
    await devVm.getByText('Проекты и конфигурация',{exact:true}).click();
    await devVm.getByText('frontend',{exact:true}).waitFor();
    await devVm.getByText('backend',{exact:true}).waitFor();
    await devVm.getByRole('button',{name:'Открыть конфигурацию',exact:true}).click();
    assert.deepEqual(await page.evaluate(()=>window.__panelFixture.calls.filter(c=>c.name==='vmAction').map(c=>c.args)),[['dev-linux','start'],['dev-linux','start'],['dev-linux','open-config']]);
    records.push('vm-capabilities-failure-retry-and-multiple-projects');
    await page.setViewportSize({width:740,height:600});
    await page.evaluate(()=>window.jarvisTheme.adopt({theme:'light',mode:'overlay'}));
    await devVm.scrollIntoViewIfNeeded();
    await capture('machines-modern-light-compact');
    const machineLayout=await page.locator('#s2-pane-remotes').evaluate(n=>({width:n.clientWidth,scrollWidth:n.scrollWidth}));
    assert.ok(machineLayout.scrollWidth<=machineLayout.width+1,'VM details overflow: '+JSON.stringify(machineLayout));
    await page.locator('#s2-task-isolation > summary').click();
    await page.getByRole('textbox',{name:'Образ Docker',exact:true}).fill('example/agent:stable');
    await page.getByRole('button',{name:'Сохранить образ',exact:true}).click();
    await page.waitForFunction(()=>window.__panelFixture.saved().launchDockerImage==='example/agent:stable');
    await page.locator('#s2-task-isolation').scrollIntoViewIfNeeded();
    await capture('machines-docker-light-compact');
    await page.locator('.snav [data-pane="launch"]').click();
    assert.equal(await page.locator('#s2-launch-advanced').evaluate(n=>n.open),false);
    assert.equal(await page.locator('input[aria-label="Образ Docker"]').count(),1);
    records.push('single-docker-editor-and-collapsed-local-advanced');
    // Every settings pane at the actual compact overlay width, both themes.
    for(const theme of ['dark','light']) {
      await page.setViewportSize({width:740,height:600});
      await page.evaluate(theme=>window.jarvisTheme.adopt({theme,mode:'overlay'}),theme);
      const panes=await page.locator('.snav [data-pane]').evaluateAll(nodes=>nodes.map(n=>n.dataset.pane));
      for(const pane of panes) {
        await page.locator(`.snav [data-pane="${pane}"]`).click();
        await page.locator(`#s2-pane-${pane} .dtitle`).waitFor();
        await page.waitForFunction(p=>!document.querySelector('#s2-pane-'+p).querySelector('.skel'),pane);
        await settle();
        const layout=await page.locator(`#s2-pane-${pane}`).evaluate(node=>({width:node.clientWidth,scrollWidth:node.scrollWidth,opacity:getComputedStyle(node).opacity}));
        assert.ok(layout.scrollWidth<=layout.width+1,`${pane} overflow ${JSON.stringify(layout)}`);
        assert.equal(layout.opacity,'1');
        await capture(`settings-${pane}-${theme}-compact`);
      }
    }
    await open('tabLoops');
    await page.getByRole('button',{name:'+ Новый цикл',exact:true}).click();
    const name=page.locator('#loops input').first(); await name.fill('Мой черновик');
    await page.getByRole('button',{name:'из каталога',exact:true}).first().click();
    await page.keyboard.press('Escape'); assert.equal(await page.locator('.lp-shade').count(),0);
    assert.equal(await page.locator('html').getAttribute('data-view'),'loops');
    await page.getByRole('button',{name:'Создать цикл',exact:true}).click();
    await page.locator('.lp-note').filter({hasText:'Тестовый диск недоступен'}).waitFor();
    assert.equal(await name.inputValue(),'Мой черновик');
    await page.keyboard.press('Escape'); await page.getByRole('button',{name:'+ Новый цикл',exact:true}).click();
    assert.equal(await page.locator('#loops input').first().inputValue(),'Мой черновик');
    await capture('automation-draft-compact');
    records.push('automation-draft-errors-modal-escape');
    await open('tabBundle');
    await page.locator('.bd-task').first().fill('Задача остаётся в черновике');
    await capture('bundle-draft-compact');
    await open('tabStats'); await capture('stats-compact');
    const unknown=await page.evaluate(()=>window.__panelFixture.unknown);
    assert.deepEqual(unknown,[]); assert.deepEqual(errors,[]);
    fs.writeFileSync(path.join(out,'report.json'),JSON.stringify({ok:true,scope:'Chromium and strict synthetic bridge; no native actions',records,errors,unknown},null,2));
    fs.rmSync(path.join(out,'failure.png'),{force:true});
    console.log(JSON.stringify({ok:true,records:records.length,errors,unknown}));
  } catch(error) {
    console.error(error); console.error(JSON.stringify({unknown:await page.evaluate(()=>window.__panelFixture?.unknown),errors}));
    await page.screenshot({path:path.join(out,'failure.png')}); process.exitCode=1;
  } finally {await browser.close();server.close();}
})().catch(error=>{console.error(error);server.close();process.exitCode=1;});
