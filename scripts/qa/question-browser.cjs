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
function questionFixture() {
  const original = window.jarvis;
  window.__questionQA = { pending: [], calls: [], outcome: null };
  window.jarvis = new Proxy(original, { get(target, name) {
    if (name === 'answerQuestion') return (sid, payload) => {
      window.__questionQA.calls.push({ sid, payload: structuredClone(payload) });
      return new Promise(resolve => window.__questionQA.pending.push(resolve));
    };
    return target[name];
  } });
}
const root = path.resolve(__dirname, '../../ui');
const out = path.resolve(__dirname, '../../docs/qa/assets/questions');
const server = http.createServer((req,res) => {
  const file = path.resolve(root, '.' + new URL(req.url,'http://localhost').pathname);
  if (!file.startsWith(root + path.sep)) { res.writeHead(403).end(); return; }
  fs.readFile(file, (e,data) => { if(e) res.writeHead(404).end(); else res.writeHead(200,{'Content-Type':({'.html':'text/html','.js':'text/javascript','.css':'text/css','.svg':'image/svg+xml'})[path.extname(file)] || 'application/octet-stream'}).end(data); });
});
const q = { requestId:'request-qa', revision:1, transport:'tmux', at:123, questions:[
  { id:'machine', header:'Машина', question:'Где запустить проверки?', options:[{id:'local',label:'Локально',description:'На этом компьютере'},{id:'vm',label:'В VM',description:'На удалённой машине'}], multiSelect:false, customAllowed:true },
  { id:'checks', header:'Проверки', question:'Какие проверки нужны?', options:[{id:'unit',label:'Unit',description:''},{id:'ui',label:'UI',description:''},{id:'integration',label:'Integration',description:''}], multiSelect:true, customAllowed:true },
  { id:'notes', header:'Пожелания', question:'Что ещё учесть?', options:[], multiSelect:false, customAllowed:true },
] };
const records=[];
(async()=>{
  fs.mkdirSync(out,{recursive:true}); await new Promise(r=>server.listen(0,'127.0.0.1',r));
  const browser=await playwright.chromium.launch({channel:'chrome',headless:true});
  const page=await browser.newPage({viewport:{width:1160,height:860},colorScheme:'dark',reducedMotion:'reduce'});
  const errors=[]; page.on('pageerror',e=>errors.push(e.message)); page.setDefaultTimeout(7000);
  const emit = question => page.evaluate(question=>{
    const f=window.__sessionFixture; const list=f.sessions();
    const s=list.find(s=>s.id==='local-chat'); s.question=question; s.status=question?'waiting':'working'; f.emitState(list);
  },question);
  const open = async()=>{await page.evaluate(()=>window.openSessionById('local-chat'));await page.locator('#qWrap').waitFor({state:'visible'});};
  const primary=()=>page.locator('#qpFoot [data-q-submit]');
  const resolve=result=>page.evaluate(result=>window.__questionQA.pending.shift()(result),result);
  const count=()=>page.evaluate(()=>window.__questionQA.calls.length);
  try {
    await page.route('**/bridge.js',route=>route.fulfill({contentType:'text/javascript',body:`(${context.fixtureSource})();(${questionFixture.toString()})();`}));
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.waitForFunction(()=>document.documentElement.dataset.view==='home');
    await emit(q); await open();
    await page.locator('#qpOpts .qopt').nth(1).click(); assert.equal(await count(),0);
    await primary().click();
    await page.locator('#qpOpts .qopt').nth(0).click(); await page.locator('#qpOpts .qopt').nth(2).click();
    await page.locator('#qpCustom').fill('И тест отключения сети');
    await primary().click();
    await page.locator('#qpCustom').fill('Первая строка'); await page.locator('#qpCustom').press('Enter'); await page.locator('#qpCustom').pressSequentially('Вторая строка');
    assert.equal(await count(),0); assert.match(await page.locator('#qpCustom').inputValue(),/Первая строка\nВторая строка/);
    await page.evaluate(()=>{window.__questionText=document.getElementById('qpCustom');window.__questionText.setSelectionRange(3,3);});
    await emit(q);
    assert.equal(await page.evaluate(()=>document.getElementById('qpCustom')===window.__questionText && document.activeElement===window.__questionText && window.__questionText.selectionStart===3),true);
    await page.locator('#qpOpts').focus(); await page.keyboard.press('Space');
    assert.match(await page.locator('#qpCustom').inputValue(),/Первая строка\nВторая строка/);
    records.push('multiline-state-push-preserves-node-focus-and-caret');
    await page.locator('#qpFoot').getByRole('button',{name:'Назад',exact:true}).click();
    assert.equal(await page.locator('#qpOpts [aria-checked="true"]').count(),2);
    assert.equal(await page.locator('#qpCustom').inputValue(),'И тест отключения сети');
    await primary().click(); assert.match(await page.locator('#qpCustom').inputValue(),/Вторая строка/);
    await primary().click(); await page.locator('.q-review-row').first().waitFor();
    assert.equal(await page.locator('.q-review-row').count(),3);
    await page.screenshot({path:path.join(out,'review-mixed.png')});
    records.push('single-multi-custom-back-review');
    await primary().click(); await page.keyboard.press('Enter');
    assert.equal(await count(),1); assert.equal(await primary().isDisabled(),true);
    const sent=await page.evaluate(()=>window.__questionQA.calls[0]);
    assert.equal(sent.sid,'local-chat'); assert.equal(sent.payload.requestId,q.requestId); assert.equal(sent.payload.revision,1);
    assert.deepEqual(sent.payload.answers[0],{questionId:'machine',optionIds:['vm'],text:null});
    assert.deepEqual(sent.payload.answers[1],{questionId:'checks',optionIds:['unit','integration'],text:'И тест отключения сети'});
    await resolve({ok:false,delivery:'failed',error:'Машина недоступна'});
    await page.locator('.q-status').filter({hasText:'Машина недоступна'}).waitFor(); assert.equal(await primary().isDisabled(),false);
    await primary().click(); assert.equal(await count(),2);
    const retry=await page.evaluate(()=>window.__questionQA.calls[1]); assert.equal(retry.payload.submissionId,sent.payload.submissionId);
    await resolve({ok:false,delivery:'unknown',error:'Ответ мог дойти. Проверь терминал.'});
    await page.locator('.q-status').filter({hasText:'Ответ мог дойти'}).waitFor(); assert.equal(await primary().isDisabled(),true);
    await page.keyboard.press('Enter'); assert.equal(await count(),2);
    records.push('duplicate-pending-failed-retry-unknown-no-replay');
    await page.locator('#qpClose').click(); await open(); assert.equal(await primary().isDisabled(),true); assert.equal(await page.locator('.q-review-row').count(),3);
    records.push('unknown-draft-survives-close-reopen');
    const newer={...q,revision:2,questions:[{...q.questions[2],id:'new',question:'Новый вопрос'}]};
    await emit(newer); await page.locator('#qpTitle').filter({hasText:'Новый вопрос'}).waitFor();
    assert.equal(await page.locator('#qpCustom').inputValue(),''); assert.equal(await primary().isDisabled(),false);
    await page.locator('#qpCustom').fill('Только новый ответ'); await primary().click(); await primary().click();
    assert.equal(await count(),3); await emit(null); await resolve({ok:true,delivery:'confirmed'});
    await page.locator('#qWrap').waitFor({state:'hidden'});
    records.push('new-revision-isolated-and-confirmation-closes');
    await emit({...newer,revision:4}); await open();
    await page.locator('#qpCustom').fill('Ответ в локальный чат'); await primary().click(); await primary().click();
    await page.evaluate(question=>{
      const f=window.__sessionFixture, list=f.sessions(); const remote=list.find(s=>s.id==='build-box:remote-chat');
      remote.status='waiting'; remote.question=question; f.emitState(list); window.openSessionById(remote.id);
    },{...newer,revision:4});
    await page.waitForFunction(()=>document.querySelector('#sessionSidebar [data-session-id="build-box:remote-chat"]')?.getAttribute('aria-current')==='page');
    await page.locator('#qpCustom').fill('Независимый удалённый ответ');
    await resolve({ok:true,delivery:'confirmed'});
    assert.equal(await page.locator('#qWrap').isVisible(),true);
    assert.equal(await page.locator('#qpCustom').inputValue(),'Независимый удалённый ответ');
    records.push('late-answer-cannot-close-another-machine-question');
    await emit({...newer,revision:3,transport:'external'}); await open();
    assert.equal(await primary().isDisabled(),true); await page.getByRole('button',{name:'Открыть в Codex',exact:true}).click();
    records.push('external-rpc-explicit-open-in-agent-capability');
    assert.deepEqual(errors,[]);
    fs.writeFileSync(path.join(out,'results.json'),JSON.stringify({ok:true,records,errors},null,2)+'\n');
    console.log(JSON.stringify({ok:true,records,errors}));
  } catch(error) { await page.screenshot({path:path.join(out,'failure.png')}); console.error(error); console.error(JSON.stringify({records,errors,unknown:await page.evaluate(()=>window.__sessionFixture?.unknown)}));process.exitCode=1; }
  finally {await browser.close();server.closeAllConnections();await new Promise(r=>server.close(r));}
})();
