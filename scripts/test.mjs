#!/usr/bin/env node
/**
 * Тесты без фреймворка: node scripts/test.mjs
 *
 * 1. rcblock: merge / идемпотентность / замена / демёрж — чистые функции.
 * 2. claude-shim: skip-логика против фейкового PATH (фейковые claude и tmux
 *    пишут свои argv в файлы). Ветка «обернуть в tmux» требует настоящий tty
 *    на stdin/stdout — в CI его нет, поэтому проверяем все пути сквозного exec.
 */

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { mergeBlock, removeBlock, hasBlock, BEGIN, END } from './rcblock.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const SHIM = path.join(__dirname, '..', 'bin', 'claude-shim');

let failed = 0;
function ok(cond, name) {
  if (cond) console.log(`✓ ${name}`);
  else { failed++; console.error(`✗ ${name}`); }
}

/* ---------------- rcblock ---------------- */

{
  const dir = '/Users/test/.jarvis/shims';

  const fromEmpty = mergeBlock('', dir);
  ok(hasBlock(fromEmpty), 'rcblock: вставка в пустой файл');
  ok(fromEmpty.includes(`export PATH="${dir}:$PATH"`), 'rcblock: PATH правильный');

  const existing = '# мой zshrc\nexport FOO=bar\n';
  const merged = mergeBlock(existing, dir);
  ok(merged.startsWith(existing), 'rcblock: существующее содержимое не тронуто');

  ok(mergeBlock(merged, dir) === merged, 'rcblock: повторный merge идемпотентен');

  const stale = merged.replace(dir, '/old/path');
  const refreshed = mergeBlock(stale, dir);
  ok(refreshed.includes(dir) && !refreshed.includes('/old/path'), 'rcblock: устаревший блок заменяется');
  ok((refreshed.match(new RegExp(BEGIN.replace(/[>]/g, '\\$&'), 'g')) || []).length === 1
    || refreshed.split(BEGIN).length === 2, 'rcblock: блок ровно один');

  const removed = removeBlock(merged);
  ok(!hasBlock(removed), 'rcblock: демёрж убирает блок');
  ok(removed.includes('export FOO=bar'), 'rcblock: демёрж сохраняет чужое');
  ok(removeBlock(removed) === removed, 'rcblock: повторный демёрж идемпотентен');
}

/* ---------------- claude-shim: skip-логика ---------------- */

function mkFakeEnv() {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'jarvis-test-'));
  const bin = path.join(tmp, 'bin');
  fs.mkdirSync(bin, { recursive: true });

  // фейковый claude: пишет свои argv в файл и выходит
  const claudeLog = path.join(tmp, 'claude-args');
  fs.writeFileSync(path.join(bin, 'claude'),
    `#!/bin/sh\nprintf '%s\\n' "$@" > "${claudeLog}"\nexit 0\n`);
  fs.chmodSync(path.join(bin, 'claude'), 0o755);

  // фейковый tmux: фиксирует сам факт вызова
  const tmuxLog = path.join(tmp, 'tmux-args');
  fs.writeFileSync(path.join(bin, 'tmux'),
    `#!/bin/sh\nprintf '%s\\n' "$@" > "${tmuxLog}"\nexit 0\n`);
  fs.chmodSync(path.join(bin, 'tmux'), 0o755);

  return { tmp, bin, claudeLog, tmuxLog };
}

function runShim(args, env, { allowFail = false } = {}) {
  try {
    execFileSync(SHIM, args, {
      env: { HOME: os.homedir(), ...env },
      stdio: ['pipe', 'pipe', 'pipe'], // stdin — пайп, т.е. заведомо не tty
    });
    return { code: 0 };
  } catch (err) {
    if (!allowFail) throw err;
    return { code: err.status, stderr: String(err.stderr || '') };
  }
}

