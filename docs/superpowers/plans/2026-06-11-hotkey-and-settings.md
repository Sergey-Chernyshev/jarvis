# Хоткей и настройки панели — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Глобальный хоткей (⌘J) открывает панель по центру экрана в стиле Raycast; внутри панели появляется страница настроек (хоткей, тумблеры уведомлений, позиция, автозапуск).

**Architecture:** Настройки — JSON в `~/.jarvis/settings.json` через новый модуль `src/settings.js`. Хоткей — Electron `globalShortcut` в main. Панель получает два режима показа: «тихий» (трей/уведомление, без фокуса) и «фокусный» (хоткей; Esc/blur закрывают). UI настроек — вторая вьюха в том же окне.

**Tech Stack:** Electron (main + preload + renderer, без фреймворков), vanilla JS/CSS.

**Верификация:** в проекте нет тестов и git (по спеке) — каждая задача завершается ручной проверкой через `npm start` / `npm run demo`.

---

### Task 1: Модуль настроек `src/settings.js`

**Files:**
- Create: `src/settings.js`

- [ ] **Step 1: Создать модуль**

```js
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
```

- [ ] **Step 2: Проверить вручную**

Run: `node -e "const s=require('./src/settings.js'); console.log(s.load()); console.log(s.save({notifyDone:false})); console.log(require('node:fs').readFileSync(require('node:os').homedir()+'/.jarvis/settings.json','utf8'))"`
Expected: дефолты → объект с `notifyDone:false` → JSON на диске. После проверки вернуть: `node -e "require('./src/settings.js').save({notifyDone:true})"`.

### Task 2: Хоткей и два режима показа в `src/main.js`

**Files:**
- Modify: `src/main.js` (импорты; `positionPanel`/`showPanel`/`togglePanel`; `notify`-гейтинг в `reduce`; регистрация хоткея в `whenReady`; `will-quit`)

- [ ] **Step 1: Импорты и состояние**

В деструктуризацию electron добавить `globalShortcut`; рядом с константами:

```js
const settings = require('./settings');
let panelFocusMode = false; // показана ли панель «фокусно» (хоткей)
```

- [ ] **Step 2: Позиционирование по настройке + дисплей под курсором**

Заменить `positionPanel()`:

```js
function positionPanel() {
  const display = screen.getDisplayNearestPoint(screen.getCursorScreenPoint());
  const { workArea } = display;
  const [w, h] = panel.getSize();
  if (settings.load().position === 'corner') {
    panel.setPosition(workArea.x + workArea.width - w - 12, workArea.y + 12);
  } else {
    // центр по горизонтали, ~⅓ сверху — как Raycast
    panel.setPosition(
      workArea.x + Math.round((workArea.width - w) / 2),
      workArea.y + Math.round((workArea.height - h) / 3),
    );
  }
}
```

- [ ] **Step 3: Режимы показа**

Заменить `showPanel`/`togglePanel` и добавить blur-обработчик в `createPanel()`:

```js
function showPanel() {            // тихий режим: трей, клик по уведомлению
  if (!panel || panel.isDestroyed()) createPanel();
  panelFocusMode = false;
  positionPanel();
  panel.showInactive();
  push();
}

function showPanelFocused() {     // raycast-режим: хоткей
  if (!panel || panel.isDestroyed()) createPanel();
  panelFocusMode = true;
  positionPanel();
  panel.show();
  panel.focus();
  push();
}

function togglePanel() {          // трей
  if (panel && panel.isVisible()) panel.hide();
  else showPanel();
}

function toggleHotkeyPanel() {    // глобальный хоткей
  if (panel && panel.isVisible()) panel.hide();
  else showPanelFocused();
}
```

В `createPanel()` после `panel.on('close', ...)`:

```js
panel.on('blur', () => {
  if (panelFocusMode && panel.isVisible()) panel.hide();
});
```

- [ ] **Step 4: Регистрация хоткея с откатом**

```js
function registerHotkey(accelerator) {
  const current = settings.load().hotkey;
  if (current !== accelerator) {
    try { globalShortcut.unregister(current); } catch {}
  }
  let ok = false;
  try { ok = globalShortcut.register(accelerator, toggleHotkeyPanel); } catch {}
  if (!ok && accelerator !== current) {
    try { globalShortcut.register(current, toggleHotkeyPanel); } catch {}
    return { ok: false, error: `Сочетание ${accelerator} занято системой` };
  }
  return { ok };
}
```

В `app.whenReady().then(...)` после `startServer()` добавить `registerHotkey(settings.load().hotkey);`, и рядом с `before-quit`:

```js
app.on('will-quit', () => globalShortcut.unregisterAll());
```

- [ ] **Step 5: Гейтинг уведомлений в `reduce()`**

В кейсе `notification`: `if (isNew && settings.load().notifyWaiting) notify(...)`.
В кейсе `stop`: `if (settings.load().notifyDone) notify(...)`.

- [ ] **Step 6: Проверить вручную**

