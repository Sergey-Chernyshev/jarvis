/* Внешность: тема, краска и режим окна (дизайн «Клевер», экран 14f «вид»).
 *
 * Ставит на <html> три атрибута, из которых собирается вся палитра и раскладка:
 *   data-theme = light | dark        (auto → системная prefers-color-scheme)
 *   data-paint = clover | coal | raspberry
 *   data-mode  = overlay | window    (накладка ⌘J или обычное окно, макет 14h)
 *
 * Порядок применения (важен, чтобы не мигало): localStorage → сразу, ещё до
 * первой отрисовки; settings.json → когда мост ответит; событие `appearance`
 * из демона → когда настройку сменили в другом окне.
 *
 * Скрипт синхронный и без зависимостей: подключается в <head> ПЕРЕД bridge.js. */

(() => {
  const THEMES = ['light', 'dark', 'auto'];
  const PAINTS = ['clover', 'coal', 'raspberry'];
  const MODES = ['overlay', 'window'];
  const KEY = 'jarvis.appearance';
  const root = document.documentElement;

  const media = window.matchMedia ? window.matchMedia('(prefers-color-scheme: dark)') : null;

  /** Текущий выбор пользователя (не разрешённый): {theme, paint, mode}. */
  let choice = { theme: 'light', paint: 'clover', mode: 'overlay' };

  const clean = (v, allowed, fallback) => (allowed.includes(v) ? v : fallback);

  /** auto → в реальную тему по системной настройке. */
  const resolve = (theme) => (theme === 'auto' ? (media && media.matches ? 'dark' : 'light') : theme);

  function paint() {
    root.setAttribute('data-theme', resolve(choice.theme));
    root.setAttribute('data-paint', choice.paint);
    // раскладка: 'overlay' — накладка ⌘J, 'window' — обычное окно (макет 14h)
    root.setAttribute('data-mode', choice.mode);
  }

  /** Применить выбор (без записи в settings.json) и разбудить слушателей. */
  function apply(next, { persist = true } = {}) {
    const prev = choice;
    choice = {
      theme: clean(next && next.theme, THEMES, prev.theme),
      paint: clean(next && next.paint, PAINTS, prev.paint),
      mode: clean(next && next.mode, MODES, prev.mode),
    };
    paint();
    if (persist) {
      try { localStorage.setItem(KEY, JSON.stringify(choice)); } catch { /* приватный режим */ }
    }
    if (prev.theme !== choice.theme || prev.paint !== choice.paint || prev.mode !== choice.mode) {
      window.dispatchEvent(new CustomEvent('jarvis:appearance', { detail: { ...choice } }));
    }
  }

  // 1. Кэш — синхронно, до первой отрисовки: панель не моргает белым на тёмной теме.
  try {
    const cached = JSON.parse(localStorage.getItem(KEY) || 'null');
    if (cached) apply(cached, { persist: false });
    else paint();
  } catch { paint(); }

  // 2. Системная тема — только когда выбран auto.
  if (media) {
    const onSystem = () => { if (choice.theme === 'auto') { paint(); window.dispatchEvent(new CustomEvent('jarvis:appearance', { detail: { ...choice } })); } };
    if (media.addEventListener) media.addEventListener('change', onSystem);
    else if (media.addListener) media.addListener(onSystem);
  }

  window.jarvisTheme = {
    THEMES, PAINTS, MODES,
    get: () => ({ ...choice }),
    /** Разрешённая тема — та, что реально на <html> (auto уже развёрнут). */
    resolved: () => resolve(choice.theme),
    /** Сменить и сохранить в settings.json (демон разошлёт остальным окнам). */
    set(next) {
      apply(next);
      const patch = {};
      if (next && next.theme !== undefined) patch.theme = choice.theme;
      if (next && next.paint !== undefined) patch.paint = choice.paint;
      if (next && next.mode !== undefined) patch.mode = choice.mode;
      try { window.jarvis?.setSettings?.(patch); } catch { /* окно без моста */ }
    },
    /** Принять состояние извне (settings.json / другое окно) без обратной записи. */
    adopt: (next) => apply(next),
  };

  // 3. Мост появляется позже скрипта — ждём его и подтягиваем settings.json.
  const pull = () => {
    const j = window.jarvis;
    if (!j) return false;
    try {
      Promise.resolve(j.getSettings?.()).then((s) => {
        if (s && (s.theme || s.paint || s.mode)) apply({ theme: s.theme, paint: s.paint, mode: s.mode });
      }).catch(() => {});
      j.onAppearance?.((p) => apply(p));
    } catch { /* мост есть, но команда не поддержана — остаёмся на кэше */ }
    return true;
  };
  if (!pull()) {
    let tries = 0;
    const t = setInterval(() => { if (pull() || ++tries > 40) clearInterval(t); }, 50);
  }
})();