{
  const { bin, claudeLog, tmuxLog } = mkFakeEnv();
  const env = { PATH: `${bin}:/usr/bin:/bin` };

  runShim(['-p', '2+2'], env);
  ok(fs.readFileSync(claudeLog, 'utf8') === '-p\n2+2\n', 'shim: -p уходит в настоящий claude как есть');
  ok(!fs.existsSync(tmuxLog), 'shim: -p не трогает tmux');

  fs.rmSync(claudeLog, { force: true });
  runShim(['--print'], env);
  ok(fs.existsSync(claudeLog), 'shim: --print тоже сквозной');

  fs.rmSync(claudeLog, { force: true });
  runShim(['--append-system-prompt', 'два слова'], env); // stdin-пайп → сквозной
  ok(fs.readFileSync(claudeLog, 'utf8') === '--append-system-prompt\nдва слова\n',
    'shim: аргумент с пробелом доходит целиком (без tty)');

  fs.rmSync(claudeLog, { force: true });
  runShim([], { ...env, TMUX: '/tmp/fake,1,0' });
  ok(fs.existsSync(claudeLog), 'shim: внутри $TMUX — сквозной');

  fs.rmSync(claudeLog, { force: true });
  runShim([], { ...env, JARVIS_SHIM: '1' });
  ok(fs.existsSync(claudeLog), 'shim: предохранитель от рекурсии — сквозной');

  // настоящего claude нет в PATH
  const res = runShim([], { PATH: '/usr/bin:/bin' }, { allowFail: true });
  ok(res.code === 127, 'shim: без настоящего claude — код 127');
  ok(res.stderr.includes('не найден'), 'shim: внятная ошибка про отсутствие бинаря');
}

/* ---------------- keep-awake: движок грантов ---------------- */
/* Чистый движок: assertion активна ⇔ грантов > 0. Electron не нужен —
 * blocker, часы и пульс инжектятся. Таймеры в тестах настоящие, короткие. */

const { createEngine } = await import('../src/plugins/keep-awake/engine.js');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function mkBlocker() {
  // считаем start/stop и отдаём id=0 — ловушка на проверку truthiness
  const b = { starts: [], stops: 0, next: 0 };
  b.start = (type) => { b.starts.push(type); return b.next++; };
  b.stop = () => { b.stops++; };
  return b;
}

{
  const b = mkBlocker();
  const e = createEngine({ blocker: b, blockerType: () => 'prevent-app-suspension' });
  ok(!e.active(), 'engine: свежий — не активен');

  e.setWorking(2);
  ok(e.active(), 'engine: working>0 → assertion взята');
  ok(b.starts.length === 1 && b.starts[0] === 'prevent-app-suspension',
    'engine: blocker стартован с нужным типом');
  ok(e.state().auto === true, 'engine: state показывает auto-грант');

  e.setWorking(3); // больше working — но грант уже есть
  ok(b.starts.length === 1, 'engine: повторный working не плодит блокеры');
  e.dispose();
}

{
  // линджер: working→0 держит грант ещё lingerMs (мост для авто-циклов)
  const b = mkBlocker();
  const e = createEngine({ blocker: b, blockerType: () => 'x', lingerMs: 30 });
  e.setWorking(1);
  e.setWorking(0);
  ok(e.active(), 'engine: линджер — сразу после working=0 ещё активен');
  await sleep(60);
  ok(!e.active() && b.stops === 1, 'engine: линджер вышел — assertion снята');

  e.setWorking(1);
  e.setWorking(0);
  e.setWorking(1); // вернулся в работу внутри линджера
  await sleep(60);
  ok(e.active(), 'engine: working вернулся в линджер — грант жив');
  ok(b.starts.length === 2, 'engine: возврат в линджере не перезапускает блокер');
  e.dispose();
}

