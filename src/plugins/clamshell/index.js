/**
 * Плагин «Крышка»: closed-display mode — мак работает с закрытой крышкой.
 *
 * Механика: `pmset -a disablesleep 1` — запрет на уровне IOPMrootDomain,
 * выше категорий idle/forced (программный родственник пути Amphetamine).
 * Это root-уровень и термо-риски, поэтому политика плагина:
 * ДЕТЕКТИТЬ И ПОДСКАЗЫВАТЬ, а не молча sudo. Тихое переключение — только
 * после явного опт-ина: установки /etc/sudoers.d/jarvis-pmset (ровно две
 * команды pmset, см. core.sudoersContent).
 *
 * Fail-safe (урок Amphetamine Enhancer — «мак не должен зажариться в рюкзаке»):
 *   1) маркер ~/.jarvis/clamshell.json: кто и когда поднял флаг;
 *   2) на старте демона: флаг стоит, маркер наш → демон умирал — восстановить;
 *   3) dispose/квит → восстановить;
 *   4) батарейный сторож: armed + батарея ≤ floor → тихий сброс, нельзя
 *      тихо → pmset sleepnow (форс-сон без root: лучше уснуть, чем зажариться;
 *      admin-диалог под закрытой крышкой никто не увидит — его не зовём).
 */

