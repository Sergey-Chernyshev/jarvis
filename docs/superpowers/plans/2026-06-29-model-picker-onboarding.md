# Выбор и мультизагрузка моделей — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Дать выбор моделей (Whisper, Qwen 0.6/1.7, wake-word, Silero) и их мультизагрузку с прогрессом — в онбординге и в панели «Модели».

**Architecture:** Единый backend-оркестратор `models_install(ids)` качает выбранные модели последовательно в фоне, переиспользуя существующие загрузчики, и шлёт единые события с `id` модели. Чистая функция `plan_install` строит план (раскрытие qwen→runtime, порядок, дедуп) и покрыта юнит-тестами. UI (онбординг + панель) рисует прогресс по `id`.

**Tech Stack:** Rust + Tauri 2 (события `emit_to`), ванильный JS (ui/bridge.js, onboarding, settings2).

---

## Файловая структура

- `src-tauri/src/install/mod.rs` — каталог/план (`Installed`, `InstallTask`, `plan_install`), `installed_state()`, `run_install_task()` + тесты.
- `src-tauri/src/onboarding.rs` — команда `models_install` + эмиссия событий.
- `src-tauri/src/main.rs` — регистрация команды.
- `ui/bridge.js` — `modelsInstall` + подписки на события.
- `ui/onboarding.html` / `ui/onboarding.js` — шаг выбора моделей.
- `ui/settings2.js` — чекбоксы + «Скачать выбранное» + маршрутизация прогресса по `id`.

---

## Task 1: Чистый планировщик `plan_install` (Rust, TDD)

**Files:**

- Modify: `src-tauri/src/install/mod.rs` (рядом со `status()`/загрузчиками)
- Test: тот же файл, `#[cfg(test)] mod plan_tests`

- [ ] **Step 1: Написать падающий тест**

В конце `src-tauri/src/install/mod.rs` (внутри существующего `mod tests` или новым модулем):

```rust
#[cfg(test)]
mod plan_tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn qwen_selection_pulls_runtime_first_and_dedups() {
        let inst = Installed::default(); // ничего не установлено
        let plan = plan_install(&ids(&["qwen3-0.6b"]), &inst);
        let order: Vec<&str> = plan.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(order, vec!["qwen3-runtime", "qwen3-0.6b"]);
    }

    #[test]
    fn skips_already_installed() {
        let inst = Installed { whisper: true, runtime: true, qwen_0_6b: true, ..Default::default() };
        let plan = plan_install(&ids(&["whisper-turbo", "qwen3-0.6b"]), &inst);
        assert!(plan.is_empty(), "уже установленное не планируется: {plan:?}");
    }

    #[test]
    fn runtime_not_readded_when_present() {
        let inst = Installed { runtime: true, ..Default::default() };
        let plan = plan_install(&ids(&["qwen3-1.7b"]), &inst);
        let order: Vec<&str> = plan.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(order, vec!["qwen3-1.7b"]); // venv уже есть → без runtime-шага
    }

    #[test]
    fn canonical_order_full_selection() {
        let inst = Installed::default();
        let plan = plan_install(&ids(&["hey_jarvis", "silero", "whisper-turbo", "qwen3-0.6b"]), &inst);
        let order: Vec<&str> = plan.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(order, vec!["qwen3-runtime", "whisper-turbo", "qwen3-0.6b", "silero", "hey_jarvis"]);
    }
}
```

- [ ] **Step 2: Прогнать тест — убедиться, что не компилируется/падает**

Run: `cargo test --manifest-path src-tauri/Cargo.toml plan_tests`
Expected: ошибка компиляции (`Installed`, `InstallTask`, `plan_install` не определены).

- [ ] **Step 3: Реализовать типы и функцию**

Добавить в `src-tauri/src/install/mod.rs` (например, после `impl Status`):

