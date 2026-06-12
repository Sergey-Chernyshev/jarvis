/**
 * Хост плагинов: сканирует src/plugins/<id>/index.js, ведёт жизненный цикл.
 * Подключаемость = папка с index.js + тумблер plugins.<id>.enabled в
 * ~/.jarvis/settings.json (дефолт: включён). Плагин не может уронить демона —
 * каждый вызов обёрнут, упавший логируется и игнорируется.
 *
 * Контракт плагина (module.exports):
 *   id, name, defaults              — идентичность и дефолты настроек
 *   init(ctx) / dispose()           — включение/выключение (обязан прибрать)
 *   onSessions(list)?               — снапшот сессий при каждом изменении
 *   onPeerChanged(srcId)?           — другой плагин сообщил о смене состояния
 *   trayMenu()? → items | Promise   — секция в right-click меню трея
 *   badge()? → string               — символ в title трея
 *   status()? → object              — статус для панели
 *   cmd(name, args)? → result       — команды из панели
 *
 * ctx: settings.get()/set(patch) (scoped), sessions(), notify(),
 *      changed() (обновить трей/панель + оповестить соседей), peer(id), log().
 */

const fs = require('node:fs');
const path = require('node:path');

const plugins = new Map(); // id → { mod, active }
let services = null;       // { settingsStore, sessions, notify, changed }

function log(id, ...args) {
  console.log(`[jarvis:${id}]`, ...args);
}

function safe(id, fn, ...args) {
  if (typeof fn !== 'function') return undefined;
  try {
    return fn(...args);
  } catch (err) {
    console.error(`[jarvis:plugins] ${id}:`, err.message);
    return undefined;
  }
}

function scopedSettings(id, defaults) {
  return {
    get() {
      const all = services.settingsStore.load().plugins || {};
      return { ...defaults, ...(all[id] || {}) };
    },
    set(patch) {
      const all = services.settingsStore.load().plugins || {};
      services.settingsStore.save({
        plugins: { ...all, [id]: { ...(all[id] || {}), ...patch } },
      });
    },
  };
}

/** соседям: состояние srcId сменилось (для связок вроде clamshell↔keep-awake) */
function broadcast(srcId) {
  for (const [pid, p] of plugins) {
    if (pid !== srcId && p.active) safe(pid, p.mod.onPeerChanged, srcId);
  }
}

function mkCtx(id, mod) {
  return {
    settings: scopedSettings(id, mod.defaults || {}),
    sessions: () => services.sessions(),
    notify: (title, body, sessionId) => services.notify(title, body, sessionId),
    changed: () => { services.changed(); broadcast(id); },
    peer: (pid) => {
      const p = plugins.get(pid);
      return p && p.active ? p.mod : null;
    },
    log: (...args) => log(id, ...args),
  };
}

function activate(id) {
  const p = plugins.get(id);
  if (!p || p.active) return;
  safe(id, p.mod.init && p.mod.init.bind(p.mod), mkCtx(id, p.mod));
  p.active = true;
  log(id, 'включён');
}

function deactivate(id) {
  const p = plugins.get(id);
  if (!p || !p.active) return;
  safe(id, p.mod.dispose && p.mod.dispose.bind(p.mod));
  p.active = false;
  log(id, 'выключен');
}

function init(svc) {
  services = svc;
  let dirs = [];
  try { dirs = fs.readdirSync(__dirname, { withFileTypes: true }); } catch { return; }
  for (const d of dirs) {
    if (!d.isDirectory()) continue;
    const file = path.join(__dirname, d.name, 'index.js');
    if (!fs.existsSync(file)) continue;
    let mod;
    try { mod = require(file); } catch (err) {
      console.error(`[jarvis:plugins] ${d.name} не загрузился:`, err.message);
      continue;
    }
    if (!mod || !mod.id || typeof mod.init !== 'function') continue;
    plugins.set(mod.id, { mod, active: false });
    if (scopedSettings(mod.id, mod.defaults || {}).get().enabled !== false) activate(mod.id);
  }
}

function setEnabled(id, on) {
  const p = plugins.get(id);
  if (!p) return { ok: false, error: 'плагин не найден' };
  scopedSettings(id, p.mod.defaults || {}).set({ enabled: !!on });
  if (on) activate(id); else deactivate(id);
  services.changed();
  return { ok: true };
}

function onSessions(list) {
  for (const [id, p] of plugins) {
    if (p.active) safe(id, p.mod.onSessions && p.mod.onSessions.bind(p.mod), list);
  }
}

/** секции трея активных плагинов; асинхронные источники ограничены таймаутом */
async function trayMenus() {
  const out = [];
  for (const [id, p] of plugins) {
    if (!p.active || typeof p.mod.trayMenu !== 'function') continue;
    let items = safe(id, p.mod.trayMenu.bind(p.mod));
    if (items && typeof items.then === 'function') {
      items = await Promise.race([
        items.catch(() => []),
        new Promise((r) => setTimeout(() => r([]), 900)),
      ]);
    }
    if (Array.isArray(items) && items.length) {
      if (out.length) out.push({ type: 'separator' });
      out.push(...items);
    }
  }
  return out;
}

function badges() {
  let s = '';
  for (const [id, p] of plugins) {
    if (p.active) s += safe(id, p.mod.badge && p.mod.badge.bind(p.mod)) || '';
  }
  return s;
}

function statuses() {
  return [...plugins.entries()].map(([id, p]) => ({
    id,
    name: p.mod.name || id,
    enabled: p.active,
    status: p.active ? (safe(id, p.mod.status && p.mod.status.bind(p.mod)) || null) : null,
  }));
}

async function cmd(id, name, args) {
  if (name === '_enable') return setEnabled(id, !!(args && args.on));
  const p = plugins.get(id);
  if (!p || !p.active) return { ok: false, error: 'плагин выключен' };
  const res = await safe(id, p.mod.cmd && p.mod.cmd.bind(p.mod), name, args);
  return res || { ok: true };
}

function dispose() {
  for (const id of plugins.keys()) deactivate(id);
}

module.exports = { init, onSessions, trayMenus, badges, statuses, cmd, setEnabled, dispose };
