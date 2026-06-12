#!/usr/bin/env node
/**
 * Установщик интеграции Jarvis ⇄ Claude Code.
 *
 *   node scripts/setup.mjs install     — вшить хуки в ~/.claude/settings.json
 *   node scripts/setup.mjs uninstall   — вычистить свои записи
 *   node scripts/setup.mjs status      — показать, что установлено
 *
 * Принципы: merge, не overwrite; идемпотентно; бэкап перед записью;
 * атомарная запись (tmp + rename); битый JSON не трогаем.
 */

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execSync, execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { mergeBlock, removeBlock, hasBlock } from './rcblock.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const HOME = os.homedir();
const JARVIS_DIR = path.join(HOME, '.jarvis');
const HOOK_SRC = path.join(__dirname, '..', 'bin', 'jarvis-hook');
const HOOK_DST = path.join(JARVIS_DIR, 'bin', 'jarvis-hook');
const SHIM_SRC = path.join(__dirname, '..', 'bin', 'claude-shim');
const SHIMS_DIR = path.join(JARVIS_DIR, 'shims');
const SHIM_DST = path.join(SHIMS_DIR, 'claude');
const TMUX_CONF_SRC = path.join(__dirname, '..', 'bin', 'jarvis-tmux.conf');
const TMUX_CONF_DST = path.join(JARVIS_DIR, 'tmux.conf');
const CLAUDE_DIR = path.join(HOME, '.claude');
const SETTINGS = path.join(CLAUDE_DIR, 'settings.json');

// Признак "это наша запись" — путь шима в команде.
// Ловит и абсолютный путь, и вариант с $HOME.
const MARKER = '.jarvis/bin/jarvis-hook';

// Событие Claude Code → аргумент шима
const EVENTS = [
  ['SessionStart', 'session-start'],
  ['UserPromptSubmit', 'prompt'],
  ['PreToolUse', 'pre-tool'],
  ['PostToolUse', 'post-tool'],
  ['Notification', 'notification'],
  ['Stop', 'stop'],
  ['StopFailure', 'stop-failure'],
  ['SessionEnd', 'session-end'],
];

/* ---------------- helpers ---------------- */

function readSettings() {
  if (!fs.existsSync(SETTINGS)) return { exists: false, json: {} };
  const raw = fs.readFileSync(SETTINGS, 'utf8');
  if (!raw.trim()) return { exists: true, json: {} };
  try {
    return { exists: true, json: JSON.parse(raw) };
  } catch {
    console.error(`✗ ${SETTINGS} содержит невалидный JSON — не трогаю.`);
    console.error('  Почини файл вручную и запусти setup ещё раз.');
    process.exit(1);
  }
}

function atomicWrite(file, content) {
  const tmp = path.join(path.dirname(file), `.${path.basename(file)}.tmp-${process.pid}`);
  fs.writeFileSync(tmp, content, 'utf8');
  fs.renameSync(tmp, file);
}

function backup(file) {
  if (!fs.existsSync(file)) return null;
  const dst = `${file}.bak-${new Date().toISOString().replace(/[:.]/g, '-')}`;
  fs.copyFileSync(file, dst);
  return dst;
}

function isOurs(hook) {
  return hook && typeof hook.command === 'string' && hook.command.includes(MARKER);
}

function groupHasOurs(group) {
  return Array.isArray(group?.hooks) && group.hooks.some(isOurs);
}

function eventInstalled(json, event) {
  const arr = json?.hooks?.[event];
  return Array.isArray(arr) && arr.some(groupHasOurs);
}

function claudeFound() {
  try {
    execSync('command -v claude', { stdio: 'ignore', shell: '/bin/sh' });
    return true;
  } catch {
    return false;
  }
}