```rust
/// Снимок «что уже установлено» — вход планировщика (чистый, без обращения к FS,
/// чтобы план тестировался без сети/диска).
#[derive(Debug, Clone, Default)]
pub struct Installed {
    pub whisper: bool,
    pub silero: bool,
    pub wake: bool,
    pub runtime: bool, // qwen3-MLX venv (рантайм для весов Qwen)
    pub qwen_0_6b: bool,
    pub qwen_1_7b: bool,
}

/// Один шаг плана установки: `id` совпадает с id строки модели в UI (маршрутизация
/// прогресса), `kind` — группа панели (stt|wake|voice|runtime).
#[derive(Debug, Clone, PartialEq)]
pub struct InstallTask {
    pub id: String,
    pub kind: &'static str,
}

/// Построить план установки из выбранных id: раскрывает зависимость qwen→runtime
/// (venv первым), даёт канонический порядок и выкидывает уже установленное.
pub fn plan_install(ids: &[String], inst: &Installed) -> Vec<InstallTask> {
    let want = |x: &str| ids.iter().any(|i| i == x);
    let mut tasks = Vec::new();
    let needs_qwen = (want("qwen3-0.6b") && !inst.qwen_0_6b) || (want("qwen3-1.7b") && !inst.qwen_1_7b);
    if needs_qwen && !inst.runtime {
        tasks.push(InstallTask { id: "qwen3-runtime".into(), kind: "runtime" });
    }
    if want("whisper-turbo") && !inst.whisper {
        tasks.push(InstallTask { id: "whisper-turbo".into(), kind: "stt" });
    }
    if want("qwen3-0.6b") && !inst.qwen_0_6b {
        tasks.push(InstallTask { id: "qwen3-0.6b".into(), kind: "stt" });
    }
    if want("qwen3-1.7b") && !inst.qwen_1_7b {
        tasks.push(InstallTask { id: "qwen3-1.7b".into(), kind: "stt" });
    }
    if want("silero") && !inst.silero {
        tasks.push(InstallTask { id: "silero".into(), kind: "voice" });
    }
    if want("hey_jarvis") && !inst.wake {
        tasks.push(InstallTask { id: "hey_jarvis".into(), kind: "wake" });
    }
    tasks
}
```

- [ ] **Step 4: Прогнать тесты — зелёные**

Run: `cargo test --manifest-path src-tauri/Cargo.toml plan_tests`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/install/mod.rs
git commit -m "feat(install): чистый планировщик plan_install с тестами"
```

---

## Task 2: `installed_state()` + `run_install_task()` (Rust)

**Files:**

- Modify: `src-tauri/src/install/mod.rs`

- [ ] **Step 1: Реализовать обёртки над статусом и загрузчиками**

Добавить в `src-tauri/src/install/mod.rs`:

```rust
/// Текущее «что установлено» из реального статуса (для оркестратора).
pub fn installed_state() -> Installed {
    let s = status();
    Installed {
        whisper: s.whisper_model,
        silero: s.silero,
        wake: s.wakeword_models,
        runtime: s.qwen3_sidecar,
        qwen_0_6b: qwen_weights_present("qwen3-0.6b"),
        qwen_1_7b: qwen_weights_present("qwen3-1.7b"),
    }
}

/// Запустить установку одной модели по id (маршрутизация на готовый загрузчик).
pub fn run_install_task(id: &str, progress: &Progress, proxy: Option<&str>) -> Result<(), String> {
    match id {
        "qwen3-runtime" => install_stt_sidecar(progress, proxy),
        "whisper-turbo" => install_whisper(progress, proxy),
        "qwen3-0.6b" => preload_qwen("qwen3-0.6b", progress, proxy),
        "qwen3-1.7b" => preload_qwen("qwen3-1.7b", progress, proxy),
        "hey_jarvis" => install_wakeword(progress, proxy),
        "silero" => install_silero(progress, proxy),
        other => Err(format!("неизвестная модель: {other}")),
    }
}
```

- [ ] **Step 2: Проверить компиляцию**

Run: `cargo build --manifest-path src-tauri/Cargo.toml --features wakeword-ort,whisper-native,stt-vad`
Expected: успех (все вызываемые функции уже `pub`).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/install/mod.rs
git commit -m "feat(install): installed_state + run_install_task для оркестратора"
```

---

## Task 3: Команда `models_install` + единые события (Rust)

**Files:**

- Modify: `src-tauri/src/onboarding.rs`
- Modify: `src-tauri/src/main.rs:192-200` (регистрация)

- [ ] **Step 1: Добавить команду в `onboarding.rs`**