{
  // тумблер авто-режима
  const b = mkBlocker();
  const e = createEngine({ blocker: b, blockerType: () => 'x', lingerMs: 5 });
  e.setWorking(2);
  e.setAuto(false);
  ok(!e.active(), 'engine: setAuto(false) снимает auto-грант сразу, без линджера');
  e.setWorking(5);
  ok(!e.active(), 'engine: при выключенном авто working игнорируется');
  e.setAuto(true);
  e.setWorking(1);
  ok(e.active(), 'engine: авто включили обратно — триггер работает');
  e.dispose();
}

{
  // таймер: семантика caffeinate -t
  const b = mkBlocker();
  let ended = null;
  const e = createEngine({
    blocker: b, blockerType: () => 'x',
    onTimerEnd: (g) => { ended = g; },
  });
  e.startTimer(40, '40мс');
  ok(e.active(), 'engine: таймер взял assertion');
  const st = e.state();
  ok(st.manual && st.manual.kind === 'timer' && st.manual.until > Date.now(),
    'engine: state отдаёт kind=timer и until в будущем');
  await sleep(80);
  ok(!e.active(), 'engine: таймер вышел — assertion снята');
  ok(ended && ended.kind === 'timer', 'engine: onTimerEnd сработал');
  e.dispose();
}

{
  // ручной слот один: новый старт заменяет предыдущий (kill-then-start, как Coffee)
  const b = mkBlocker();
  const e = createEngine({ blocker: b, blockerType: () => 'x' });
  e.startTimer(5000, 'час');
  e.startManual();
  const st = e.state();
  ok(st.manual && st.manual.kind === 'manual', 'engine: manual заменил timer в слоте');
  ok(b.starts.length === 1 && b.stops === 0, 'engine: замена слота не дёргает блокер');
  await sleep(20); // старый таймер не должен выстрелить после замены
  ok(e.active(), 'engine: протухший таймер после замены не снимает assertion');
  e.dispose();
}

{
  // «пока жив процесс»: пульс kill(pid,0) через инжекцию
  const b = mkBlocker();
  let alive = true;
  let died = null;
  const e = createEngine({
    blocker: b, blockerType: () => 'x', pulseMs: 10,
    pidAlive: () => alive,
    onProcessDied: (g) => { died = g; },
  });
  e.startProcess(12345, 'Safari');
  ok(e.active(), 'engine: process-грант взял assertion');
  await sleep(25);
  ok(e.active(), 'engine: процесс жив — assertion держится');
  alive = false;
  await sleep(25);
  ok(!e.active(), 'engine: процесс умер — assertion снята');
  ok(died && died.pid === 12345 && died.label === 'Safari', 'engine: onProcessDied с грантом');
  e.dispose();
}

{
  // независимость auto и ручного слота
  const b = mkBlocker();
  const e = createEngine({ blocker: b, blockerType: () => 'x' });
  e.setWorking(1);
  e.startManual();
  e.stopManual();
  ok(e.active(), 'engine: stopManual не трогает auto-грант');
  e.setAuto(false);
  ok(!e.active(), 'engine: оба гранта сняты — assertion ушла');
  ok(b.stops === 1, 'engine: блокер остановлен ровно один раз');
  e.dispose();
}

{
  // id=0 от powerSaveBlocker — валидный id (ловушка truthiness)
  const b = mkBlocker();
  const e = createEngine({ blocker: b, blockerType: () => 'x' });
  e.startManual();
  ok(e.active(), 'engine: blocker id=0 считается активным');
  e.stopManual();
  ok(b.stops === 1, 'engine: blocker id=0 корректно останавливается');
  e.dispose();
}

{
  // смена типа блокера на лету (тумблер «не гасить экран»)
  let type = 'prevent-app-suspension';
  const b = mkBlocker();
  const e = createEngine({ blocker: b, blockerType: () => type });
  e.startManual();
  type = 'prevent-display-sleep';
  e.restartBlocker();
  ok(b.stops === 1 && b.starts.length === 2 && b.starts[1] === 'prevent-display-sleep',
    'engine: restartBlocker перезапускает с новым типом');
  e.restartBlocker(); // тип не менялся — но идемпотентность нам не нужна, просто не падает
  e.stopManual();
  ok(!e.active(), 'engine: после рестартов корректно гасится');
  e.dispose();
}

