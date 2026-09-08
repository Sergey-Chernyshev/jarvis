import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const source = readFileSync(new URL('./toast.js', import.meta.url), 'utf8');
const flush = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };
function boot({ pendingResize = false, reduce = false } = {}) {
  const { window, document } = parseHTML('<html><body><div id="stack"></div></body></html>');
  const events = {}, animations = [], resizes = [], timers = [];
  let release;
  window.matchMedia = () => ({ matches: reduce });
  window.getComputedStyle = () => ({ borderRadius:'19px', paddingLeft:'16px', paddingRight:'16px' });
  window.HTMLElement.prototype.getBoundingClientRect = function () {
    const compact = ['listening','analyzing'].includes(this.dataset.phase);
    return { width:compact ? 225 : 392, height:compact ? 48 : 100, top:0, bottom:100 };
  };
  window.HTMLElement.prototype.animate = function (frames, options) {
    let resolve, reject;
    const a = { el:this, frames, options, cancelled:false,
      finished:new Promise((yes, no) => { resolve=yes; reject=no; }),
      finish:() => resolve(), cancel:() => { a.cancelled=true; reject(new Error('cancelled')); } };
    animations.push(a); return a;
  };
  Object.defineProperty(document.getElementById('stack'), 'scrollHeight', { get:() => 160 });
  window.toast = new Proxy({
    resize: h => { resizes.push(h); return pendingResize ? new Promise(resolve => { release = () => { pendingResize=false; resolve(); }; }) : Promise.resolve(); },
    audioState: async () => null, meetingStatus: async () => null,
  }, { get(target, name) { if (name in target) return target[name]; if (name.startsWith('on')) return callback => { events[name] = callback; }; return () => Promise.resolve(); } });
  new Function('window', 'document', 'setTimeout', 'clearTimeout', 'setInterval', source)(window, document,
    (fn, ms) => { const timer={ fn, ms }; timers.push(timer); return timer; }, timer => { if(timer) timer.cleared=true; }, () => 0);
  return { document, animations, resizes, timers, release:() => release(),
    send: phase => events.onVoiceHud({ id:'voice-hud', phase, title:phase, body:phase === 'heard' ? 'Synthetic text' : '' }) };
}

test('hidden toast starts its entry only after native resize acknowledges presentation', async () => {
  const h = boot({ pendingResize:true }); h.send('listening'); await flush();
  assert.equal(h.animations.length, 0);
  assert.equal(h.document.querySelector('.voice').classList.contains('in'), false);
  h.release(); await flush();
  assert.equal(h.animations.length, 1);
  assert.equal(h.document.querySelector('.voice').classList.contains('in'), true);
});

test('a superseded morph completion cannot clear the current phase or its content', async () => {
  const h = boot(); h.send('listening'); await flush(); h.animations[0].finish(); await flush();
  const shell = h.document.querySelector('.voice');
  h.send('analyzing'); await flush(); const prior = h.animations.findLast(a => a.el === shell);
  h.send('heard'); await flush(); const current = h.animations.findLast(a => a.el === shell);
  assert.notEqual(current, prior); assert.equal(prior.cancelled, true);
  prior.finish(); await flush();
  assert.equal(shell.dataset.phase, 'heard');
  assert.equal(shell.querySelector('.title').textContent, 'heard');
  assert.equal(current.cancelled, false);
  current.finish(); await flush();
  assert.equal(shell.querySelector('.card-ghost'), null);
  assert.equal(shell.style.width, '');
  assert.equal(shell.querySelectorAll('.cont').length, 1, 'copy action survives cleanup');
});

test('restarting during exit reuses its shell and ignores stale exit cleanup', async () => {
  const h = boot(); h.send('listening'); await flush(); h.animations[0].finish(); await flush();
  const shell = h.document.querySelector('.voice'); h.send('dismiss');
  const exitFallback = h.timers.findLast(timer => timer.ms === 260);
  h.send('listening'); await flush(); exitFallback.fn(); await flush();
  assert.equal(h.document.querySelectorAll('.voice').length, 1);
  assert.equal(h.document.querySelector('.voice'), shell);
  assert.equal(shell.dataset.phase, 'listening');
  assert.ok(!h.resizes.includes(0), 'stale exit must not hide the new recording');
});

test('reduced motion keeps all phases functional without finite animations', async () => {
  const h = boot({ reduce:true }); h.send('listening'); await flush(); h.send('heard'); await flush();
  assert.equal(h.animations.length, 0);
  assert.ok(h.document.querySelector('.voice.in .cont'));
  h.send('dismiss'); await flush();
  assert.equal(h.document.querySelector('.voice'), null);
  assert.equal(h.resizes.at(-1), 0);
});


test('elapsed listening updates keep the waveform and its animation clock intact', async () => {
  const h = boot(); h.send('listening'); await flush();
  const wave = h.document.querySelector('.hud-wave');
  const entry = h.animations[0];
  h.send('listening'); await flush();
  assert.equal(h.document.querySelector('.hud-wave'), wave);
  assert.equal(h.animations.length, 1);
  assert.equal(entry.cancelled, false);
});