```rust
/// Скачать набор моделей последовательно в фоне. Единые события:
/// `model_install_progress {id, step}`, `model_install_done {id, ok, error}`,
/// в конце `models_install_all_done`. Сбой одной модели не прерывает очередь.
#[tauri::command]
pub fn models_install(app: AppHandle, ids: Vec<String>) {
    let d = crate::daemon::Daemon::get(&app);
    let proxy = d.settings.proxy();
    std::thread::spawn(move || {
        let plan = install::plan_install(&ids, &install::installed_state());
        for task in &plan {
            let app_p = app.clone();
            let id_p = task.id.clone();
            let prog = move |step: Step| {
                let _ = app_p.emit_to(
                    "main",
                    "model_install_progress",
                    serde_json::json!({ "id": id_p, "step": step }),
                );
            };
            let r = install::run_install_task(&task.id, &prog, proxy.as_deref());
            let _ = app.emit_to(
                "main",
                "model_install_done",
                serde_json::json!({ "id": task.id, "ok": r.is_ok(), "error": r.err() }),
            );
        }
        let _ = app.emit_to("main", "models_install_all_done", serde_json::json!({}));
    });
}
```

- [ ] **Step 2: Зарегистрировать команду в `main.rs`**

В `tauri::generate_handler![...]` после `onboarding::voice_install_silero,` добавить строку:

```rust
            onboarding::models_install,
```

- [ ] **Step 3: Сборка**

Run: `cargo build --manifest-path src-tauri/Cargo.toml --features wakeword-ort,whisper-native,stt-vad`
Expected: успех.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/onboarding.rs src-tauri/src/main.rs
git commit -m "feat(onboarding): команда models_install + единые события прогресса"
```

---

## Task 4: JS-API в `bridge.js`

**Files:**

- Modify: `ui/bridge.js` (рядом с другими model-install методами и подписками)

- [ ] **Step 1: Добавить метод и подписки**

В объект API (рядом с `voiceInstallSilero`/`sttInstall*`):

```js
    modelsInstall: (ids) => invoke('models_install', { ids }),
```

Рядом с подписками `onSttInstallProgress`/`onSttInstallDone` (тем же шаблоном `listen`):

```js
    onModelInstallProgress: (cb) => listen('model_install_progress', (e) => cb(e.payload)),
    onModelInstallDone: (cb) => listen('model_install_done', (e) => cb(e.payload)),
    onModelsInstallAllDone: (cb) => listen('models_install_all_done', (e) => cb(e.payload)),