/* ---------------- clamshell: парсеры и решения ---------------- */

const core = await import('../src/plugins/clamshell/core.js');

{
  const ioregOpen = `+-o IOPMrootDomain  <class IOPMrootDomain>
  |   "AppleClamshellCausesSleep" = Yes
  |   "AppleClamshellState" = No
`;
  const ioregClosed = `  |   "AppleClamshellCausesSleep" = No
  |   "AppleClamshellState" = Yes`;
  const lid1 = core.parseClamshellState(ioregOpen);
  ok(lid1.present && lid1.closed === false && lid1.causesSleep === true,
    'clamshell: ioreg открытая крышка + сон от крышки');
  const lid2 = core.parseClamshellState(ioregClosed);
  ok(lid2.present && lid2.closed === true && lid2.causesSleep === false,
    'clamshell: ioreg закрытая крышка + сон отключён');
  const lid3 = core.parseClamshellState('что-то без ключей');
  ok(lid3.present === false, 'clamshell: нет ключей — крышки нет (десктоп)');
}

{
  ok(core.parseSleepDisabled(' SleepDisabled\t\t1\n standby 1') === true,
    'clamshell: SleepDisabled 1 → true');
  ok(core.parseSleepDisabled(' SleepDisabled\t\t0') === false,
    'clamshell: SleepDisabled 0 → false');
  ok(core.parseSleepDisabled('мусор') === null,
    'clamshell: нет строки → null');
}

{
  const batt = core.parseBattery(
    `Now drawing from 'Battery Power'\n -InternalBattery-0 (id=23396451)\t37%; discharging; 4:27 remaining present: true`);
  ok(batt.pct === 37 && batt.onBattery === true, 'clamshell: батарея 37% на батарее');
  const ac = core.parseBattery(
    `Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t95%; charging; 0:40 remaining present: true`);
  ok(ac.pct === 95 && ac.onBattery === false, 'clamshell: на проводе');
  ok(core.parseBattery('garbage').pct === null, 'clamshell: десктоп без батареи → pct null');
}

{
  const base = { workingAtSleep: 2, armed: false, externalDisplay: false, lastSuggestAt: 0, now: 10 * 60 * 60 * 1000, minGapMs: 60 * 60 * 1000 };
  ok(core.decideSuggest(base).suggest === true && core.decideSuggest(base).kind === 'arm',
    'clamshell: сон прервал работу → предложить closed-display');
  ok(core.decideSuggest({ ...base, externalDisplay: true }).kind === 'native',
    'clamshell: с внешним дисплеем — подсказка про родной clamshell');
  ok(core.decideSuggest({ ...base, workingAtSleep: 0 }).suggest === false,
    'clamshell: работы не было — молчим');
  ok(core.decideSuggest({ ...base, armed: true }).suggest === false,
    'clamshell: уже armed — молчим');
  ok(core.decideSuggest({ ...base, lastSuggestAt: base.now - 1000 }).suggest === false,
    'clamshell: подсказка была недавно — не спамим');
}

{
  const content = core.sudoersContent('se.chernyshev');
  ok(content.includes('se.chernyshev ALL=(root) NOPASSWD:')
    && content.includes('/usr/bin/pmset -a disablesleep 1')
    && content.includes('/usr/bin/pmset -a disablesleep 0'),
    'clamshell: sudoers разрешает ровно две команды pmset');
  let threw = false;
  try { core.sudoersContent('user name; ALL'); } catch { threw = true; }
  ok(threw, 'clamshell: кривое имя юзера в sudoers не пролазит');
}

/* ---------------- итог ---------------- */

if (failed) {
  console.error(`\n${failed} тест(ов) упало`);
  process.exit(1);
}
console.log('\nВсе тесты прошли');
