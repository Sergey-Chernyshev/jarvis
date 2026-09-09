/* Подписи клавиш и платформенные названия.
 *
 * На macOS модификаторы принято рисовать символами (⌘⌥⇧⌃), на Linux — словами
 * (Ctrl, Alt, Shift, Super). Раньше символы были зашиты по всему UI, из-за чего
 * Linux-сборка обещала клавиши, которых на клавиатуре нет.
 *
 * ОС определяем синхронно по userAgent: если ждать ответа моста, подписи успеют
 * отрисоваться маковскими и мигнуть. Заодно ставим на <html> data-os — им
 * пользуется CSS и по нему же удобно отлаживать.
 *
 * Подключается в <head> сразу после theme.js, ДО bridge.js. */

(() => {
  // Через `window`, а не голым глобалом: в вебвью это одно и то же, а вне его
  // — нет. Голый `navigator` появился в Node только в 21-й версии, и на 18-й
  // модуль падал ReferenceError'ом ещё до первой строки полезной работы,
  // унося с собой все тесты панели, которые его подключают.
  const ua = ((typeof window !== 'undefined' && window.navigator) || {}).userAgent || '';
  const isMac = /Mac OS X|Macintosh/i.test(ua) && !/Android/i.test(ua);
  document.documentElement.setAttribute('data-os', isMac ? 'macos' : 'linux');

  /* Модификаторы. `MOD` — главный модификатор приложения: на маке ⌘, на Linux
   * Ctrl. Super на Linux принадлежит окружению рабочего стола (в GNOME Super+1..4
   * переключает приложения дока), поэтому приложение его не занимает. Значение
   * обязано совпадать с тем, что слушает renderer (isMod) и что регистрируется
   * глобальным хоткеем — иначе подпись врёт. */
  const MOD = isMac ? '⌘' : 'Ctrl';
  const ALT = isMac ? '⌥' : 'Alt';
  const SHIFT = isMac ? '⇧' : 'Shift';
  const CTRL = isMac ? '⌃' : 'Ctrl';
  const SUPER = isMac ? '⌘' : 'Super';

  /* Разделитель: у символов он не нужен (⌘⌥J), у слов — обязателен (Ctrl+Alt+J). */
  const SEP = isMac ? '' : '+';

  /** Собрать подпись из модификаторов и клавиши: k('J') → '⌘J' | 'Ctrl+J'. */
  const k = (key, { alt = false, shift = false, mod = true } = {}) => {
    const parts = [];
    if (mod) parts.push(MOD);
    if (alt) parts.push(ALT);
    if (shift) parts.push(SHIFT);
    parts.push(key);
    return parts.join(SEP);
  };

  /** Клавиша без модификаторов, с человеческим именем на Linux. */
  const NAMES = isMac
    ? { enter: '↵', esc: 'esc', del: '⌫', up: '↑', down: '↓', updown: '↑↓', tab: '⇥' }
    : { enter: 'Enter', esc: 'Esc', del: 'Backspace', up: '↑', down: '↓', updown: '↑↓', tab: 'Tab' };

  /**
   * Аксельератор Tauri → подпись. 'Command+Alt+J' → '⌘⌥J' | 'Ctrl+Alt+J'.
   * Порядок и набор модификаторов сохраняем как есть — их задаёт пользователь.
   */
  function displayHotkey(acc) {
    const s = String(acc || '');
    if (!s) return '';
    return s
      .split('+')
      .map((part) => {
        switch (part.trim()) {
          case 'CommandOrControl':
          case 'CmdOrCtrl':
          case 'Command':
          case 'Cmd':
          case 'Super':
          case 'Meta': {
            // На Linux аксельератор «Command» плагин регистрирует как Super —
            // так и подписываем, иначе юзер будет искать несуществующую клавишу.
            // Дефолты на Linux при этом выдаются уже в Control (см. settings.rs).
            if (isMac) return '⌘';
            const t = part.trim();
            return t === 'CommandOrControl' || t === 'CmdOrCtrl' ? 'Ctrl' : 'Super';
          }
          case 'Control':
          case 'Ctrl':
            return CTRL;
          case 'Option':
          case 'Alt':
            return ALT;
          case 'Shift':
            return SHIFT;
          default:
            return part.trim();
        }
      })
      .filter(Boolean)
      .join(isMac ? ' ' : '+');
  }

  /** Аксельератор → массив отдельных клавиш-капсов. */
  const hotkeyKeys = (acc) => displayHotkey(acc).split(isMac ? ' ' : '+').filter(Boolean);

  /* Названия системных вещей, у которых на Linux другое имя. */
  const NOUNS = isMac
    ? { fileManager: 'Finder', reveal: 'Показать в Finder', os: 'macOS' }
    : { fileManager: 'файловом менеджере', reveal: 'Показать в папке', os: 'Linux' };

  // A physical letter fallback keeps shortcuts usable in Cyrillic layouts.
  // Latin layouts keep their semantic key (including Dvorak/AZERTY).
  function matches(event, wanted) {
    const key = String(event.key || '');
    if (key.toLowerCase() === wanted.toLowerCase()) return true;
    return /^[a-z]$/i.test(wanted) && !/^[a-z]$/i.test(key) && event.code === 'Key' + wanted.toUpperCase();
  }
  function editing(target) {
    return !!target && (['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName) || target.isContentEditable === true || !!target.closest?.('[contenteditable="true"], [role="textbox"], .xterm'));
  }
  window.jarvisKeys = { matches, editing, isMac, MOD, ALT, SHIFT, CTRL, SUPER, SEP, k, NAMES, NOUNS, displayHotkey, hotkeyKeys };

  /* Разметка в index.html статична, поэтому подписи в ней проставляем здесь:
   * элемент помечается data-key (модификатор+клавиша) или data-keyname (одиночная
   * клавиша), а текст подставляется под текущую ОС. */
  const paint = () => {
    for (const el of document.querySelectorAll('[data-key]')) {
      const spec = el.getAttribute('data-key'); // напр. "J", "alt:C", "shift:P"
      const [flags, key] = spec.includes(':') ? spec.split(':') : ['', spec];
      el.textContent = k(key, { alt: flags.includes('alt'), shift: flags.includes('shift') });
    }
    for (const el of document.querySelectorAll('[data-keyname]')) {
      const n = el.getAttribute('data-keyname');
      el.textContent = NAMES[n] || n;
    }
  };
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', paint);
  else paint();
})();