```

(Если `listen` импортируется локально в существующих `onSttInstall*` — повторить тот же способ получения `listen`.)

- [ ] **Step 2: Проверить наличие методов**

Run: `grep -n "modelsInstall\|onModelInstallProgress\|onModelsInstallAllDone" ui/bridge.js`
Expected: 4 строки.

- [ ] **Step 3: Commit**

```bash
git add ui/bridge.js
git commit -m "feat(ui): bridge API modelsInstall + подписки на единые события"
```

---

## Task 5: Маршрутизация прогресса по `id` в панели «Модели»

**Files:**

- Modify: `ui/settings2.js` (подписки в `subscribeOnce`, ~1512-1544; строки моделей ~876-908, ~991-995)

- [ ] **Step 1: Подписать единые события (по id, без `activeDownload`)**

В `subscribeOnce()` добавить (рядом с существующими `onSttInstall*`):

```js
try {
  window.jarvis.onModelInstallProgress(({ id, step }) => {
    if (!currentRoot || !id) return;
    const h = currentRoot.querySelector('[data-model="' + id + '"]');
    if (!h) return;
    h.textContent = "";
    if (step && step.msg) h.appendChild(el("span.loadcap", { text: step.msg }));
    const pct = step && typeof step.pct === "number" ? step.pct : null;
    if (pct != null) h.appendChild(progressBar(pct));
  });
} catch (e) {}
try {
  window.jarvis.onModelInstallDone(({ id, ok, error }) => {
    if (!ok)
      dlState[id] = {
        error:
          error || "неизвестная ошибка (подробности в ~/.jarvis/jarvis.log)",
      };
    else delete dlState[id];
  });
} catch (e) {}
try {
  window.jarvis.onModelsInstallAllDone(() => {
    reRenderPane("stt");
    reRenderPane("wake");
    reRenderPane("voice");
  });
} catch (e) {}
```

- [ ] **Step 2: Перевести кнопки строк на `modelsInstall([id])`**

В `modelRow` (≈882) заменить обработчик клика кнопки: вместо `activeDownload = m.id; … await safe(action.run, …)` использовать:

```js
btn.addEventListener("click", async () => {
  delete dlState[m.id];
  btn.disabled = true;
  btn.replaceChildren(document.createTextNode("Качаю…"));
  await safe(() => window.jarvis.modelsInstall([m.id]), null);
});
```

Аналогично для wake-кнопки (≈993): `await safe(() => window.jarvis.modelsInstall(['hey_jarvis']), null)`. Поле `activeDownload` и `finishDownload` больше не используются этим путём — оставить старые `onSttInstallDone`/`onWakeInstallDone` подписки можно, но новые `done` маршрутизируют по id.

- [ ] **Step 3: Чекбоксы + «Скачать выбранное» над группами STT/voice/wake**

В `render*` секции моделей (где строятся группы, ≈832): для не-установленных моделей добавить чекбокс в строку и кнопку группы. Минимально:

```js
const selected = new Set();
function bulkBar(groupIds) {
  const b = el("button.btn.sm", null, [
    iconSpan("download"),
    document.createTextNode("Скачать выбранное"),
  ]);
  b.addEventListener("click", async () => {
    const ids = groupIds.filter((id) => selected.has(id));
    if (!ids.length) return;
    b.disabled = true;
    b.replaceChildren(document.createTextNode("Качаю…"));
    await safe(() => window.jarvis.modelsInstall(ids), null);
  });
  return b;
}
```

В `modelRow` для не-установленной модели добавить перед кнопкой чекбокс, который кладёт/убирает `m.id` в `selected`. Кнопку `bulkBar([...не-установленные id...])` положить в заголовок группы (≈835).

- [ ] **Step 4: Проверка вручную (компиляция UI не требуется — статика)**

Run: `node -e "require('fs').readFileSync('ui/settings2.js','utf8')" && echo OK`
Expected: OK (файл синтаксически читается; полноценная проверка — в Task 7 запуском приложения).

- [ ] **Step 5: Commit**

```bash
git add ui/settings2.js
git commit -m "feat(ui): мультивыбор моделей + прогресс по id в панели Модели"
```

---

## Task 6: Шаг выбора моделей в онбординге

**Files:**

- Modify: `ui/onboarding.html` (новая секция чеклиста)
- Modify: `ui/onboarding.js` (рендер чеклиста, кнопка «Скачать выбранное», прогресс по id)

- [ ] **Step 1: Разметка чеклиста в `onboarding.html`**

Добавить секцию (скрытую по умолчанию), рядом с `#steps`:

```html
<div id="models" hidden>
  <div class="models-title">
    Модели (можно скачать сейчас или позже в настройках)
  </div>
  <label class="mrow"
    ><input type="checkbox" id="m-whisper" checked /> Whisper large-v3-turbo
    <span class="msize">~574 МБ</span></label
  >
  <label class="mrow"
    ><input type="checkbox" id="m-qwen" /> Qwen3-ASR
    <span class="msize">~1 ГБ</span>
    <select id="m-qwen-size">
      <option value="qwen3-0.6b">0.6B</option>
      <option value="qwen3-1.7b">1.7B</option>
    </select></label
  >
  <label class="mrow"
    ><input type="checkbox" id="m-wake" checked /> Голосовая активация
    <span class="msize">~3.5 МБ</span></label
  >
  <label class="mrow"
    ><input type="checkbox" id="m-silero" checked /> Голос Silero
    <span class="msize">~1 ГБ</span></label
  >
  <button id="models-go" class="btn">Скачать выбранное</button>
  <div id="models-progress"></div>
</div>
```

- [ ] **Step 2: Логика в `onboarding.js`**

После `onboarding:done` (ядро установлено) показать `#models`. Собрать id из чекбоксов, по клику `#models-go` вызвать `invoke('models_install', { ids })`, а прогресс рисовать по событиям:

