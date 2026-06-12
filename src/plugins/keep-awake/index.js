/**
 * Плагин «Не спать»: вето на idle-сон через power assertion.
 *
 * powerSaveBlocker — те же IOPMAssertions, что у caffeinate/Amphetamine:
 *   prevent-app-suspension → PreventUserIdleSystemSleep (экран может гаснуть);
 *   prevent-display-sleep  → PreventUserIdleDisplaySleep (экран горит).
 * Assertion живёт в процессе демона: краш = автоснятие, «застрявший» запрет
 * сна невозможен (в отличие от detached caffeinate у Raycast Coffee).
 *
 * Триггеры (UX-модель Amphetamine, подогнанная под сценарий Jarvis):
 *   авто     — пока есть working-сессии (наш «умный» триггер, дефолт);
 *   таймер   — пресеты 15м…8ч (caffeinate -t);
 *   процесс  — пока жив pid: claude-сессии Jarvis + GUI-приложения (caffeinate -w);
 *   бессрочно — пока не выключишь.
 * Проверка живьём: pmset -g assertions | grep -i electron
 */

const { powerSaveBlocker } = require('electron');
const { execFile } = require('node:child_process');
const { createEngine } = require('./engine');

const MIN = 60 * 1000;
const PRESETS = [15, 30, 60, 120, 240, 480]; // минуты

let ctx = null;
let engine = null;
let ticker = null; // обновление «ещё 47м» в трее/панели, пока идёт таймер

function countWorking(list) {
  return list.filter((s) => s.status === 'working').length;
}

function fmtLeft(until) {
  const m = Math.max(1, Math.round((until - Date.now()) / MIN));
  if (m < 60) return `${m}м`;
  const h = Math.floor(m / 60);
  const mm = m % 60;
  return mm ? `${h}ч ${mm}м` : `${h}ч`;
}

function presetLabel(min) {
  if (min < 60) return `${min} минут`;
  const h = min / 60;
  if (h === 1) return '1 час';
  return `${h} ${h < 5 ? 'часа' : 'часов'}`;
}

function statusLine(st) {
  if (!st.active) return null;
  const parts = [];
  if (st.auto) {
    parts.push(st.lingering ? 'агенты затихли — держу ещё минуту' : `агенты работают (${st.working})`);
  }
  if (st.manual) {
    if (st.manual.kind === 'timer') parts.push(`ещё ${fmtLeft(st.manual.until)}`);
    else if (st.manual.kind === 'process') parts.push(`пока жив ${st.manual.label}`);
    else parts.push('бессрочно');
  }
  return parts.join(' · ');
}

function syncTicker(st) {
  const needs = st.active && st.manual && st.manual.kind === 'timer';
  if (needs && !ticker) ticker = setInterval(() => ctx && ctx.changed(), 30 * 1000);
  if (!needs && ticker) { clearInterval(ticker); ticker = null; }
}

function osa(line) {
  return new Promise((resolve, reject) => {
    execFile('osascript', ['-e', line], { timeout: 1500 }, (err, out) => {
      if (err) reject(err);
      else resolve(String(out).trim());
    });
  });
}

/** кандидаты «пока жив процесс»: claude-сессии Jarvis + GUI-приложения.
 * GUI — два ОТДЕЛЬНЫХ AppleScript-вызова, как у Raycast Coffee: несколько
 * -e в одном osascript — это один скрипт, печатается только последний результат. */
async function listProcesses() {
  const own = [];
  const seen = new Set();
  for (const s of ctx.sessions()) {
    if (s.pid && !seen.has(s.pid)) {
      seen.add(s.pid);
      own.push({ pid: s.pid, label: `claude · ${s.project || '?'}` });
    }
  }
  const apps = [];
  try {
    const [idsLine, namesLine] = await Promise.all([
      osa('tell application "System Events" to get the unix id of every process whose background only is false'),
      osa('tell application "System Events" to get the name of every process whose background only is false'),
    ]);
    const ids = idsLine.split(',').map((x) => parseInt(x.trim(), 10));
    const names = namesLine.split(',').map((x) => x.trim());
    for (let i = 0; i < ids.length && i < names.length; i++) {
      const pid = ids[i];
      if (!Number.isInteger(pid) || pid === process.pid || seen.has(pid) || !names[i]) continue;
      apps.push({ pid, label: names[i] });
    }
    apps.sort((a, b) => a.label.localeCompare(b.label, 'ru'));
  } catch { /* нет пермишена Automation — покажем хотя бы claude-сессии */ }
  return [...own, ...apps];
}