Run: `npm start`, затем ⌘J.
Expected: панель по центру (~⅓ сверху) с фокусом; Esc пока НЕ работает (Task 4), повторный ⌘J скрывает; клик мимо — скрывает; клик по трею — тихий показ, блюра-скрытия нет.

### Task 3: IPC `settings:get`/`settings:set` + preload

**Files:**
- Modify: `src/main.js` (блок `/* ===== ipc ===== */`)
- Modify: `src/preload.js`

- [ ] **Step 1: IPC-хендлеры**

```js
ipcMain.handle('settings:get', () => ({
  ...settings.load(),
  openAtLogin: app.getLoginItemSettings().openAtLogin,
}));

ipcMain.handle('settings:set', (_e, patch) => {
  if (!patch || typeof patch !== 'object') return { ok: false, error: 'bad patch' };
  const { openAtLogin, hotkey, ...rest } = patch;

  if (typeof openAtLogin === 'boolean') app.setLoginItemSettings({ openAtLogin });

  if (typeof hotkey === 'string' && hotkey) {
    const res = registerHotkey(hotkey);
    if (!res.ok) return res;
    settings.save({ hotkey });
  }

  if (Object.keys(rest).length) settings.save(rest);
  if (panel && panel.isVisible()) positionPanel(); // позиция могла смениться
  return { ok: true };
});
```

- [ ] **Step 2: preload**

В `contextBridge.exposeInMainWorld('jarvis', {...})` добавить:

```js
getSettings: () => ipcRenderer.invoke('settings:get'),
setSettings: (patch) => ipcRenderer.invoke('settings:set', patch),
```

- [ ] **Step 3: Проверить вручную**

Run: `npm start`, в DevTools панели (`panel.webContents.openDevTools()` временно или через осмотр): `await window.jarvis.getSettings()`.
Expected: объект с hotkey/notifyDone/notifyWaiting/position/openAtLogin. Проще: проверка придёт целиком в Task 4 через UI.

### Task 4: UI настроек в панели

**Files:**
- Modify: `src/renderer/index.html` (шестерёнка в шапке, вьюха настроек, CSS)
- Modify: `src/renderer/renderer.js` (переключение вьюх, биндинги, хоткей-рекордер, Esc)

- [ ] **Step 1: HTML — шестерёнка и вьюха настроек**

В `<header>` перед кнопкой `hide`: `<button id="gear" title="Настройки">⚙</button>`.
После `<div class="list" id="list"></div>`:

```html
<div class="settings" id="settings" hidden>
  <div class="srow">
    <span class="slabel">Хоткей панели</span>
    <button class="recorder" id="hotkey">⌘J</button>
  </div>
  <div class="serror" id="hotkeyError" hidden></div>
  <div class="srow">
    <span class="slabel">Уведомлять: закончил ответ</span>
    <input type="checkbox" id="notifyDone" class="toggle">
  </div>
  <div class="srow">
    <span class="slabel">Уведомлять: ждёт тебя</span>
    <input type="checkbox" id="notifyWaiting" class="toggle">
  </div>
  <div class="srow">
    <span class="slabel">Позиция панели</span>
    <div class="seg" id="position">
      <button data-v="center" class="segbtn">Центр</button>
      <button data-v="corner" class="segbtn">Угол</button>
    </div>
  </div>
  <div class="srow">
    <span class="slabel">Запускать при входе</span>
    <input type="checkbox" id="openAtLogin" class="toggle">
  </div>
</div>
```

- [ ] **Step 2: CSS настроек (тот же тёмный стиль)**

```css
.settings { flex: 1; overflow-y: auto; padding: 6px 0; }
.srow {
  display: flex; align-items: center; justify-content: space-between;
  gap: 12px; padding: 9px 14px;
}
.srow:hover { background: var(--row-hover); }
.slabel { font-size: 12px; color: var(--text); }
.serror { padding: 0 14px 6px; font-size: 11px; color: var(--waiting); }

.recorder {
  appearance: none; border: 1px solid var(--border); background: rgba(255,255,255,0.05);
  color: var(--text); font: inherit; font-size: 12px; padding: 4px 10px;
  border-radius: 7px; min-width: 64px; cursor: default;
  font-variant-numeric: tabular-nums;
}
.recorder.recording { border-color: var(--working); color: var(--working); }

.toggle {
  appearance: none; width: 34px; height: 20px; border-radius: 10px;
  background: rgba(255,255,255,0.12); position: relative; outline: none;
  transition: background 0.15s ease;
}
.toggle:checked { background: var(--done); }
.toggle::after {
  content: ""; position: absolute; top: 2px; left: 2px; width: 16px; height: 16px;
  border-radius: 50%; background: #fff; transition: left 0.15s ease;
}
.toggle:checked::after { left: 16px; }

.seg { display: flex; border: 1px solid var(--border); border-radius: 7px; overflow: hidden; }
.segbtn {
  appearance: none; border: 0; background: transparent; color: var(--muted);
  font: inherit; font-size: 11px; padding: 4px 10px; cursor: default;
}
.segbtn.active { background: rgba(255,255,255,0.1); color: var(--text); }
header button.active { color: var(--text); background: var(--row-hover); }
```