```js
const modelsEl = document.getElementById("models");
const modelsGo = document.getElementById("models-go");
const modelsProg = document.getElementById("models-progress");

function selectedModelIds() {
  const ids = [];
  if (document.getElementById("m-whisper").checked) ids.push("whisper-turbo");
  if (document.getElementById("m-qwen").checked)
    ids.push(document.getElementById("m-qwen-size").value);
  if (document.getElementById("m-wake").checked) ids.push("hey_jarvis");
  if (document.getElementById("m-silero").checked) ids.push("silero");
  return ids;
}
modelsGo.addEventListener("click", () => {
  const ids = selectedModelIds();
  if (!ids.length) return;
  modelsGo.disabled = true;
  modelsGo.textContent = "Качаю…";
  invoke("models_install", { ids });
});
listen("model_install_progress", (e) => {
  const { id, step } = e.payload;
  let row = modelsProg.querySelector('[data-mid="' + id + '"]');
  if (!row) {
    row = document.createElement("div");
    row.dataset.mid = id;
    modelsProg.appendChild(row);
  }
  const pct = step && typeof step.pct === "number" ? step.pct : null;
  row.textContent =
    id + ": " + (step.msg || "") + (pct != null ? " " + pct + "%" : "");
});
listen("model_install_done", (e) => {
  const { id, ok, error } = e.payload;
  let row = modelsProg.querySelector('[data-mid="' + id + '"]');
  if (!row) {
    row = document.createElement("div");
    row.dataset.mid = id;
    modelsProg.appendChild(row);
  }
  row.textContent =
    id + ": " + (ok ? "готово ✓" : "ошибка — " + (error || "см. логи"));
});
listen("models_install_all_done", () => {
  modelsGo.disabled = false;
  modelsGo.textContent = "Скачать выбранное";
});
```

В `showDone()` добавить `modelsEl.hidden = false;` (показать чеклист после ядра).

- [ ] **Step 3: Проверка синтаксиса**

Run: `node -e "require('fs').readFileSync('ui/onboarding.js','utf8')" && echo OK`
Expected: OK.

- [ ] **Step 4: Commit**

```bash
git add ui/onboarding.html ui/onboarding.js
git commit -m "feat(onboarding): шаг выбора и мультизагрузки моделей с прогрессом"
```

---

## Task 7: Сборка, прогон тестов, ручная проверка

- [ ] **Step 1: Полный прогон тестов**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: все зелёные (включая `plan_tests`, `proxy_tests`).

- [ ] **Step 2: Сборка приложения (как CI/npm start)**

Run: `cargo build --manifest-path src-tauri/Cargo.toml --features wakeword-ort,whisper-native,stt-vad --bin jarvis`
Expected: успех.

- [ ] **Step 3: Ручная проверка (dev)**

Run: `npm start` → открыть онбординг (Переустановить) → выбрать модели → «Скачать выбранное»; убедиться, что строки прогресса заполняются, окно закрываемо; в панели «Модели» — чекбоксы + «Скачать выбранное», прогресс идёт в строки.
Expected: загрузки идут через прокси (если задан), прогресс виден, ошибки одной модели не валят остальные.

- [ ] **Step 4: Финальный commit (если были правки по итогам проверки)**

```bash
git add -A && git commit -m "test: ручная проверка мультизагрузки моделей"
```

---

## Self-review

- **Spec coverage:** выбор моделей (Task 6 онбординг, Task 5 панель) ✓; «все 4 типа» (каталог в Task 1/3) ✓; мультизагрузка/«скачать всё» (Task 3 оркестратор, Task 5/6 кнопки) ✓; прогресс (единые события Task 3, рендер Task 5/6) ✓; фон/закрываемо (поток в Task 3, окно не блокируется) ✓; сбой не валит очередь (цикл без break, Task 3) ✓; прокси (`d.settings.proxy()` Task 3) ✓; идемпотентность/дедуп (`plan_install` Task 1) ✓.
- **Placeholder scan:** код приведён во всех code-шагах; UI-задачи (5/6) описывают точные вставки и id.
- **Type consistency:** `InstallTask{id,kind}`, `Installed{...}`, событие `model_install_progress{id,step}` / `model_install_done{id,ok,error}` — имена совпадают между backend (Task 3) и JS (Task 4/5/6).
