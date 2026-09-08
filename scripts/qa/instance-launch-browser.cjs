// Production question wizard, real Chromium, synthetic bridge only.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const vm = require('node:vm');
const os = require('node:os');
let playwright;
try { playwright = require('playwright'); } catch { playwright = require(process.env.JARVIS_PLAYWRIGHT_PATH || path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }
const source = fs.readFileSync(path.join(__dirname, 'session-workspace.cjs'), 'utf8');
const base = source.slice(source.indexOf('function bridgeFixture()'), source.indexOf('\nconst records ='));
const context = {}; vm.runInNewContext(base + '\nthis.fixtureSource = bridgeFixture.toString();', context);
function profileFixture() {
  const original=window.jarvis;
  const qa=window.__profileQA={calls:[],pending:[],hold:false,failRemote:false};
  const local={instances:[{id:'local-personal',label:'Personal',agent:'codex'},{id:'local-work',label:'Work',agent:'codex'}],defaultCodexInstance:'local-personal'};
  const remotes=[{name:'build-box',sources:[{id:'vm-personal',label:'VM Personal',agent:'codex'},{id:'vm-work',label:'VM Work',agent:'codex'}]}];
  window.jarvis=new Proxy(original,{get(target,name){
    if(name==='agentInstancesList')return async()=>{qa.calls.push(name);if(qa.hold){qa.hold=false;return new Promise(resolve=>qa.pending.push(()=>resolve(structuredClone(local))));}return structuredClone(local);};
    if(name==='remotesList')return async()=>{qa.calls.push(name);if(qa.failRemote){qa.failRemote=false;throw new Error('Synthetic profile list failure');}return structuredClone(remotes);};
    return target[name];
  }});
}
const root = path.resolve(__dirname, '../../ui');
const out = path.resolve(__dirname, '../../docs/qa/assets/questions');
const server = http.createServer((req,res) => {
  const file = path.resolve(root, '.' + new URL(req.url,'http://localhost').pathname);
  if (!file.startsWith(root + path.sep)) { res.writeHead(403).end(); return; }
  fs.readFile(file, (e,data) => { if(e) res.writeHead(404).end(); else res.writeHead(200,{'Content-Type':({'.html':'text/html','.js':'text/javascript','.css':'text/css','.svg':'image/svg+xml'})[path.extname(file)] || 'application/octet-stream'}).end(data); });
});
const records=[];
(async()=>{
  fs.mkdirSync(out,{recursive:true});await new Promise(r=>server.listen(0,'127.0.0.1',r));
  const browser=await playwright.chromium.launch({channel:'chrome',headless:true});
  const page=await browser.newPage({viewport:{width:1160,height:860}}),errors=[];
  page.on('pageerror',e=>errors.push(e.message));page.setDefaultTimeout(7000);
  const go=machine=>page.evaluate(machine=>window.jarvisSessionWorkspace.newChat({machine,cwd:machine==='local'?'/work/jarvis':'/srv/fixture'}),machine);
  const values=()=>page.locator('#newChatInstance option').evaluateAll(opts=>opts.map(o=>o.value));
  try{
    await page.route('**/bridge.js',route=>route.fulfill({contentType:'text/javascript',body:`(${context.fixtureSource})();(${profileFixture.toString()})();`}));
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);await page.locator('#tabSessions').click();
    await page.waitForFunction(()=>document.querySelectorAll('#newChatMachine option').length===3);
    await page.locator('#newChatProvider').selectOption('codex');
    await page.waitForFunction(()=>document.querySelector('#newChatInstance option[value="local-work"]'));
    await page.locator('#newChatInstance').selectOption('local-work');
    await go('build-box');await page.waitForFunction(()=>document.querySelector('#newChatInstance option[value="vm-work"]'));
    assert.deepEqual(await values(),['vm-personal','vm-work']);assert.equal(await page.locator('#newChatMachine').inputValue(),'build-box');
    await page.locator('#newChatInstance').selectOption('vm-work');await page.locator('#newChatPrompt').fill('Synthetic profile-scoped task');
    assert.equal(await page.getByRole('button',{name:'Начать задачу',exact:true}).isDisabled(),false);
    records.push('programmatic-newChat-remote-reloads-only-remote-profiles');
    await go('local');await page.waitForFunction(()=>document.querySelector('#newChatInstance option[value="local-work"]'));
    assert.deepEqual(await values(),['local-personal','local-work']);records.push('programmatic-return-local-reloads-only-local-profiles');
    await page.evaluate(()=>{window.__profileQA.hold=true;});await go('local');
    assert.equal(await page.getByRole('button',{name:'Начать задачу',exact:true}).isDisabled(),true);
    await go('build-box');await page.waitForFunction(()=>document.querySelector('#newChatInstance option[value="vm-work"]'));
    await page.evaluate(()=>window.__profileQA.pending.splice(0).forEach(resolve=>resolve()));
    assert.deepEqual(await values(),['vm-personal','vm-work']);records.push('late-local-profile-response-cannot-replace-current-remote-options');
    await go('local');await page.waitForFunction(()=>document.querySelector('#newChatInstance option[value="local-work"]'));
    await page.evaluate(()=>{window.__profileQA.failRemote=true;});await go('build-box');
    await page.locator('.sw-launch-status').filter({hasText:'Synthetic profile list failure'}).waitFor();
    assert.deepEqual(await values(),[]);assert.equal(await page.getByRole('button',{name:'Начать задачу',exact:true}).isDisabled(),true);
    assert.equal(await page.locator('#newChatPrompt').inputValue(),'Synthetic profile-scoped task');
    assert.equal(await page.evaluate(()=>window.__sessionFixture.calls.filter(c=>c.name==='launchSession').length),0);
    records.push('failed-profile-refresh-clears-stale-options-blocks-launch-keeps-draft');
    await page.locator('#newChatProvider').selectOption('claude');
    assert.equal(await page.getByRole('button',{name:'Начать задачу',exact:true}).isDisabled(),false);
    await page.locator('#newChatProvider').selectOption('codex');
    await go('build-box');await page.waitForFunction(()=>document.querySelector('#newChatInstance option[value="vm-work"]'));
    assert.equal(await page.getByRole('button',{name:'Начать задачу',exact:true}).isDisabled(),false);
    assert.deepEqual(errors,[]);const result={ok:true,records,errors};
    fs.writeFileSync(path.join(out,'profile-launch-results.json'),JSON.stringify(result,null,2));console.log(JSON.stringify(result));
  }catch(error){await page.screenshot({path:path.join(out,'profile-launch-failure.png')});console.error(error);console.error(JSON.stringify({records,errors}));process.exitCode=1;}
  finally{await browser.close();server.closeAllConnections();server.close();}
})().catch(error=>{console.error(error);server.closeAllConnections();server.close();process.exitCode=1;});
