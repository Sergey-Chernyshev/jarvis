/* Внешность: тема, краска, режим окна и настройка вида (экран 14f «вид»).
 *
 * Ставит на <html> атрибуты, из которых собирается вся палитра и раскладка:
 *   data-theme   = light | dark        (auto → системная prefers-color-scheme)
 *   data-paint   = clover | coal | raspberry | custom
 *   data-mode    = overlay | window    (накладка ⌘J или обычное окно, макет 14h)
 *   data-density = compact | normal | roomy
 *   data-radius  = sharp | normal | soft
 * плюс --ui-scale (масштаб интерфейса) и, для своей краски, вычисленные
 * инлайновые токены акцента.
 *
 * Порядок применения (важен, чтобы не мигало): localStorage → сразу, ещё до
 * первой отрисовки; settings.json → когда мост ответит; событие `appearance`
 * из демона → когда настройку сменили в другом окне.
 *
 * Скрипт синхронный и без зависимостей: подключается в <head> ПЕРЕД bridge.js. */

(() => {
  const THEMES = ['light', 'dark', 'auto'];
  const PAINTS = ['clover', 'coal', 'raspberry', 'custom'];
  const MODES = ['overlay', 'window'];
  const DENSITIES = ['compact', 'normal', 'roomy'];
  const RADII = ['sharp', 'normal', 'soft'];
  const SCALE_MIN = 0.85;
  const SCALE_MAX = 1.4;
  const KEY = 'jarvis.appearance';
  const root = document.documentElement;

  const media = window.matchMedia ? window.matchMedia('(prefers-color-scheme: dark)') : null;

  const DEFAULTS = {
    theme: 'light',
    paint: 'clover',
    mode: 'overlay',
    density: 'normal',
    radius: 'normal',
    scale: 1,
    accent: '#0B6B44', // своя краска: база, из которой выводится весь акцент
  };

  /** Текущий выбор пользователя (не разрешённый). */
  let choice = { ...DEFAULTS };

  const clean = (v, allowed, fallback) => (allowed.includes(v) ? v : fallback);
  const clamp = (n, lo, hi) => Math.min(hi, Math.max(lo, n));

  /* ---------- цвет: вывод акцентной семьи из одного тона ---------- */

  const HEX = /^#?([0-9a-f]{3}|[0-9a-f]{6})$/i;

  /** '#0B6B44' | '0b6' → {r,g,b}; мусор → null. */
  function parseHex(v) {
    if (typeof v !== 'string' || !HEX.test(v.trim())) return null;
    let h = v.trim().replace('#', '');
    if (h.length === 3) h = h.split('').map((c) => c + c).join('');
    const n = parseInt(h, 16);
    return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
  }

  const toHex = ({ r, g, b }) =>
    '#' + [r, g, b].map((c) => clamp(Math.round(c), 0, 255).toString(16).padStart(2, '0')).join('').toUpperCase();

  /** Смешать два цвета: t=0 → a, t=1 → b. */
  const mix = (a, b, t) => ({
    r: a.r + (b.r - a.r) * t,
    g: a.g + (b.g - a.g) * t,
    b: a.b + (b.b - a.b) * t,
  });

  /** Относительная яркость (WCAG) — по ней выбираем текст на заливке. */
  function luminance({ r, g, b }) {
    const f = (c) => {
      const s = c / 255;
      return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
    };
    return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
  }

  const rgba = ({ r, g, b }, a) =>
    `rgba(${Math.round(r)}, ${Math.round(g)}, ${Math.round(b)}, ${a})`;

  /**
   * Своя краска: из одного тона выводим всю акцентную семью так же, как она
   * устроена у готовых красок — заливка, тональная подложка, цифра на ней,
   * текст на заливке. Тёмная тема получает свой набор: тон осветляется,
   * подложка становится полупрозрачной.
   */
  function applyCustomAccent(dark) {
    const base = parseHex(choice.accent) || parseHex(DEFAULTS.accent);
    const white = { r: 255, g: 255, b: 255 };
    const ink = dark ? { r: 239, g: 242, b: 239 } : { r: 23, g: 32, b: 26 };
    const paper = dark ? { r: 30, g: 33, b: 31 } : white;

    // на тёмном тёмный тон не читается — подтягиваем к светлому
    const accent = dark && luminance(base) < 0.18 ? mix(base, white, 0.38) : base;
    // текст акцентом на бумаге: слишком светлый тон притемняем
    const accentText = !dark && luminance(accent) > 0.45 ? mix(accent, ink, 0.35) : accent;
    const onAccent = luminance(accent) > 0.42 ? ink : white;

    const set = (k, v) => root.style.setProperty(k, v);
    set('--accent', toHex(accent));
    set('--accent-text', toHex(accentText));
    set('--accent-soft', dark ? rgba(accent, 0.15) : toHex(mix(accent, paper, 0.88)));
    set('--accent-ink', toHex(dark ? mix(accent, white, 0.45) : mix(accent, ink, 0.3)));
    set('--accent-line', rgba(accent, dark ? 0.34 : 0.28));
    set('--on-accent', toHex(onAccent));
    set('--on-accent-dim', rgba(onAccent, 0.62));

    // Нейтрали тоже тонируются выбранным тоном — иначе поверхности остаются
    // зеленоватыми от «клевера» и спорят с краской. Готовые краски устроены
    // так же: у каждой свой оттенок бумаги, подложек и волосяных линий.
    set('--surface', toHex(mix(paper, accent, dark ? 0.10 : 0.055)));
    set('--surface-2', toHex(mix(paper, accent, dark ? 0.17 : 0.11)));
    set('--paper-2', toHex(mix(paper, accent, dark ? 0.05 : 0.022)));
    set('--line', dark ? rgba(accent, 0.16) : toHex(mix(paper, accent, 0.10)));
    set('--line-strong', dark ? rgba(accent, 0.26) : toHex(mix(paper, accent, 0.20)));
    set('--dot-sleep', toHex(mix(paper, accent, dark ? 0.22 : 0.18)));
  }

  /** Снять инлайновые токены — вернуть управление готовой краске из theme.css. */
  function clearCustomAccent() {
    for (const k of ['--accent', '--accent-text', '--accent-soft', '--accent-ink',
                     '--accent-line', '--on-accent', '--on-accent-dim',
                     '--surface', '--surface-2', '--paper-2',
                     '--line', '--line-strong', '--dot-sleep']) {
      root.style.removeProperty(k);
    }
  }

  /** auto → в реальную тему по системной настройке. */
  const resolve = (theme) => (theme === 'auto' ? (media && media.matches ? 'dark' : 'light') : theme);

  function paint() {
    const theme = resolve(choice.theme);
    root.setAttribute('data-theme', theme);
    root.setAttribute('data-paint', choice.paint);
    // раскладка: 'overlay' — накладка ⌘J, 'window' — обычное окно (макет 14h)
    root.setAttribute('data-mode', choice.mode);
    root.setAttribute('data-density', choice.density);
    root.setAttribute('data-radius', choice.radius);
    root.style.setProperty('--ui-scale', String(choice.scale));
    if (choice.paint === 'custom') applyCustomAccent(theme === 'dark');
    else clearCustomAccent();
  }

  /** Применить выбор (без записи в settings.json) и разбудить слушателей. */
  function apply(next, { persist = true } = {}) {
    const prev = choice;
    const n = next || {};
    choice = {
      theme: clean(n.theme, THEMES, prev.theme),
      paint: clean(n.paint, PAINTS, prev.paint),
      mode: clean(n.mode, MODES, prev.mode),
      density: clean(n.density, DENSITIES, prev.density),
      radius: clean(n.radius, RADII, prev.radius),
      scale: Number.isFinite(+n.scale) && +n.scale > 0
        ? clamp(+n.scale, SCALE_MIN, SCALE_MAX) : prev.scale,
      accent: parseHex(n.accent) ? toHex(parseHex(n.accent)) : prev.accent,
    };
    paint();
    if (persist) {
      try { localStorage.setItem(KEY, JSON.stringify(choice)); } catch { /* приватный режим */ }
    }
    const changed = Object.keys(choice).some((k) => choice[k] !== prev[k]);
    if (changed) {
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
    const onSystem = () => {
      if (choice.theme !== 'auto') return;
      paint();
      window.dispatchEvent(new CustomEvent('jarvis:appearance', { detail: { ...choice } }));
    };
    if (media.addEventListener) media.addEventListener('change', onSystem);
    else if (media.addListener) media.addListener(onSystem);
  }

  window.jarvisTheme = {
    THEMES, PAINTS, MODES, DENSITIES, RADII,
    SCALE_MIN, SCALE_MAX,
    DEFAULTS: { ...DEFAULTS },
    get: () => ({ ...choice }),
    /** Разрешённая тема — та, что реально на <html> (auto уже развёрнут). */
    resolved: () => resolve(choice.theme),
    /** Сменить и сохранить в settings.json (демон разошлёт остальным окнам). */
    set(next) {
      apply(next);
      const patch = {};
      for (const k of ['theme', 'paint', 'mode', 'density', 'radius', 'scale', 'accent']) {
        if (next && next[k] !== undefined) patch[k] = choice[k];
      }
      try { window.jarvis?.setSettings?.(patch); } catch { /* окно без моста */ }
    },
    /** Вернуть вид к заводскому (краска и режим остаются — их выбирают отдельно). */
    reset() {
      this.set({
        density: DEFAULTS.density,
        radius: DEFAULTS.radius,
        scale: DEFAULTS.scale,
      });
    },
    /** Принять состояние извне (settings.json / другое окно) без обратной записи. */
    adopt: (next) => apply(next),
  };

  // 3. Мост появляется позже скрипта — ждём его и подтягиваем settings.json.
  const FIELDS = ['theme', 'paint', 'mode', 'density', 'radius', 'scale', 'accent'];
  const pull = () => {
    const j = window.jarvis;
    if (!j) return false;
    try {
      Promise.resolve(j.getSettings?.()).then((s) => {
        if (!s) return;
        const next = {};
        for (const k of FIELDS) if (s[k] !== undefined) next[k] = s[k];
        if (Object.keys(next).length) apply(next);
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
