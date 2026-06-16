/* Окно онбординга: статус интеграции + запуск установки со стримом шагов.
 * Поверх Tauri global API (withGlobalTauri). Бэкенд — команды onboarding_* и
 * события onboarding:progress / onboarding:done (см. src/onboarding.rs). */

(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const W = window.__TAURI__.window;
  const appWindow = (W.getCurrentWindow || W.getCurrent)();

  const PHASES = [
    { key: "Хуки", name: "Хуки Claude Code", desc: "События сессий → Jarvis" },
    { key: "Транспорт", name: "Транспорт запуска", desc: "Шим claude + tmux" },
    { key: "Голос", name: "Голос Silero", desc: "Синтез речи (PyTorch)" },
  ];

  const stepsEl = document.getElementById("steps");
  const titleEl = document.getElementById("title");
  const subEl = document.getElementById("subtitle");
  const hintEl = document.getElementById("hint");
  const proxyEl = document.getElementById("proxy");
  const btn = document.getElementById("action");
  function closeWindow() {
    invoke("onboarding_close").catch(() => { try { appWindow.close(); } catch {} });
  }
  document.getElementById("close").addEventListener("click", closeWindow);

  // безопасный многострочный текст (без innerHTML)
  function setLines(el, lines) {
    el.textContent = "";
    lines.forEach((ln, i) => {
      if (i) el.appendChild(document.createElement("br"));
      el.appendChild(document.createTextNode(ln));
    });
  }

  // строим строки фаз
  const rowByKey = {};
  for (const p of PHASES) {
    const row = document.createElement("div");
    row.className = "step";
    row.dataset.state = "pending";
    const ico = document.createElement("div");
    ico.className = "ico";
    const meta = document.createElement("div");
    meta.className = "meta";
    const name = document.createElement("div");
    name.className = "name";
    name.textContent = p.name;
    const desc = document.createElement("div");
    desc.className = "desc";
    desc.textContent = p.desc;
    meta.appendChild(name);
    meta.appendChild(desc);
    row.appendChild(ico);
    row.appendChild(meta);
    stepsEl.appendChild(row);
    rowByKey[p.key] = { row, desc };
  }

  function setRow(key, state, msg) {
    const r = rowByKey[key];
    if (!r) return;
    r.row.dataset.state = state;
    if (msg) r.desc.textContent = msg;
  }

  let running = false;

  function startRun() {
    if (running) return;
    running = true;
    for (const p of PHASES) setRow(p.key, "pending", p.desc);
    btn.disabled = true;
    btn.textContent = "Устанавливаю…";
    btn.classList.remove("ok");
    hintEl.textContent = "";
    invoke("onboarding_run", { proxy: (proxyEl.value || "").trim() });
  }

  // прогресс установки
  listen("onboarding:progress", (e) => {
    const s = e.payload; // { phase, state, msg }
    if (s.state === "start") setRow(s.phase, "run", "устанавливаю…");
    else if (s.state === "info") {
      setRow(s.phase, "run", s.msg);
      if (s.phase === "Голос") hintEl.textContent = "Silero тянет PyTorch — это может занять несколько минут.";
    } else if (s.state === "done") setRow(s.phase, "done", s.msg);
    else if (s.state === "warn") setRow(s.phase, "warn", s.msg);
  });

  // завершение
  listen("onboarding:done", () => {
    running = false;
    titleEl.textContent = "Готово!";
    setLines(subEl, ["Jarvis подключён к Claude Code.", "Перезапусти активные сессии — и всё заработает."]);
    hintEl.textContent = "";
    btn.disabled = false;
    btn.textContent = "Закрыть";
    btn.classList.add("ok");
    btn.onclick = closeWindow;
  });

  // первичное состояние
  async function init() {
    let info = null;
    try { info = await invoke("integration_get"); } catch {}
    const st = info && info.status;
    if (info && info.proxy) proxyEl.value = info.proxy; // префилл сохранённого прокси
    const integrated = st && st.hooks && st.shim; // мирроринг Status::integrated()
    if (integrated) {
      titleEl.textContent = "Jarvis настроен";
      setLines(subEl, ["Интеграция с Claude Code на месте.", "Можно переустановить, если что-то сломалось."]);
      for (const p of PHASES) setRow(p.key, "done", "установлено");
      if (st && !st.silero) setRow("Голос", "warn", "Silero не установлен");
      btn.textContent = "Переустановить";
    } else {
      btn.textContent = "Настроить";
    }
    btn.onclick = startRun;
  }

  init();
})();
