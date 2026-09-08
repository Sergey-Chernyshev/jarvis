import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';
const source = name => readFileSync(new URL(name, import.meta.url), 'utf8');

test('attachment batch settles before reporting failure so late progress cannot replace an error', async () => {
  const {window, document} = parseHTML('<html></html>');
  let finishSecond;
  window.jarvis = { saveAttachment: async (data, name) => name === 'a' ? {ok:false,error:'Upload failed'} : new Promise(resolve => { finishSecond = resolve; }) };
  new Function('window', 'document', source('./attachments.js'))(window, document);
  const progress = []; let failed = false;
  const result = window.JarvisAttachments.save([{name:'a',dataUrl:'data:,AA=='},{name:'b',dataUrl:'data:,BB=='}], 'remote', (done, total) => progress.push([done,total])).catch(() => { failed = true; });
  await Promise.resolve(); await Promise.resolve();
  assert.equal(failed, false);
  finishSecond({ok:true,path:'/tmp/b'}); await result;
  assert.equal(failed, true); assert.deepEqual(progress, [[0,2],[1,2]]);
});

test('status transition removes its spinner and announces errors with an actionable button', () => {
  const {window, document} = parseHTML('<html><div id="status"></div></html>');
  new Function('window', 'document', source('./async-state.js'))(window, document);
  const root = document.getElementById('status');
  window.JarvisAsyncState.status(root, 'Отправляем'); assert.ok(root.querySelector('.ui-spinner'));
  window.JarvisAsyncState.status(root, 'Отправлено', 'success'); assert.equal(root.querySelector('.ui-spinner'), null);
  let retried = false;
  const error = window.JarvisAsyncState.message({title:'Нет связи',detail:'Черновик сохранён',kind:'error',action:'Повторить',onAction:() => {retried = true;}});
  assert.equal(error.getAttribute('role'), 'alert'); error.querySelector('button').click(); assert.equal(retried, true);
});