const { powerMonitor, screen } = require('electron');
const { execFile, execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const core = require('./core');

const SUDOERS = '/etc/sudoers.d/jarvis-pmset';
const MARKER = path.join(os.homedir(), '.jarvis', 'clamshell.json');
const SUGGEST_GAP = 60 * 60 * 1000; // подсказка не чаще раза в час
const GUARD_MS = 60 * 1000;

let ctx = null;
let armed = false;
let armedBy = null;       // 'manual' | 'auto'
let busy = false;         // arm/disarm в полёте — не наслаиваем
let guard = null;         // батарейный сторож
let workingAtSleep = 0;   // сколько working было в момент засыпания
let lastSuggestAt = 0;
let isAir = false;        // безвентиляторный — предупреждаем о троттлинге
let onSuspend = null;
let onResume = null;

function sudoersInstalled() {
  try { return fs.existsSync(SUDOERS); } catch { return false; }
}

/** тихий путь: sudo -n работает только с установленным sudoers-правилом */
function pmsetQuiet(on) {
  return new Promise((resolve) => {
    execFile('sudo', ['-n', '/usr/bin/pmset', '-a', 'disablesleep', on ? '1' : '0'],
      { timeout: 8000 }, (err) => resolve(!err));
  });
}

/** честный путь: сначала тихо, не вышло — admin-диалог (юзер видит и решает) */
async function pmsetAsk(on) {
  if (await pmsetQuiet(on)) return true;
  const script = `do shell script "/usr/bin/pmset -a disablesleep ${on ? 1 : 0}"`
    + ` with administrator privileges`
    + ` with prompt "Jarvis ${on ? 'включает' : 'выключает'} closed-display mode"`;
  return new Promise((resolve) => {
    execFile('osascript', ['-e', script], { timeout: 120000 }, (err) => resolve(!err));
  });
}

function writeMarker(by) {
  try {
    fs.mkdirSync(path.dirname(MARKER), { recursive: true });
    fs.writeFileSync(MARKER, JSON.stringify({ pid: process.pid, by, at: Date.now() }) + '\n');
  } catch {}
}

function readMarker() {
  try { return JSON.parse(fs.readFileSync(MARKER, 'utf8')); } catch { return null; }
}

function clearMarker() {
  try { fs.unlinkSync(MARKER); } catch {}
}

function readSleepDisabled() {
  return new Promise((resolve) => {
    execFile('pmset', ['-g'], { timeout: 4000 }, (err, out) =>
      resolve(err ? null : core.parseSleepDisabled(out)));
  });
}

function readBattery() {
  return new Promise((resolve) => {
    execFile('pmset', ['-g', 'batt'], { timeout: 4000 }, (err, out) =>
      resolve(err ? { pct: null, onBattery: null, charging: null } : core.parseBattery(out)));
  });
}

function readLid() {
  return new Promise((resolve) => {
    execFile('ioreg', ['-r', '-k', 'AppleClamshellState', '-d', '4'], { timeout: 4000 },
      (err, out) => resolve(err ? { present: false, closed: null, causesSleep: null }
        : core.parseClamshellState(out)));
  });
}

function externalDisplayPresent() {
  try { return screen.getAllDisplays().some((d) => d.internal === false); } catch { return false; }
}

function startGuard() {
  if (guard) return;
  guard = setInterval(async () => {
    if (!armed) return;
    const floor = ctx.settings.get().batteryFloor;
    const batt = await readBattery();
    if (batt.onBattery !== true || batt.pct == null || batt.pct > floor) return;
    ctx.log(`батарея ${batt.pct}% ≤ ${floor}% — снимаю disablesleep`);
    if (await pmsetQuiet(false)) {
      armed = false; armedBy = null;
      clearMarker();
      stopGuard();
      ctx.notify('⌒ Крышка: батарея садится', `Осталось ${batt.pct}% — вернул нормальный сон`);
      ctx.changed();
    } else {
      // тихо не получилось, диалог под закрытой крышкой бессмыслен —
      // форс-сон (root не нужен) спасает батарею и температуру
      ctx.notify('⌒ Крышка: батарея садится', `Осталось ${batt.pct}% — усыпляю мак`);
      execFile('pmset', ['sleepnow'], { timeout: 4000 }, () => {});
    }
  }, GUARD_MS);
}

function stopGuard() {
  if (guard) { clearInterval(guard); guard = null; }
}

async function arm(by) {
  if (busy) return { ok: false, error: 'операция уже идёт' };
  if (armed) return { ok: true };
  busy = true;
  try {
    const ok = by === 'auto' ? await pmsetQuiet(true) : await pmsetAsk(true);
    if (!ok) return { ok: false, error: 'не получилось включить (пароль отменён?)' };
    armed = true;
    armedBy = by;
    writeMarker(by);
    startGuard();
    if (by === 'manual' && !sudoersInstalled()) {
      ctx.notify('⌒ Closed-display включён', 'Не забудь выключить: без тихого режима я не смогу снять его сам');
    }
    ctx.changed();
    return { ok: true };
  } finally { busy = false; }
}

async function disarm() {
  if (busy) return { ok: false, error: 'операция уже идёт' };
  if (!armed) return { ok: true };
  busy = true;
  try {
    const ok = await pmsetAsk(false);
    if (!ok) return { ok: false, error: 'не получилось выключить' };
    armed = false;
    armedBy = null;
    clearMarker();
    stopGuard();
    ctx.changed();
    return { ok: true };
  } finally { busy = false; }
}

/** подвисший с прошлой жизни демона disablesleep — вернуть как было */
async function restoreAfterRestart() {
  const marker = readMarker();
  if (!marker) return;
  if (await readSleepDisabled() !== true) { clearMarker(); return; }
  if (await pmsetQuiet(false)) {
    clearMarker();
    ctx.log('демон перезапустился с поднятым disablesleep — восстановил нормальный сон');
  } else {
    ctx.notify('⌒ Мак не спит с прошлого запуска',
      'Остался closed-display mode — выключи в меню ◇ → Крышка (спросит пароль)');
  }
}

async function installSudoers() {
  const user = os.userInfo().username;
  const content = core.sudoersContent(user); // бросит на странном имени
  const tmp = path.join(os.homedir(), '.jarvis', 'sudoers-pmset');
  fs.mkdirSync(path.dirname(tmp), { recursive: true });
  fs.writeFileSync(tmp, content);
  // visudo -c валидирует ДО установки; всё одним admin-скриптом = один пароль
  const script = `do shell script "/usr/sbin/visudo -c -q -f '${tmp}'`
    + ` && /usr/bin/install -m 0440 -o root -g wheel '${tmp}' '${SUDOERS}'"`
    + ` with administrator privileges`
    + ` with prompt "Jarvis настраивает тихое переключение closed-display mode"`;
  return new Promise((resolve) => {
    execFile('osascript', ['-e', script], { timeout: 120000 }, (err) => {
      try { fs.unlinkSync(tmp); } catch {}
      if (err) return resolve({ ok: false, error: 'установка отменена' });
      ctx.notify('⌒ Тихий режим настроен', 'Теперь closed-display переключается без пароля');
      ctx.changed();
      resolve({ ok: true });
    });
  });
}

module.exports = {
  id: 'clamshell',
  name: 'Крышка',
  defaults: { enabled: true, suggest: true, autoArm: false, batteryFloor: 15 },

  init(c) {
    ctx = c;
    execFile('sysctl', ['-n', 'hw.model'], { timeout: 3000 }, (err, out) => {
      isAir = !err && /MacBookAir/i.test(String(out));
    });
    restoreAfterRestart().catch(() => {});

    onSuspend = () => {
      workingAtSleep = ctx.sessions().filter((s) => s.status === 'working').length;
    };
    onResume = async () => {
      if (!ctx.settings.get().suggest) return;
      const dec = core.decideSuggest({
        workingAtSleep,
        armed,
        externalDisplay: externalDisplayPresent(),
        lastSuggestAt,
        now: Date.now(),
        minGapMs: SUGGEST_GAP,
      });
      if (!dec.suggest) return;
      lastSuggestAt = Date.now();
      const n = workingAtSleep;
      const head = `Сон прервал ${n} ${n === 1 ? 'работающую сессию' : 'работающие сессии'}`;
      if (dec.kind === 'native') {
        ctx.notify(head, 'Есть внешний дисплей: держи мак на питании — родной clamshell-режим не даст ему уснуть с закрытой крышкой');
      } else {
        ctx.notify(head, 'Включи closed-display mode (меню ◇ → Крышка), чтобы мак не засыпал под крышкой'
          + (isAir ? '. Air без вентилятора — под крышкой возможен троттлинг' : ''));
      }
    };
    powerMonitor.on('suspend', onSuspend);
    powerMonitor.on('resume', onResume);
  },

  dispose() {
    stopGuard();
    if (onSuspend) { powerMonitor.removeListener('suspend', onSuspend); onSuspend = null; }
    if (onResume) { powerMonitor.removeListener('resume', onResume); onResume = null; }
    if (armed) {
      // квит не ждёт промисов — восстанавливаем синхронно и только тихо;
      // без sudoers ручной armed переживёт квит, его поднимет restoreAfterRestart
      try {
        execFileSync('sudo', ['-n', '/usr/bin/pmset', '-a', 'disablesleep', '0'], { timeout: 4000 });
        armed = false;
        armedBy = null;
        clearMarker();
      } catch {}
    }
    ctx = null;
  },

  /** связка с keep-awake: авто-режим повторяет его assertion (нужен sudoers) */
  onPeerChanged(srcId) {
    if (srcId !== 'keep-awake' || busy) return;
    const s = ctx.settings.get();
    if (!s.autoArm || !sudoersInstalled()) return;
    const ka = ctx.peer('keep-awake');
    const active = !!(ka && typeof ka.status === 'function' && ka.status() && ka.status().active);
    if (active && !armed) arm('auto').catch(() => {});
    else if (!active && armed && armedBy === 'auto') disarm().catch(() => {});
  },

  badge() {
    return armed ? '⌒' : '';
  },

  status() {
    return {
      armed,
      armedBy,
      autoArm: ctx.settings.get().autoArm,
      suggest: ctx.settings.get().suggest,
      batteryFloor: ctx.settings.get().batteryFloor,
      sudoers: sudoersInstalled(),
    };
  },

  async cmd(name, args = {}) {
    switch (name) {
      case 'arm': return arm('manual');
      case 'disarm': return disarm();
      case 'install-sudoers': return installSudoers();
      case 'set': {
        const patch = {};
        if (typeof args.autoArm === 'boolean') patch.autoArm = args.autoArm;
        if (typeof args.suggest === 'boolean') patch.suggest = args.suggest;
        if (Number.isFinite(args.batteryFloor)) {
          patch.batteryFloor = Math.min(80, Math.max(5, Math.floor(args.batteryFloor)));
        }
        if (!Object.keys(patch).length) return { ok: false, error: 'пустой set' };
        ctx.settings.set(patch);
        // авто включили — сразу синхронизируемся с keep-awake
        if (patch.autoArm) this.onPeerChanged('keep-awake');
        ctx.changed();
        return { ok: true };
      }
      default:
        return { ok: false, error: `неизвестная команда: ${name}` };
    }
  },

  async trayMenu() {
    const s = ctx.settings.get();
    const sudoers = sudoersInstalled();
    const lid = await readLid();
    const statusLabel = armed
      ? '⌒ Крышка: мак не уснёт даже закрытой'
      : lid.causesSleep === false
        ? '⌒ Крышка: закрытие сейчас не усыпляет'
        : '⌒ Крышка: закроешь — уснёт';
    return [
      { label: statusLabel, enabled: false },
      {
        label: 'Closed-display mode',
        type: 'checkbox',
        checked: armed,
        click: () => (armed ? disarm() : arm('manual')).catch(() => {}),
      },
      {
        label: sudoers ? 'Авто при работе агентов' : 'Авто при работе агентов (нужен тихий режим)',
        type: 'checkbox',
        checked: !!s.autoArm,
        enabled: sudoers,
        click: () => this.cmd('set', { autoArm: !s.autoArm }),
      },
      {
        label: 'Подсказывать после прерванного сна',
        type: 'checkbox',
        checked: !!s.suggest,
        click: () => this.cmd('set', { suggest: !s.suggest }),
      },
      ...(sudoers ? [] : [{
        label: 'Настроить тихий режим (sudoers)…',
        click: () => installSudoers().catch(() => {}),
      }]),
    ];
  },
};
