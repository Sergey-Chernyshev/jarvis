/* Project artwork is local image data. Discovery is lazy, bounded and read-only;
 * choosing an image is persisted by the Projects form, never by this helper. */
(() => {
  'use strict';
  const MAX_FILE = 5 * 1024 * 1024, MAX_STORED = 128 * 1024;
  const cache = new Map(), normalized = new Map(), queue = [];
  let active = 0;
  const keyOf = (machine, cwd) => JSON.stringify([machine || 'local', cwd]);
  function safeSource(value, rasterOnly = false) {
    if (typeof value !== 'string' || value.length > Math.ceil(MAX_FILE * 4 / 3) + 128) return false;
    const match = /^data:image\/(png|jpeg|webp|gif|x-icon|vnd\.microsoft\.icon|svg\+xml);base64,([A-Za-z0-9+/]+={0,2})$/.exec(value);
    if (!match || (rasterOnly && !['png', 'jpeg', 'webp'].includes(match[1]))) return false;
    if (match[1] === 'svg+xml') {
      try {
        const xml = window.atob(match[2]);
        // SVG remains an <img>, never document markup. Also reject resource
        // references so loading artwork cannot contact a website or local URL.
        if (!/<svg(?:\s|>)/i.test(xml) || /<!DOCTYPE|<!ENTITY|<\?xml-stylesheet|<(?:script|foreignObject|iframe|object|embed|image|feImage)\b|\bon\w+\s*=|@import/i.test(xml)
          || /\b(?:href|src)\s*=\s*["']\s*(?!#)/i.test(xml)
          || /url\(\s*["']?\s*(?!#)/i.test(xml)) return false;
      } catch { return false; }
    }
    return true;
  }
  function prune(map, max) {
    for (const [key, value] of map) {
      if (map.size <= max) break;
      if (!value.pending) map.delete(key);
    }
  }
  function pump() {
    while (active < 2 && queue.length) {
      const job = queue.shift(); active++;
      Promise.resolve().then(job.run).then(job.resolve, job.reject).finally(() => { active--; pump(); });
    }
  }
  async function discover(machine, cwd, { refresh = false } = {}) {
    machine ||= 'local'; cwd = String(cwd || '').trim();
    if (!cwd.startsWith('/') || /[\x00-\x1f]/.test(cwd)) return { ok: false, candidates: [], error: 'Сначала укажи полный путь к проекту.' };
    const key = keyOf(machine, cwd), old = cache.get(key);
    if (old && (old.pending || (!refresh && Date.now() - old.at < (old.ok ? 300000 : 60000)))) return old.promise;
    const entry = { pending: true, at: Date.now(), ok: false };
    entry.promise = new Promise((resolve, reject) => {
      queue.push({ resolve, reject, run: async () => {
        if (typeof window.jarvis?.projectsIconCandidates !== 'function') throw new Error('Поиск иконок недоступен в этой версии Jarvis.');
        const result = await window.jarvis.projectsIconCandidates(machine, cwd);
        if (!result?.ok) throw new Error(result?.error || 'Не удалось найти иконки проекта.');
        const candidates = (Array.isArray(result.candidates) ? result.candidates : []).slice(0, 12)
          .filter(c => c && typeof c.path === 'string' && safeSource(c.dataUrl));
        return { ok: true, candidates, truncated: !!result.truncated };
      } });
    }).catch(error => ({ ok: false, candidates: [], error: String(error?.message || error) }))
      .then(result => { entry.pending = false; entry.ok = result.ok; entry.at = Date.now(); prune(cache, 48); return result; });
    cache.set(key, entry); pump(); return entry.promise;
  }
  function normalize(dataUrl) {
    if (!safeSource(dataUrl)) return Promise.reject(new Error('Выбери PNG, JPEG, WebP, GIF, ICO или SVG без внешних ресурсов.'));
    if (normalized.has(dataUrl)) return normalized.get(dataUrl).promise;
    const entry = { pending: true };
    entry.promise = new Promise((resolve, reject) => {
      const image = new window.Image();
      const finish = (error, result) => { window.clearTimeout(timer); image.onload = null; image.onerror = null; error ? reject(error) : resolve(result); };
      const timer = window.setTimeout(() => { image.src = ''; finish(new Error('Не удалось прочитать картинку. Попробуй другой файл.')); }, 10000);
      image.onerror = () => finish(new Error('Файл не удалось распознать как картинку.'));
      image.onload = () => {
        try {
          const width = image.naturalWidth || image.width, height = image.naturalHeight || image.height;
          if (!width || !height || width > 8192 || height > 8192 || width * height > 40000000) throw new Error('Картинка слишком большая. Максимальная сторона — 8192 пикселя.');
          const scale = Math.min(1, 128 / Math.max(width, height));
          const canvas = document.createElement('canvas'); canvas.width = Math.max(1, Math.round(width * scale)); canvas.height = Math.max(1, Math.round(height * scale));
          const context = canvas.getContext('2d'); if (!context) throw new Error('Не удалось обработать картинку.');
          context.drawImage(image, 0, 0, canvas.width, canvas.height);
          const result = canvas.toDataURL('image/png');
          if (!safeSource(result, true) || result.length > Math.ceil(MAX_STORED * 4 / 3) + 64) throw new Error('Не удалось уменьшить картинку для аватарки.');
          finish(null, result);
        } catch (error) { finish(error); }
      };
      image.src = dataUrl;
    }).finally(() => { entry.pending = false; prune(normalized, 48); });
    normalized.set(dataUrl, entry);
    entry.promise.catch(() => { normalized.delete(dataUrl); });
    return entry.promise;
  }
  function fromFile(file) {
    if (!file || !file.size || file.size > MAX_FILE) return Promise.reject(new Error('Выбери картинку размером до 5 МБ.'));
    return new Promise((resolve, reject) => {
      const reader = new window.FileReader();
      reader.onerror = () => reject(new Error('Не удалось прочитать выбранный файл.'));
      reader.onload = () => {
        let data = String(reader.result || '');
        if (/^data:(?:application\/octet-stream|);base64,/.test(data)) {
          const ext = String(file.name || '').split('.').pop().toLowerCase();
          const mime = { png: 'png', jpg: 'jpeg', jpeg: 'jpeg', webp: 'webp', gif: 'gif', ico: 'x-icon', svg: 'svg+xml' }[ext];
          if (mime) data = data.replace(/^data:[^;]*;/, `data:image/${mime};`);
        }
        normalize(data).then(resolve, reject);
      };
      reader.readAsDataURL(file);
    });
  }
  const observed = new WeakMap();
  const observer = typeof window.IntersectionObserver === 'function' ? new window.IntersectionObserver(entries => {
    for (const entry of entries) {
      if (!entry.target.isConnected) { observer.unobserve(entry.target); observed.delete(entry.target); }
      else if (entry.isIntersecting) { observer.unobserve(entry.target); observed.get(entry.target)?.(); observed.delete(entry.target); }
    }
  }, { rootMargin: '80px' }) : null;
  function create(project, { className = '', onChoose } = {}) {
    const element = document.createElement(onChoose ? 'button' : 'span');
    element.className = 'project-avatar' + (className ? ` ${className}` : '');
    if (onChoose) {
      element.type = 'button'; element.setAttribute('aria-label', 'Выбрать аватарку проекта');
      element.addEventListener('click', event => { event.stopPropagation(); onChoose(); });
    } else element.setAttribute('aria-hidden', 'true');
    const fallback = () => { element.replaceChildren(window.jarvisIcons.create('folder-simple', 21)); };
    const paint = src => {
      const image = document.createElement('img'); image.className = 'project-avatar-image'; image.alt = ''; image.draggable = false;
      image.addEventListener('error', fallback, { once: true }); image.src = src; element.replaceChildren(image);
    };
    fallback();
    if (safeSource(project.avatar?.dataUrl, true)) { paint(project.avatar.dataUrl); return element; }
    if (!project.cwd) return element;
    const machine = project.machine || project.remote || 'local';
    const offline = project.connection?.online === false;
    const previous = cache.get(keyOf(machine, project.cwd));
    if (offline && !previous) return element;
    const load = async () => {
      const result = await (offline ? previous.promise : discover(machine, project.cwd));
      if (!result.ok) { element.title = result.error; return; }
      if (result.candidates.length === 1) {
        try { paint(await normalize(result.candidates[0].dataUrl)); element.title = result.candidates[0].path; }
        catch { element.title = 'Не удалось прочитать иконку. Можно выбрать свою картинку.'; }
      } else if (result.candidates.length > 1) {
        const badge = document.createElement('small'); badge.className = 'project-avatar-count'; badge.textContent = String(result.candidates.length); element.append(badge);
        element.title = `Найдено иконок: ${result.candidates.length}. Выбери аватарку проекта.`;
      }
    };
    if (observer) { window.requestAnimationFrame(() => { if (element.isConnected) { observed.set(element, load); observer.observe(element); } }); }
    else Promise.resolve().then(load);
    return element;
  }
  window.JarvisProjectAvatars = { create, discover, normalize, fromFile, safeSource };
})();
