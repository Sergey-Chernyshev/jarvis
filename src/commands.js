/**
 * Каталог слэш-команд Claude Code для палитры в панели.
 *
 * Источники (все без затрат квоты):
 *  - встроенные — статическая карта (машинных описаний для них нет);
 *  - кастомные .md в ~/.claude/commands и <cwd>/.claude/commands (frontmatter);
 *  - плагинные .md в ~/.claude/plugins/cache (имя namespace'ится по плагину).
 *
 * Авторитетный per-session список лежит в init-сообщении (поле slash_commands),
 * но его добыча стоит вызова claude -p — поэтому имена оттуда подмешиваются
 * лениво (enrichFromInit), когда resume-канал и так дёргает claude.
 */

const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');

const HOME = os.homedir();
const USER_CMDS = path.join(HOME, '.claude', 'commands');
const PLUGINS = path.join(HOME, '.claude', 'plugins', 'cache');

// Встроенные команды, полезные при управлении живой сессией.
// model/effort с подсказкой — Jarvis открывает свой пикер значений вместо
// интерактивного слайдера TUI (см. палитру в renderer).
const BUILTINS = [
  ['model', 'Сменить модель сессии', '‹выбрать›'],
  ['effort', 'Уровень рассуждения', '‹выбрать›'],
  ['compact', 'Сжать историю разговора, освободив контекст'],
  ['context', 'Показать, чем занят контекст (токены по компонентам)'],
  ['clear', 'Очистить историю и начать заново'],
  ['usage', 'Показать использование плана и лимиты'],
  ['cost', 'Показать стоимость текущей сессии'],
  ['init', 'Создать CLAUDE.md с описанием проекта'],
  ['review', 'Code-review текущих изменений'],
  ['security-review', 'Проверить изменения на уязвимости'],
  ['agents', 'Управление субагентами'],
  ['memory', 'Редактировать файлы памяти (CLAUDE.md)'],
  ['resume', 'Возобновить прошлую сессию'],
  ['export', 'Экспортировать разговор'],
  ['doctor', 'Диагностика установки Claude Code'],
  ['help', 'Список команд'],
].map(([name, description, hint]) => ({ name, description, hint: hint || '', source: 'builtin' }));

/* ---------- парсинг frontmatter команды ---------- */

function parseFrontmatter(text) {
  if (!text.startsWith('---')) return {};
  const end = text.indexOf('\n---', 3);
  if (end === -1) return {};
  const out = {};
  for (const line of text.slice(3, end).split('\n')) {
    const m = line.match(/^([a-zA-Z0-9_-]+):\s*(.*)$/);
    if (m) out[m[1]] = m[2].replace(/^["']|["']$/g, '').trim();
  }
  return out;
}

function readCommandFile(file, name, source) {
  try {
    const fm = parseFrontmatter(fs.readFileSync(file, 'utf8'));
    return {
      name,
      description: (fm.description || '').slice(0, 200),
      hint: (fm['argument-hint'] || fm.argumentHint || '').slice(0, 80),
      source,
    };
  } catch {
    return { name, description: '', hint: '', source };
  }
}

/** рекурсивный обход каталога команд: подкаталоги → namespace через ':' */
function scanDir(dir, source, prefix = '') {
  let entries;
  try { entries = fs.readdirSync(dir, { withFileTypes: true }); } catch { return []; }
  const out = [];
  for (const e of entries) {
    const full = path.join(dir, e.name);
    if (e.isDirectory()) {
      out.push(...scanDir(full, source, `${prefix}${e.name}:`));
    } else if (e.name.endsWith('.md')) {
      out.push(readCommandFile(full, prefix + e.name.slice(0, -3), source));
    }
  }
  return out;
}

/** плагинные команды: <cache>/<repo>/<plugin>/<version>/commands/<cmd>.md → plugin:cmd */
function scanPlugins() {
  const out = [];
  let repos;
  try { repos = fs.readdirSync(PLUGINS, { withFileTypes: true }); } catch { return out; }
  for (const repo of repos) {
    if (!repo.isDirectory()) continue;
    let plugins;
    try { plugins = fs.readdirSync(path.join(PLUGINS, repo.name), { withFileTypes: true }); } catch { continue; }
    for (const plugin of plugins) {
      if (!plugin.isDirectory()) continue;
      // версия может быть, а может и не быть — ищем каталог commands на 1–2 уровня вниз
      for (const sub of [plugin.name, '']) {
        const base = path.join(PLUGINS, repo.name, plugin.name);
        let versions;
        try { versions = fs.readdirSync(base, { withFileTypes: true }); } catch { continue; }
        for (const v of versions) {
          if (!v.isDirectory()) continue;
          const cmds = path.join(base, v.name, 'commands');
          if (fs.existsSync(cmds)) {
            for (const c of scanDir(cmds, 'plugin')) {
              out.push({ ...c, name: `${plugin.name}:${c.name}` });
            }
          }
        }
        break; // одного прохода достаточно
      }
    }
  }
  return out;
}

/* ---------- сборка и кэш ---------- */

let pluginCache = null; // плагины меняются редко — кэшируем на процесс
const cwdCache = new Map(); // cwd → { list, at }
const extraNames = new Set(); // имена из init, которых нет в файлах

function dedupe(list) {
  const byName = new Map();
  for (const c of list) {
    const prev = byName.get(c.name);
    // описание из файла важнее статической заглушки
    if (!prev || (!prev.description && c.description)) byName.set(c.name, c);
  }
  return [...byName.values()].sort((a, b) => a.name.localeCompare(b.name));
}

function build(cwd) {
  if (!pluginCache) pluginCache = scanPlugins();
  const list = [
    ...BUILTINS,
    ...scanDir(USER_CMDS, 'user'),
    ...(cwd ? scanDir(path.join(cwd, '.claude', 'commands'), 'project') : []),
    ...pluginCache,
    ...[...extraNames].map((name) => ({ name, description: '', hint: '', source: 'init' })),
  ];
  return dedupe(list);
}

function getForCwd(cwd) {
  const key = cwd || '~';
  const hit = cwdCache.get(key);
  if (hit && Date.now() - hit.at < 30000) return hit.list;
  const list = build(cwd);
  cwdCache.set(key, { list, at: Date.now() });
  return list;
}

/** подмешать имена из init-сообщения (slash_commands), сбросить кэш */
function enrichFromInit(names) {
  if (!Array.isArray(names)) return;
  let added = false;
  for (const n of names) {
    if (typeof n === 'string' && n && !extraNames.has(n)) { extraNames.add(n); added = true; }
  }
  if (added) cwdCache.clear();
}

/** сбросить кэши при изменении файлов команд */
function invalidate() {
  pluginCache = null;
  cwdCache.clear();
}

module.exports = { getForCwd, enrichFromInit, invalidate, USER_CMDS };
