/* Shared attachment handling for the new-chat and reply composers. */
(() => {
  const limit = 25 * 1024 * 1024;
  function read(file) {
    if (file.size > limit) return Promise.reject(new Error('Файл больше 25 МБ'));
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onerror = () => reject(new Error('Не удалось прочитать ' + file.name));
      reader.onload = () => resolve({ id: crypto.randomUUID(), name: file.name || 'image.png', type: file.type, dataUrl: String(reader.result) });
      reader.readAsDataURL(file);
    });
  }
  function render(root, files, remove) {
    root.replaceChildren(); root.hidden = !files.length;
    for (const file of files) {
      const chip = document.createElement('div'); chip.className = 'sw-attachment';
      if (file.loading) { chip.dataset.state = 'loading'; chip.setAttribute('aria-busy', 'true'); const spinner = document.createElement('span'); spinner.className = 'ui-spinner'; spinner.setAttribute('aria-hidden', 'true'); chip.append(spinner); }
      const preview = document.createElement('button'); preview.type = 'button'; preview.className = 'sw-attachment-preview'; preview.disabled = !!file.loading; preview.setAttribute('aria-label', 'Открыть ' + file.name); preview.addEventListener('click', () => window.JarvisArtifacts?.open({ ...file, source: 'Вложение' }));
      if (!file.loading && file.type?.startsWith('image/')) { const img = document.createElement('img'); img.src = file.dataUrl; img.alt = ''; preview.append(img); }
      const label = document.createElement('span'); label.textContent = file.name || 'Изображение'; preview.append(label); chip.append(preview);
      const button = document.createElement('button'); button.type = 'button'; button.textContent = '×'; button.setAttribute('aria-label', 'Убрать ' + label.textContent);
      button.disabled = !!file.loading;
      button.addEventListener('click', () => remove(file.id)); chip.append(button); root.append(chip);
    }
  }
  // The native blur handler must know a picker is open before it can hide the
  // quick panel. Preserve the input's synchronous click/user activation.
  function pick(input) {
    const finish = () => { input.removeEventListener('change', finish); input.removeEventListener('cancel', finish); window.removeEventListener('focus', focused); Promise.resolve(window.jarvis.fileDialogState?.(false)).catch(() => {}); };
    const focused = () => setTimeout(finish, 300);
    input.addEventListener('change', finish, { once: true }); input.addEventListener('cancel', finish, { once: true }); window.addEventListener('focus', focused, { once: true });
    Promise.resolve(window.jarvis.fileDialogState?.(true)).catch(() => finish());
    try { input.click(); } catch (error) { finish(); throw error; }
  }
  function bind(root, add) {
    root.addEventListener('paste', event => {
      const files = [...(event.clipboardData?.items || [])].filter(item => item.kind === 'file').map(item => item.getAsFile()).filter(Boolean);
      if (files.length) { event.preventDefault(); files.forEach(add); }
    });
    root.addEventListener('dragover', event => { if ([...(event.dataTransfer?.types || [])].includes('Files')) { event.preventDefault(); root.dataset.dragging = 'true'; } });
    root.addEventListener('dragleave', event => { if (!root.contains(event.relatedTarget)) delete root.dataset.dragging; });
    root.addEventListener('drop', event => {
      delete root.dataset.dragging;
      const files = [...(event.dataTransfer?.files || [])];
      if (files.length) { event.preventDefault(); files.forEach(add); }
    });
  }
  async function save(files, machine, progress = () => {}) {
    let completed = 0; if (files.length) progress(0, files.length);
    const results = await Promise.allSettled(files.map(async file => {
      const result = await window.jarvis.saveAttachment(file.dataUrl.slice(file.dataUrl.indexOf(',') + 1), file.name || `image.${file.ext || 'png'}`, machine || 'local');
      if (!result?.ok || !result.path) throw new Error(result?.error || 'Не удалось загрузить файл');
      file.path = result.path; file.machine = machine || 'local';
      progress(++completed, files.length);
      return result.path;
    }));
    const failure = results.find(result => result.status === 'rejected');
    if (failure) throw failure.reason;
    return results.map(result => result.value);
  }
  const prompt = (text, paths) => paths.length ? [text, 'Вложения (пути к файлам на машине агента):', ...paths.map(path => JSON.stringify(path))].filter(Boolean).join('\n') : text;
  window.JarvisAttachments = { read, render, bind, save, prompt, pick };
})();
