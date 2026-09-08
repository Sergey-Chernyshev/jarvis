// Actual Chromium keyboard/popover behavior and custom-accent review at 650px.
// All bridge data is synthetic; no installed profile/configuration is changed.
const fs=require('node:fs'),path=require('node:path'),http=require('node:http'),os=require('node:os'),vm=require('node:vm'),assert=require('node:assert/strict');
let playwright;try{playwright=require('playwright');}catch{playwright=require(path.join(os.homedir(),'.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright'));}
const source=fs.readFileSync(path.join(__dirname,'session-workspace.cjs'),'utf8'),context={};
vm.runInNewContext(source.slice(source.indexOf('function bridgeFixture()'),source.indexOf('\nconst records ='))+'\nthis.source=bridgeFixture.toString();',context);
function contrast(foreground, background) {
 const luminance = value => value.match(/[\d.]+/g).slice(0,3).map(Number).map(n=>{const x=n/255;return x<=.04045?x/12.92:((x+.055)/1.055)**2.4;}).reduce((sum,n,i)=>sum+n*[.2126,.7152,.0722][i],0);
 const a=luminance(foreground),b=luminance(background);return (Math.max(a,b)+.05)/(Math.min(a,b)+.05);
}
function reviewFixture(){
  const original=window.jarvis,calls=[];
  const config={entries:[],defaultCodexInstance:'personal'};
  const overrides={
    agentInstancesList:async()=>({config,defaultCodexInstance:'personal',health:[],instances:['personal','work'].map(id=>({id,label:id,home:`/fixture/${id}`,canonicalHome:`/fixture/${id}`,enabled:true,exists:true,observedChats:2,agent:'codex',machine:'local'}))}),
    agentsList:async()=>({ok:true,agents:[],presets:[{id:'pi',name:'Pi',bin:'pi'}]}),
    agentInstancesSave:async value=>{calls.push(value);return {ok:true};},
  };
  window.jarvis=new Proxy(original,{get:(target,name)=>overrides[name]||target[name]});window.__reviewCalls=calls;
}
const root=path.resolve(__dirname,'../../ui'),out=path.resolve(__dirname,'../../docs/qa/assets/settings-refinement');
const server=http.createServer((req,res)=>{const file=path.resolve(root,'.'+new URL(req.url,'http://localhost').pathname);if(!file.startsWith(root+path.sep))return res.writeHead(403).end();fs.readFile(file,(e,b)=>e?res.writeHead(404).end():res.writeHead(200,{'Content-Type':({'.html':'text/html','.js':'text/javascript','.css':'text/css','.svg':'image/svg+xml','.woff2':'font/woff2'})[path.extname(file)]||'application/octet-stream'}).end(b));});
(async()=>{
 fs.mkdirSync(out,{recursive:true});await new Promise(r=>server.listen(0,'127.0.0.1',r));const browser=await playwright.chromium.launch({channel:'chrome',headless:true});const page=await browser.newPage({viewport:{width:650,height:900},colorScheme:'dark',reducedMotion:'reduce'}),checks=[],errors=[];page.on('pageerror',e=>errors.push(e.message));page.setDefaultTimeout(8000);
 try{
  await page.route('**/bridge.js',r=>r.fulfill({contentType:'text/javascript',body:`(${context.source})();(${reviewFixture.toString()})();`}));
  await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);await page.locator('#tabSettings').click();await page.evaluate(()=>window.jarvisOpenSettingsPane('launch'));
  const picker=page.locator('#s2-pane-launch .cselect').first(),trigger=picker.locator('.cstrigger');await trigger.waitFor();
  await trigger.focus();await page.keyboard.press('ArrowDown');assert.equal(await picker.evaluate(e=>e.classList.contains('open')),true);
  await page.keyboard.press('ArrowDown');await page.keyboard.press('Escape');assert.equal(await picker.evaluate(e=>e.classList.contains('open')),false);assert.equal(await page.locator('#s2-pane-launch').isVisible(),true);assert.equal(await trigger.evaluate(e=>document.activeElement===e),true);
  checks.push('Escape dismisses keyboard-opened settings picker and restores focus without leaving settings');
  await trigger.click();await page.keyboard.press('Tab');assert.equal(await picker.evaluate(e=>e.classList.contains('open')),false);assert.equal(await page.locator('#s2-pane-launch').isVisible(),true);checks.push('Tab leaves a settings picker with no orphaned popup');
  await page.evaluate(()=>window.jarvisOpenSettingsPane('agents'));const defaultSelect=page.getByLabel('Профиль Codex по умолчанию');await defaultSelect.waitFor();await defaultSelect.click();await page.keyboard.press('Escape');const nativePopupStayed = await page.locator('.instance-settings').isVisible();if (!nativePopupStayed) { await page.locator('#tabSettings').click(); await page.evaluate(()=>window.jarvisOpenSettingsPane('agents')); await defaultSelect.waitFor(); } assert.equal(await defaultSelect.inputValue(),'personal');assert.equal(await page.evaluate(()=>window.__reviewCalls.length),0);checks.push(nativePopupStayed ? 'Escape closes the native profile popup without navigation or an account mutation' : 'FAIL: native profile popup Escape navigates away from settings');
  await page.locator('.s2-custom-agents').getByRole('button',{name:'Добавить агента',exact:true}).click();await page.locator('.s2-custom-agents').getByRole('button',{name:'Pi',exact:true}).click();
  const form=page.locator('.s2-agent-form');const palettes=[];
  for(const theme of ['light','dark']){
   await page.evaluate(theme=>window.jarvisTheme.set({theme,paint:'custom',accent:'#b64c18'}),theme);await form.scrollIntoViewIfNeeded();await page.waitForFunction(()=>!document.getAnimations().some(animation=>animation.playState==='running'&&Number.isFinite(animation.effect?.getComputedTiming().endTime)));
   const result=await form.evaluate(el=>{const button=el.querySelector('.primary'),field=el.querySelector('input'),styles=getComputedStyle(document.documentElement),box=el.getBoundingClientRect();return{theme:document.documentElement.dataset.theme,paint:document.documentElement.dataset.paint,accent:styles.getPropertyValue('--accent').trim(),button:getComputedStyle(button).backgroundColor,ink:getComputedStyle(button).color,width:el.clientWidth,scrollWidth:el.scrollWidth,columns:getComputedStyle(el.querySelector('.s2-form-grid')).gridTemplateColumns,controlCount:[...el.querySelectorAll('input,button,summary')].filter(n=>n.getBoundingClientRect().width>0).length,escaping:[...el.querySelectorAll('input,button,summary')].filter(n=>{const r=n.getBoundingClientRect();return r.width>0&&(r.left<0||r.right>innerWidth+1);}).map(n=>n.id||n.textContent)};});
   assert.equal(result.paint,'custom');assert.equal(result.theme,theme);assert.equal(result.scrollWidth<=result.width+1,true);assert.deepEqual(result.escaping,[]);assert.equal(result.columns.includes(' '),false);assert.notEqual(result.button,'rgb(112, 223, 173)');assert.notEqual(result.button,'rgb(8, 121, 78)');result.buttonContrast=contrast(result.button,result.ink);assert.equal(result.buttonContrast>=4.5,true);palettes.push(result);
   await page.screenshot({path:path.join(out,`custom-accent-${theme}-650.png`)});checks.push(`${theme}/650: custom accent retained, form is single-column and all controls stay inside the window`);
  }
  assert.notEqual(palettes[0].button,palettes[1].button);assert.deepEqual(errors,[]);assert.equal(await page.locator('#s2-pane-agents').innerText().then(text=>text.includes('Не удалось загрузить раздел')),false);
  fs.writeFileSync(path.join(out,'settings-keyboard.json'),JSON.stringify({checks,errors,palettes},null,2));console.log(JSON.stringify({checks,errors,palettes},null,2));assert.equal(nativePopupStayed, true, 'Native select Escape navigates away from settings');
 }finally{await browser.close();server.close();}
})().catch(e=>{console.error(e);process.exitCode=1;server.close();});
