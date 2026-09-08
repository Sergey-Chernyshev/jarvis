import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
const { mouseFilter, bufferText, cursorViewportScroll } = createRequire(import.meta.url)('./terminal-workspace.js');
const bytes = text => new TextEncoder().encode(text);
const text = data => new TextDecoder().decode(Uint8Array.from(data));

test('read mode keeps UTF-8 and VT bytes intact across every possible chunk boundary', () => {
  const original = bytes('Привет 👋\r\n\x1b[31mошибка\x1b[0m\r\n界\x1b[?2004h');
  for (let split = 0; split <= original.length; split++) {
    const filter = mouseFilter();
    const result = Uint8Array.from([...filter.push(original.slice(0, split), false), ...filter.push(original.slice(split), false)]);
    assert.deepEqual(result, original, `boundary ${split}`);
  }
});

test('mouse capture stays local while reading, preserving mixed modes and restoring the app request on input', () => {
  const filter = mouseFilter(), source = bytes('\x1b[?1000;2004;1006htext\x1b[?1002h');
  const result = [];
  for (const b of source) result.push(...filter.push([b], false));
  assert.equal(text(result), '\x1b[?2004htext');
  assert.match(text(filter.sync(true)), /\x1b\[\?1000;1006;1002h$/);
  assert.equal(text(filter.push(bytes('\x1b[?1000;1002l'), false)), '');
  assert.match(text(filter.sync(true)), /\x1b\[\?1006h$/);
  assert.doesNotMatch(text(filter.sync(false)), /h/);
});

test('interactive mode forwards mouse requests byte-for-byte and bounded unknown sequences cannot swallow later text', () => {
  const filter = mouseFilter();
  const original = bytes('\x1b[?1000;1006hhello\x1b[?1006l');
  assert.deepEqual(filter.push(original, true), original);
  const unknown = bytes('\x1b[?' + '1'.repeat(200) + 'x👋');
  assert.deepEqual(filter.push(unknown, false), unknown);
});

test('copy all joins soft wraps without losing spaces inside a logical line', () => {
  const lines = [
    { isWrapped: false, text: 'hello ' },
    { isWrapped: true, text: 'world  ' },
    { isWrapped: false, text: 'next   ' },
    { isWrapped: false, text: '' },
  ].map(line => ({ ...line, translateToString: trim => trim ? line.text.trimEnd() : line.text }));
  assert.equal(bufferText({ length: lines.length, getLine: i => lines[i] }), 'hello world\nnext');
});

// Reproduced in the real WKWebView: a 24-row remote screen is 444px tall
// inside a 270px viewport. Scrolling to its physical bottom (184px) makes a
// three-line response disappear above the viewport despite a populated buffer.
const nativeGeometry = {screenTop:10,screenHeight:444,viewportHeight:270,scrollHeight:454};
const nativeBuffer = (cursorY, changes = {}) => ({baseY:0,viewportY:0,cursorY,type:'normal',...changes});
function assertRowVisible(row, top, geometry = nativeGeometry, rows = 24) {
  const start = geometry.screenTop + row * geometry.screenHeight / rows - top;
  assert.ok(start >= 0, `row ${row} clipped above viewport: ${start}`);
  assert.ok(start + geometry.screenHeight / rows <= geometry.viewportHeight, `row ${row} clipped below viewport: ${start}`);
}

test('short output and its cursor stay visible instead of following the blank bottom of the remote screen', () => {
  const top = cursorViewportScroll(nativeBuffer(3),24,nativeGeometry);
  assert.equal(top,0);
  for (const row of [0,1,2,3]) assertRowVisible(row,top);
  assert.throws(() => assertRowVisible(2,nativeGeometry.scrollHeight - nativeGeometry.viewportHeight), /clipped above/);
});

test('a cursor at the last row remains entirely visible even with 100000 lines of history', () => {
  for (const baseY of [0,100000]) {
    const top = cursorViewportScroll(nativeBuffer(23,{baseY,viewportY:baseY}),24,nativeGeometry);
    assert.equal(top,184);
    assertRowVisible(23,top);
  }
});

test('moving an alternate-screen cursor upwards reveals its new row instead of blank trailing rows', () => {
  const buffer = nativeBuffer(23,{type:'alternate'});
  const lower = cursorViewportScroll(buffer,24,nativeGeometry);
  buffer.cursorY = 2;
  const upper = cursorViewportScroll(buffer,24,nativeGeometry);
  assertRowVisible(23,lower); assertRowVisible(2,upper);
  assert.equal(upper,0);
});

test('even one row of deliberate history navigation disables cursor following', () => {
  for (const viewportY of [99999,1000,0]) {
    assert.equal(cursorViewportScroll(nativeBuffer(23,{baseY:100000,viewportY}),24,nativeGeometry),null);
  }
});

test('cursor visibility survives viewport size changes without resizing the remote screen', () => {
  for (const viewportHeight of [120,270,700]) {
    const geometry = {...nativeGeometry,viewportHeight,scrollHeight:Math.max(viewportHeight,454)};
    for (const cursorY of [0,3,12,23]) {
      const top = cursorViewportScroll(nativeBuffer(cursorY),24,geometry);
      assertRowVisible(cursorY,top,geometry);
    }
  }
});

test('hidden or unmeasured terminal geometry leaves scroll position alone', () => {
  for (const changes of [{screenHeight:0},{viewportHeight:0},{screenTop:NaN}]) {
    assert.equal(cursorViewportScroll(nativeBuffer(3),24,{...nativeGeometry,...changes}),null);
  }
});