module.exports = {
  id: 'keep-awake',
  name: 'Не спать',
  // как у Caffeine/Amphetamine: ничего не делает, пока сам не включишь;
  // авто-триггер «пока агенты работают» — опция, не дефолт
  defaults: { enabled: true, auto: false, keepDisplayOn: false },

  init(c) {
    ctx = c;
    const s = ctx.settings.get();
    engine = createEngine({
      blocker: {
        start: (type) => powerSaveBlocker.start(type),
        stop: (id) => { try { powerSaveBlocker.stop(id); } catch {} },
      },
      blockerType: () => (ctx.settings.get().keepDisplayOn
        ? 'prevent-display-sleep'
        : 'prevent-app-suspension'),
      autoEnabled: s.auto,
      onChange: (st) => { syncTicker(st); ctx.changed(); },
      onTimerEnd: () => ctx.notify('☕ Таймер вышел', 'Мак снова может спать как обычно'),
      onProcessDied: (g) => ctx.notify('☕ Снимаю запрет сна', `${g.label} завершился`),
    });
    // демон мог рестартовать посреди работы — подхватываем текущее состояние
    engine.setWorking(countWorking(ctx.sessions()));
  },

  dispose() {
    if (ticker) { clearInterval(ticker); ticker = null; }
    if (engine) { engine.dispose(); engine = null; }
    ctx = null;
  },

  onSessions(list) {
    if (engine) engine.setWorking(countWorking(list));
  },

  badge() {
    return engine && engine.active() ? '☕' : '';
  },

  status() {
    if (!engine) return null;
    const st = engine.state();
    return { ...st, line: statusLine(st), keepDisplayOn: ctx.settings.get().keepDisplayOn };
  },

  cmd(name, args = {}) {
    if (!engine) return { ok: false, error: 'плагин выключен' };
    switch (name) {
      case 'start-manual':
        engine.startManual();
        break;
      case 'start-timer': {
        const minutes = Math.max(1, Math.floor(Number(args.minutes) || 0));
        engine.startTimer(minutes * MIN, `${minutes}м`);
        break;
      }
      case 'start-process': {
        const pid = Math.floor(Number(args.pid) || 0);
        if (pid <= 0) return { ok: false, error: 'кривой pid' };
        engine.startProcess(pid, String(args.label || pid));
        break;
      }
      case 'stop':
        engine.stopManual();
        break;
      case 'set': {
        const patch = {};
        if (typeof args.auto === 'boolean') patch.auto = args.auto;
        if (typeof args.keepDisplayOn === 'boolean') patch.keepDisplayOn = args.keepDisplayOn;
        if (!Object.keys(patch).length) return { ok: false, error: 'пустой set' };
        ctx.settings.set(patch);
        if ('auto' in patch) engine.setAuto(patch.auto);
        if ('keepDisplayOn' in patch) engine.restartBlocker();
        break;
      }
      default:
        return { ok: false, error: `неизвестная команда: ${name}` };
    }
    ctx.changed();
    return { ok: true };
  },

  async trayMenu() {
    const st = engine.state();
    const s = ctx.settings.get();
    const line = statusLine(st);
    const procs = await listProcesses().catch(() => []);
    return [
      { label: line ? `☕ Не спать: ${line}` : '☕ Не спать: выкл', enabled: false },
      { label: 'Бессрочно', click: () => this.cmd('start-manual') },
      {
        label: 'На время',
        submenu: PRESETS.map((m) => ({
          label: presetLabel(m),
          click: () => this.cmd('start-timer', { minutes: m }),
        })),
      },
      {
        label: 'Пока жив процесс',
        submenu: procs.length
          ? procs.slice(0, 24).map((p) => ({
            label: p.label,
            click: () => this.cmd('start-process', p),
          }))
          : [{ label: 'процессы не нашлись', enabled: false }],
      },
      ...(st.manual ? [{ label: 'Выключить ручной режим', click: () => this.cmd('stop') }] : []),
      { type: 'separator' },
      {
        label: 'Пока агенты работают (авто)',
        type: 'checkbox',
        checked: !!s.auto,
        click: () => this.cmd('set', { auto: !s.auto }),
      },
      {
        label: 'Не гасить экран',
        type: 'checkbox',
        checked: !!s.keepDisplayOn,
        click: () => this.cmd('set', { keepDisplayOn: !s.keepDisplayOn }),
      },
    ];
  },
};
