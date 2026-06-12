/** Настройки Jarvis: ~/.jarvis/settings.json. Битый файл → дефолты, молча. */

const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');

const FILE = path.join(os.homedir(), '.jarvis', 'settings.json');

const DEFAULTS = {
  hotkey: 'Command+J',
  notifyDone: true,
  notifyWaiting: true,
  position: 'center', // 'center' | 'corner'
  autoResume: true, // после сброса лимита сказать ждавшим сессиям «продолжай»
};

let cache = null;

function load() {
  if (cache) return cache;
  let data = {};
  try {
    const parsed = JSON.parse(fs.readFileSync(FILE, 'utf8'));
    if (parsed && typeof parsed === 'object') data = parsed;
  } catch {}
  cache = { ...DEFAULTS, ...data };
  return cache;
}

function save(patch) {
  cache = { ...load(), ...patch };
  try {
    fs.mkdirSync(path.dirname(FILE), { recursive: true });
    fs.writeFileSync(FILE, JSON.stringify(cache, null, 2) + '\n');
  } catch (err) {
    console.error('[jarvis] не смог записать настройки:', err.message);
  }
  return cache;
}

module.exports = { load, save, DEFAULTS };