- [ ] **Step 3: renderer.js — вьюхи, биндинги, рекордер, Esc**

```js
/* ---------- настройки ---------- */
const settingsEl = document.getElementById('settings');
const gearEl = document.getElementById('gear');
const hotkeyBtn = document.getElementById('hotkey');
const hotkeyErr = document.getElementById('hotkeyError');
let recording = false;

function showSettings(on) {
  settingsEl.hidden = !on;
  listEl.hidden = on;
  gearEl.classList.toggle('active', on);
  if (on) loadSettings();
}
gearEl.addEventListener('click', () => showSettings(settingsEl.hidden));

function displayHotkey(acc) {
  return acc
    .replace('CommandOrControl', '⌘').replace('Command', '⌘')
    .replace('Control', '⌃').replace('Option', '⌥').replace('Alt', '⌥')
    .replace('Shift', '⇧').replaceAll('+', '');
}

async function loadSettings() {
  const s = await window.jarvis.getSettings();
  hotkeyBtn.textContent = displayHotkey(s.hotkey);
  document.getElementById('notifyDone').checked = s.notifyDone;
  document.getElementById('notifyWaiting').checked = s.notifyWaiting;
  document.getElementById('openAtLogin').checked = s.openAtLogin;
  for (const b of document.querySelectorAll('.segbtn')) {
    b.classList.toggle('active', b.dataset.v === s.position);
  }
}

for (const id of ['notifyDone', 'notifyWaiting', 'openAtLogin']) {
  document.getElementById(id).addEventListener('change', (e) => {
    window.jarvis.setSettings({ [id]: e.target.checked });
  });
}
document.getElementById('position').addEventListener('click', (e) => {
  const v = e.target.dataset?.v;
  if (!v) return;
  window.jarvis.setSettings({ position: v }).then(loadSettings);
});

/* рекордер хоткея */
const CODE_KEYS = { Space: 'Space', Enter: 'Enter', Backspace: 'Backspace', Tab: 'Tab' };
function accelFromEvent(e) {
  const mods = [];
  if (e.metaKey) mods.push('Command');
  if (e.ctrlKey) mods.push('Control');
  if (e.altKey) mods.push('Option');
  if (e.shiftKey) mods.push('Shift');
  if (!mods.some((m) => m !== 'Shift')) return null; // нужен не-Shift модификатор
  let key = null;
  if (/^Key[A-Z]$/.test(e.code)) key = e.code.slice(3);
  else if (/^Digit[0-9]$/.test(e.code)) key = e.code.slice(5);
  else if (/^F([1-9]|1[0-9]|2[0-4])$/.test(e.code)) key = e.code;
  else if (CODE_KEYS[e.code]) key = CODE_KEYS[e.code];
  if (!key) return null;
  return [...mods, key].join('+');
}

hotkeyBtn.addEventListener('click', () => {
  recording = true;
  hotkeyErr.hidden = true;
  hotkeyBtn.classList.add('recording');
  hotkeyBtn.textContent = 'нажми сочетание…';
});

window.addEventListener('keydown', async (e) => {
  if (recording) {
    e.preventDefault();
    if (e.key === 'Escape') { recording = false; hotkeyBtn.classList.remove('recording'); loadSettings(); return; }
    const acc = accelFromEvent(e);
    if (!acc) return; // ждём полный аккорд
    recording = false;
    hotkeyBtn.classList.remove('recording');
    const res = await window.jarvis.setSettings({ hotkey: acc });
    if (!res.ok) { hotkeyErr.textContent = res.error || 'Не удалось назначить'; hotkeyErr.hidden = false; }
    loadSettings();
    return;
  }
  if (e.key === 'Escape') window.jarvis.hidePanel(); // raycast: Esc закрывает
}, true);
```

И в `render()` ничего не менять — список и настройки не пересекаются.

- [ ] **Step 4: Полная ручная проверка (чек-лист спеки)**

Run: `npm run demo`
Expected:
1. ⌘J — панель по центру с фокусом; Esc закрывает; клик мимо закрывает; ⌘J повторно закрывает.
2. Трей-клик — тихий показ, фокус в терминале, не схлопывается от blur.
3. Шестерёнка — настройки; рекордер пишет новое сочетание (например ⌘⇧J), старое перестаёт работать, новое работает.
4. Тумблеры уведомлений глушат соответствующие уведомления (демо шлёт notification и stop).
5. Позиция «Угол» — панель прыгает в правый верхний угол; переживает перезапуск (`settings.json`).
6. «Запускать при входе» меняет `app.getLoginItemSettings()`.

### Task 5: README

**Files:**
- Modify: `README.md` (раздел «Быстрый старт» / ограничения)

- [ ] **Step 1: Дописать про хоткей и настройки**

После строки про клик по ◇: `Глобальный хоткей ⌘J открывает панель по центру (меняется в настройках — шестерёнка в шапке панели). Esc или клик мимо — закрыть.` Убрать из ограничений упоминание, что настроек нет (если есть).