function tmuxFound() {
  try {
    execFileSync('tmux', ['-V'], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
}

/** rc-файлы, которые правим: zsh всегда, bash — если он login shell */
function rcFiles() {
  const files = [path.join(HOME, '.zshrc')];
  if ((process.env.SHELL || '').endsWith('bash')) {
    files.push(path.join(HOME, '.bashrc'), path.join(HOME, '.bash_profile'));
  }
  return files;
}

function liveTmuxSessions() {
  try {
    const out = execFileSync('tmux', ['-L', 'jarvis', 'list-sessions', '-F', '#{session_name}'], {
      stdio: ['ignore', 'pipe', 'ignore'], encoding: 'utf8',
    }).trim();
    return out ? out.split('\n') : [];
  } catch {
    return [];
  }
}

/* ---------------- commands ---------------- */

function install() {
  // 1. Шим в ~/.jarvis/bin
  fs.mkdirSync(path.dirname(HOOK_DST), { recursive: true });
  fs.copyFileSync(HOOK_SRC, HOOK_DST);
  fs.chmodSync(HOOK_DST, 0o755);
  console.log(`✓ Шим установлен: ${HOOK_DST}`);

  if (!claudeFound()) {
    console.warn('⚠ Бинарь `claude` не найден в PATH — хуки всё равно пропишу,');
    console.warn('  они подхватятся, когда Claude Code появится.');
  }

  // 2. Merge в settings.json
  const { exists, json } = readSettings();
  const bak = exists ? backup(SETTINGS) : null;
  if (bak) console.log(`✓ Бэкап: ${bak}`);

  fs.mkdirSync(CLAUDE_DIR, { recursive: true });
  json.hooks ||= {};

  const added = [];
  const present = [];
  for (const [event, arg] of EVENTS) {
    if (eventInstalled(json, event)) {
      present.push(event);
      continue;
    }
    json.hooks[event] ||= [];
    json.hooks[event].push({
      hooks: [{ type: 'command', command: `${HOOK_DST} claude ${arg}`, timeout: 5 }],
    });
    added.push(event);
  }

  if (added.length) {
    atomicWrite(SETTINGS, JSON.stringify(json, null, 2) + '\n');
    console.log(`✓ Добавлены хуки: ${added.join(', ')}`);
  }
  if (present.length) console.log(`• Уже стояли: ${present.join(', ')}`);

  // 3. tmux-транспорт: шим claude + конфиг + PATH-блок в rc-файлах
  installTmuxTransport();

  console.log('\nГотово. Активные сессии Claude Code нужно перезапустить —');
  console.log('хуки снимаются снапшотом на старте сессии.');
  console.log('Если Claude Code попросит подтвердить изменённые хуки (/hooks) — это наша запись.');
  console.log('Чтобы шим подхватился в текущем шелле: exec zsh (или новая вкладка).');
}

function installTmuxTransport() {
  if (!tmuxFound()) {
    console.warn('⚠ tmux не найден — транспорт ввода пропускаю.');
    console.warn('  Поставь: brew install tmux — и запусти npm run setup ещё раз.');
    console.warn('  Уведомления и панель работают и без него.');
    return;
  }

  // Шим claude (паттерн pyenv)
  fs.mkdirSync(SHIMS_DIR, { recursive: true });
  fs.copyFileSync(SHIM_SRC, SHIM_DST);
  fs.chmodSync(SHIM_DST, 0o755);
  console.log(`✓ Шим claude: ${SHIM_DST}`);

  // Конфиг отдельного tmux-сервера
  fs.copyFileSync(TMUX_CONF_SRC, TMUX_CONF_DST);
  console.log(`✓ tmux-конфиг: ${TMUX_CONF_DST}`);

  // Managed-блок PATH в rc-файлах
  for (const rc of rcFiles()) {
    const existed = fs.existsSync(rc);
    const content = existed ? fs.readFileSync(rc, 'utf8') : '';
    const merged = mergeBlock(content, SHIMS_DIR);
    if (merged !== content) {
      if (existed) {
        const bak = backup(rc);
        if (bak) console.log(`✓ Бэкап: ${bak}`);
      }
      atomicWrite(rc, merged);
      console.log(`✓ PATH-блок в ${rc}`);
    } else {
      console.log(`• PATH-блок уже стоит в ${rc}`);
    }
  }
}

function uninstall() {
  const { exists, json } = readSettings();
  if (!exists || !json.hooks) {
    console.log('• Записей Jarvis в settings.json нет.');
  } else {
    const bak = backup(SETTINGS);
    if (bak) console.log(`✓ Бэкап: ${bak}`);

    const removed = [];
    for (const event of Object.keys(json.hooks)) {
      const arr = json.hooks[event];
      if (!Array.isArray(arr)) continue;
      const before = arr.length;

      // Выкидываем наши команды из групп, потом пустые группы
      for (const group of arr) {
        if (Array.isArray(group?.hooks)) {
          group.hooks = group.hooks.filter((h) => !isOurs(h));
        }
      }
      json.hooks[event] = arr.filter((g) => Array.isArray(g?.hooks) && g.hooks.length > 0);

      if (json.hooks[event].length !== before) removed.push(event);
      if (json.hooks[event].length === 0) delete json.hooks[event];
    }
    if (Object.keys(json.hooks).length === 0) delete json.hooks;

    atomicWrite(SETTINGS, JSON.stringify(json, null, 2) + '\n');
    console.log(removed.length ? `✓ Удалены хуки: ${removed.join(', ')}` : '• Наших хуков не нашлось.');
  }

  for (const f of [HOOK_DST, path.join(JARVIS_DIR, 'run.sock'), SHIM_DST, TMUX_CONF_DST]) {
    try {
      fs.unlinkSync(f);
      console.log(`✓ Удалён: ${f}`);
    } catch {}
  }
  try { fs.rmdirSync(SHIMS_DIR); } catch {}

  // Де-мёрж PATH-блока из rc-файлов
  for (const rc of rcFiles()) {
    if (!fs.existsSync(rc)) continue;
    const content = fs.readFileSync(rc, 'utf8');
    const cleaned = removeBlock(content);
    if (cleaned !== content) {
      const bak = backup(rc);
      if (bak) console.log(`✓ Бэкап: ${bak}`);
      atomicWrite(rc, cleaned);
      console.log(`✓ PATH-блок убран из ${rc}`);
    }
  }

  const live = liveTmuxSessions();
  if (live.length) {
    console.warn(`⚠ Живые tmux-сессии Jarvis не тронуты: ${live.join(', ')}`);
    console.warn('  Подключиться: tmux -L jarvis attach -t <имя>; убить все: tmux -L jarvis kill-server');
  }
}

function status() {
  console.log(`Шим:      ${fs.existsSync(HOOK_DST) ? '✓ ' + HOOK_DST : '✗ не установлен'}`);
  console.log(`Сокет:    ${fs.existsSync(path.join(JARVIS_DIR, 'run.sock')) ? '✓ демон, похоже, запущен' : '✗ демон не запущен'}`);
  console.log(`claude:   ${claudeFound() ? '✓ найден в PATH' : '✗ не найден'}`);

  console.log('tmux-транспорт:');
  console.log(`  ${tmuxFound() ? '✓ tmux в PATH' : '✗ tmux не установлен (brew install tmux)'}`);
  console.log(`  ${fs.existsSync(SHIM_DST) ? '✓' : '✗'} шим claude (${SHIM_DST})`);
  console.log(`  ${fs.existsSync(TMUX_CONF_DST) ? '✓' : '✗'} конфиг (${TMUX_CONF_DST})`);
  for (const rc of rcFiles()) {
    const ok = fs.existsSync(rc) && hasBlock(fs.readFileSync(rc, 'utf8'));
    console.log(`  ${ok ? '✓' : '✗'} PATH-блок в ${rc}`);
  }
  const live = liveTmuxSessions();
  if (live.length) console.log(`  • живые сессии: ${live.join(', ')}`);
  const { exists, json } = readSettings();
  if (!exists) {
    console.log(`Settings: ✗ ${SETTINGS} не существует`);
    return;
  }
  console.log(`Settings: ${SETTINGS}`);
  for (const [event] of EVENTS) {
    console.log(`  ${eventInstalled(json, event) ? '✓' : '✗'} ${event}`);
  }
}

/* ---------------- main ---------------- */

const cmd = process.argv[2];
if (cmd === 'install') install();
else if (cmd === 'uninstall') uninstall();
else if (cmd === 'status') status();
else {
  console.log('Использование: node scripts/setup.mjs <install|uninstall|status>');
  process.exit(1);
}
