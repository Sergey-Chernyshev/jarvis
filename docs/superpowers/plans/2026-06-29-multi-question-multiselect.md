# Мультивыбор и несколько вопросов подряд — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Поддержать мультивыбор и несколько вопросов подряд в пикерах Claude (`AskUserQuestion`) и Codex, чтобы ответ корректно доходил до агента и не ломал его экран.

**Architecture:** Контракт ответа обобщается с плоского `{indices, multiSelect}` до по-вопросного `{answers: number[][]}`. Раскладка клавиш вынесена в чистую функцию `answer_keys(agent, question, answers) -> Vec<String>`, ветвящуюся по агенту (Claude — цифры; Codex — стрелки/Space/Enter) и по позиции вопроса. UI становится визардом «по одному вопросу». Точные клавиши пикеров подтверждаются живым прогоном (Task 8).

**Tech Stack:** Rust (Tauri бэкенд, `cargo test`), ванильный JS (UI, ручная верификация — JS-тестов в репозитории нет).

**Спек:** `docs/superpowers/specs/2026-06-29-multi-question-multiselect-design.md`

---

## Структура файлов

- `src-tauri/src/tmux.rs` — новая чистая функция `answer_keys` + рефактор `answer_question` (новая сигнатура) + `#[cfg(test)]` модуль с тестами раскладки.
- `src-tauri/src/ipc.rs` — `question_answer`: новый контракт `answers`, обратная совместимость, агент, валидация.
- `src-tauri/src/daemon.rs` — `answer_question_hotkey`: новый контракт `{answers:[[n]]}`.
- `ui/renderer.js` — визард по вопросам (полноэкранный `qview` и слайд-овер `varPanel`).
- `ui/toast.js` — карточка-тост: инлайн-чипы только для одиночного вопроса; для нескольких — подсказка отвечать в приложении.
- `ui/toast-bridge.js` — прозрачная передача `choice`.
- `ui/index.html` — индикатор прогресса «2/4» в шапке вопроса (опционально, в Task 5).

Модель (`src-tauri/src/model.rs`) **не меняется** — `Question.questions: Vec<QuestionItem>` уже несёт всё нужное.

---

## Task 1: Чистая функция раскладки клавиш `answer_keys`

**Files:**
- Modify: `src-tauri/src/tmux.rs` (добавить функцию + тест-модуль; рядом с `answer_question` ~строка 151)

Чистая функция строит последовательность tmux-клавиш для ответа на один или
несколько вопросов. Тестируется без живого tmux. Задержки между клавишами
добавляет уже исполнитель (`answer_question`, Task 2) — здесь только клавиши.

- [ ] **Step 1: Написать падающие тесты**

Добавить в конец `src-tauri/src/tmux.rs`:

```rust
#[cfg(test)]
mod answer_keys_tests {
    use super::*;
    use crate::backend::Agent;
    use crate::model::{Question, QuestionItem, QuestionOption};

    fn item(multi: bool, n: usize) -> QuestionItem {
        QuestionItem {
            question: "q".into(),
            header: String::new(),
            multi_select: multi,
            options: (0..n)
                .map(|i| QuestionOption { label: format!("o{i}"), description: String::new() })
                .collect(),
        }
    }
    fn q(items: Vec<QuestionItem>) -> Question {
        Question { at: 0, from_screen: false, questions: items }
    }

    #[test]
    fn claude_single_question_single_select_just_digit() {
        let keys = answer_keys(Agent::Claude, &q(vec![item(false, 3)]), &[vec![2]]);
        assert_eq!(keys, vec!["2".to_string()]);
    }

    #[test]
    fn claude_single_question_multi_select_toggles_then_submit() {
        let keys = answer_keys(Agent::Claude, &q(vec![item(true, 3)]), &[vec![1, 3]]);
        assert_eq!(
            keys,
            vec!["1".to_string(), "3".to_string(), CLAUDE_SUBMIT_RIGHT.to_string(), "1".to_string()]
        );
    }

    #[test]
    fn claude_multi_question_advance_between_then_submit() {
        let keys = answer_keys(
            Agent::Claude,
            &q(vec![item(false, 3), item(false, 2)]),
            &[vec![2], vec![1]],
        );
        assert_eq!(
            keys,
            vec![
                "2".to_string(),
                CLAUDE_ADVANCE.to_string(),
                "1".to_string(),
                CLAUDE_SUBMIT.to_string(),
            ]
        );
    }

    #[test]
    fn codex_single_select_navigates_down_then_enter() {
        // выбрана опция 3 → вниз дважды от подсветки на опции 1, затем Enter
        let keys = answer_keys(Agent::Codex, &q(vec![item(false, 4)]), &[vec![3]]);
        assert_eq!(keys, vec!["Down".to_string(), "Down".to_string(), "Enter".to_string()]);
    }

    #[test]
    fn codex_multi_select_space_at_each_then_enter() {
        // выбраны опции 1 и 3: Space на 1 (курсор уже там), вниз×2, Space на 3, Enter
        let keys = answer_keys(Agent::Codex, &q(vec![item(true, 4)]), &[vec![1, 3]]);
        assert_eq!(
            keys,
            vec![
                "Space".to_string(),
                "Down".to_string(),
                "Down".to_string(),
                "Space".to_string(),
                "Enter".to_string(),
            ]
        );
    }
}
```

- [ ] **Step 2: Запустить тесты — убедиться, что не компилируется/падает**

Run: `cargo test --manifest-path src-tauri/Cargo.toml answer_keys_tests`
Expected: FAIL — `cannot find function answer_keys` и константы не определены.

- [ ] **Step 3: Реализовать `answer_keys` и константы клавиш**

Добавить в `src-tauri/src/tmux.rs` перед `answer_question` (~строка 148):

```rust
// Клавиши пикеров. ВЫНЕСЕНЫ В КОНСТАНТЫ намеренно: точные коды подтверждаются
// живым прогоном (см. план Task 8) — правка здесь же чинит и логику, и тесты.
const CLAUDE_ADVANCE: &str = "Tab";       // переход к следующему вопросу мульти-вопроса
const CLAUDE_SUBMIT: &str = "Enter";      // финальный сабмит мульти-вопроса
const CLAUDE_SUBMIT_RIGHT: &str = "Right"; // Submit-таб одиночного multiSelect-вопроса

/// Плоская последовательность tmux send-keys для ответа на вопрос(ы).
/// Чистая и детерминированная — тестируется без tmux. `answers[i]` — выбранные
/// опции (1-based) вопроса `i`. Ветвится по агенту и по позиции вопроса.
pub fn answer_keys(agent: crate::backend::Agent, q: &crate::model::Question, answers: &[Vec<u32>]) -> Vec<String> {
    use crate::backend::Agent;
    let mut keys = Vec::new();
    let n_q = q.questions.len();

    match agent {
        Agent::Claude => {
            // Быстрый путь: один вопрос, single-select — цифра авто-подтверждает.
            if n_q == 1 && !q.questions[0].multi_select {
                if let Some(i) = answers.first().and_then(|a| a.first()) {
                    keys.push(i.to_string());
                }
                return keys;
            }
            // Один вопрос, multiSelect — тоггл цифр, затем Submit-таб и «1».
            if n_q == 1 {
                for i in answers.first().map(Vec::as_slice).unwrap_or(&[]) {
                    keys.push(i.to_string());
                }
                keys.push(CLAUDE_SUBMIT_RIGHT.to_string());
                keys.push("1".to_string());
                return keys;
            }
            // Несколько вопросов: на каждый — цифры выбора, между вопросами —
            // переход, в конце — финальный сабмит.
            for (idx, item) in q.questions.iter().enumerate() {
                let _ = item; // multiSelect внутри вопроса = те же тогглы цифрами
                for i in answers.get(idx).map(Vec::as_slice).unwrap_or(&[]) {
                    keys.push(i.to_string());
                }
                if idx + 1 < n_q {
                    keys.push(CLAUDE_ADVANCE.to_string());
                }
            }
            keys.push(CLAUDE_SUBMIT.to_string());
        }
        Agent::Codex => {
            // Codex всегда один вопрос (скрин-скрейп). Навигация стрелками от
            // подсветки на опции 1; Space тогглит в мультивыборе; Enter подтверждает.
            let item_multi = q.questions.first().map(|x| x.multi_select).unwrap_or(false);
            let mut targets: Vec<u32> =
                answers.first().cloned().unwrap_or_default();
            targets.sort_unstable();
            let mut cursor: u32 = 1; // подсветка стартует на опции 1
            for t in targets {
                for _ in cursor..t {
                    keys.push("Down".to_string());
                }
                cursor = t;
                if item_multi {
                    keys.push("Space".to_string());
                }
            }
            keys.push("Enter".to_string());
        }
    }
    keys
}
```

- [ ] **Step 4: Запустить тесты — убедиться, что проходят**

Run: `cargo test --manifest-path src-tauri/Cargo.toml answer_keys_tests`
Expected: PASS (5 тестов).

- [ ] **Step 5: Коммит**

```bash
git add src-tauri/src/tmux.rs
git commit -m "feat(tmux): чистая раскладка клавиш answer_keys (Claude/Codex, мульти-вопрос)"
```

---

## Task 2: Рефактор `tmux::answer_question` под новую сигнатуру

**Files:**
- Modify: `src-tauri/src/tmux.rs:151-164`

Исполнитель просто проигрывает `answer_keys` с задержкой между клавишами.

- [ ] **Step 1: Заменить тело `answer_question`**

Заменить целиком функцию `answer_question` (строки 148-164):

```rust
/// Ответ на вопрос(ы) клавишами в пану. Раскладку строит `answer_keys`
/// (чистая, протестирована); здесь — только проигрывание с задержками.
pub async fn answer_question(
    pane: &str,
    agent: crate::backend::Agent,
    q: &crate::model::Question,
    answers: &[Vec<u32>],
) -> Result<(), String> {
    let keys = answer_keys(agent, q, answers);
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            sleep(Duration::from_millis(140)).await; // дать пикеру перерисоваться
        }
        tmux_j(&["send-keys", "-t", pane, k]).await?;
    }
    Ok(())
}
```

- [ ] **Step 2: Собрать — ожидается ошибка у вызова в ipc.rs**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: FAIL — `ipc.rs` вызывает старую сигнатуру `answer_question(&pane, &indices, multi)`. Чиним в Task 3.

- [ ] **Step 3: Коммит (вместе с Task 3)**

Коммит этой задачи делается после Task 3, когда дерево снова компилируется.

---

## Task 3: `ipc::question_answer` — новый контракт `answers`

**Files:**
- Modify: `src-tauri/src/ipc.rs:490-532`

Принять `choice.answers: number[][]`; для совместимости — старый
`choice.indices` завернуть как `[indices]`. Достать агента из сессии,
провалидировать каждый выбор против опций своего вопроса.

- [ ] **Step 1: Заменить тело `question_answer`**

Заменить функцию целиком (строки 491-532):

```rust
/// Ответ на AskUserQuestion/пикер клавишами в пану.
/// `choice` = `{ answers: number[][] }` (answers[i] — опции 1-based вопроса i).
/// Обратная совместимость: `{ indices, multiSelect }` → `answers = [indices]`.
#[tauri::command]
pub async fn question_answer(app: AppHandle, session_id: String, choice: Value) -> Value {
    let d = Daemon::get(&app);
    let Some(s) = d.session(&session_id) else { return err("Вопрос уже неактуален") };
    let Some(q) = s.question.clone() else { return err("Вопрос уже неактуален") };
    let Some(pane) = s.tmux_pane else { return err("Сессия вне tmux — ответь в терминале") };
    if !tmux::pane_alive(&pane).await {
        return err("Пана сессии не отвечает");
    }

    // парсинг массива выборов вопроса в Vec<u32> (1-based, >0)
    let parse_row = |v: &Value| -> Vec<u32> {
        v.as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_u64)
                    .filter(|&n| n >= 1)
                    .map(|n| n as u32)
                    .collect()
            })
            .unwrap_or_default()
    };

    // новый контракт answers[][] либо старый indices[] → [indices]
    let answers: Vec<Vec<u32>> = if let Some(rows) = choice.get("answers").and_then(Value::as_array) {
        rows.iter().map(parse_row).collect()
    } else if let Some(idx) = choice.get("indices") {
        vec![parse_row(idx)]
    } else {
        Vec::new()
    };

    if answers.is_empty() || answers.iter().all(Vec::is_empty) {
        return err("Пустой выбор");
    }
    // валидация: на каждый вопрос — выбор в пределах его опций
    for (i, item) in q.questions.iter().enumerate() {
        let row = answers.get(i).map(Vec::as_slice).unwrap_or(&[]);
        if row.is_empty() {
            return err("Не на все вопросы выбран ответ");
        }
        let max = item.options.len() as u32;
        if row.iter().any(|&n| n > max) {
            return err("Выбран несуществующий вариант");
        }
    }

    let agent = crate::backend::Agent::from_opt(s.agent.as_deref());
    match tmux::answer_question(&pane, agent, &q, &answers).await {
        Ok(()) => {
            // у хук-вопроса карточку закроет post-tool; у экранного — событий
            // нет, снимаем сами (детектор подтвердит по idle-экрану)
            if q.from_screen {
                d.with_session(&session_id, |s| {
                    s.question = None;
                    s.status = Status::Working;
                    s.updated_at = now_ms();
                });
                d.push();
            }
            windows::toast_remove(&d, &format!("q-{session_id}")); // снять «липкую» карточку
            ok()
        }
        Err(e) => err(ellipsize(&one_line(&e), 100)),
    }
}
```

- [ ] **Step 2: Собрать весь крейт**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: PASS (компилируется; вызов `daemon.rs` ещё на старом контракте, но он
шлёт `{indices,...}`, который мы продолжаем принимать — компиляция не страдает).

- [ ] **Step 3: Прогнать все тесты**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS.

- [ ] **Step 4: Коммит (Task 2 + Task 3)**

```bash
git add src-tauri/src/tmux.rs src-tauri/src/ipc.rs
git commit -m "feat(ipc): по-вопросный контракт ответа answers[][] + ветвление по агенту"
```

---

## Task 4: `daemon::answer_question_hotkey` — новый контракт

**Files:**
- Modify: `src-tauri/src/daemon.rs:570-601`

Хоткей ⌘⌥N — быстрый ответ на одиночный вопрос. Перевести на `{answers:[[n]]}`.
Для мульти-вопроса хоткей отвечает только на первый вопрос — это осознанно
(полный ответ — через визард в приложении).

- [ ] **Step 1: Заменить формирование payload**

В `answer_question_hotkey` заменить блок `serde_json::json!` (строки 594-599):

```rust
            let _ = crate::ipc::question_answer(
                h,
                sid,
                serde_json::json!({ "answers": [[n]] }),
            )
            .await;
```

Переменная `multi` в этой функции больше не нужна для payload (агент/мульти
теперь определяются в `question_answer`). Упростить вычисление `target`:
заменить строки 584-586

```rust
            .map(|sid| (sid.clone(), sessions.get(&sid).and_then(|s| s.question.as_ref())
                .map(|q| q.questions.first().map(|x| x.multi_select).unwrap_or(false)).unwrap_or(false)))
```

на

```rust
            .map(|sid| sid.clone())
```

и заменить распаковку (строки 587, 591):

```rust
        let Some(sid) = target else {
            crate::log::line(&format!("[select] ⌘⌥{n}: нет активного вопроса"));
            return;
        };
        crate::log::line(&format!("[select] ⌘⌥{n} → sid={}", ellipsize(&sid, 8)));
```

- [ ] **Step 2: Собрать и прогнать тесты**

Run: `cargo build --manifest-path src-tauri/Cargo.toml && cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, без предупреждений о неиспользуемой `multi`.

- [ ] **Step 3: Коммит**

```bash
git add src-tauri/src/daemon.rs
git commit -m "feat(daemon): хоткей выбора варианта на новый контракт answers"
```

---

## Task 5: UI-визард в полноэкранном `qview` (renderer.js)

**Files:**
- Modify: `ui/renderer.js:770-886` (состояние и поток вопроса)
- Modify: `ui/index.html` (индикатор прогресса в шапке `qview`)

JS-тестов в репозитории нет — верификация ручная (Task 8).

- [ ] **Step 1: Добавить индикатор прогресса в HTML**

В `ui/index.html`, в блоке `qview` → `qvhead` (рядом с `qHeader`/`qTitle`,
~строки 1347-1352) добавить:

```html
      <span class="qprogress" id="qProgress" hidden></span>
```

- [ ] **Step 2: Расширить состояние и поток вопроса в renderer.js**

Заменить блок состояния (строки 770-772):

```javascript
let qData = null;        // текущий вопрос визарда
let qSel = 0;
let qChosen = new Set();
let qItems = [];         // все вопросы опроса (s.question.questions)
let qIdx = 0;            // индекс текущего вопроса
let qAnswers = [];       // собранные выборы по каждому вопросу: number[][]
```

Заменить `openQuestion` (строки 781-790):

```javascript
function openQuestion(s) {
  qSessionId = s.id;
  qItems = (s.question && s.question.questions) || [];
  qIdx = 0;
  qAnswers = qItems.map(() => []);
  loadQ();
  setView('question');
  qOptsEl.focus?.();
}

// Загрузить текущий вопрос визарда в общее состояние рендера.
function loadQ() {
  qData = qItems[qIdx] || null;
  qSel = 0;
  qChosen = new Set(qAnswers[qIdx] || []);
}
```

Заменить `renderQuestion` (строки 855-861):

```javascript
function renderQuestion() {
  qHeaderEl.textContent = qData.header || '';
  qHeaderEl.hidden = !qData.header;
  qTitleEl.textContent = qData.question;
  const prog = document.getElementById('qProgress');
  if (qItems.length > 1) { prog.textContent = `${qIdx + 1}/${qItems.length}`; prog.hidden = false; }
  else prog.hidden = true;
  activeQOpts = qOptsEl;
  renderQOpts(qOptsEl, qFootEl);
}
```

- [ ] **Step 3: Перевести submit на по-вопросный визард**

Заменить `submitQ` (строки 875-886) на сбор ответа текущего вопроса и переход:

```javascript
// Записать выбор текущего вопроса и пойти дальше (или отправить весь опрос).
function commitCurrentQ() {
  const sel = qData.multiSelect
    ? [...qChosen].sort((a, b) => a - b)
    : [qSel + 1];
  if (!sel.length) { showToast('Отметь хотя бы один вариант'); return false; }
  qAnswers[qIdx] = sel;
  return true;
}

function advanceQ() {
  if (qIdx + 1 < qItems.length) {
    qIdx += 1;
    loadQ();
    if (varOpen) renderVarPanel(curSession()); else renderQuestion();
  } else {
    finalizeQ();
  }
}

async function finalizeQ() {
  const sid = qSessionId;
  const res = await window.jarvis.answerQuestion(sid, { answers: qAnswers });
  if (res.ok) {
    if (varOpen) closeVarPanel();
    else { setView('list'); render(); }
  } else showToast(res.error || 'Не удалось ответить');
}

// Совместимость с существующими обработчиками (Enter / кнопка «Отправить»).
function submitQ() {
  if (commitCurrentQ()) advanceQ();
}
```

`activateQ` (строки 870-873) остаётся как есть: single-select → `submitQ()`
(теперь это «зафиксировать + перейти»), multiSelect → `toggleQ`.

- [ ] **Step 4: Собрать приложение и проверить вручную**

Run: `npm start`
Expected: приложение запускается; экран вопроса открывается без ошибок в
консоли (полная проверка визарда — Task 8).

- [ ] **Step 5: Коммит**

```bash
git add ui/renderer.js ui/index.html
git commit -m "feat(ui): визард по вопросам на полноэкранном экране опроса"
```

---

## Task 6: UI-визард в слайд-овере `varPanel` (renderer.js)

**Files:**
- Modify: `ui/renderer.js:1001-1045`

Слайд-овер вариантов поверх чата должен использовать тот же визард.

- [ ] **Step 1: Инициализировать состояние визарда при открытии панели**

Заменить `openVarPanel` (строки 1032-1045):

```javascript
function openVarPanel() {
  const s = curSession();
  if (!s || !s.question || !s.question.questions || !s.question.questions.length) return;
  qSessionId = s.id;
  qItems = s.question.questions;
  qIdx = 0;
  qAnswers = qItems.map(() => []);
  loadQ();
  varOpen = true;
  qWrap.hidden = false;
  varBtn.classList.add('open');
  replyEl.blur?.(); // освобождаем поле ввода — клавиши уходят пикеру
  renderVarPanel(s);
}
```

- [ ] **Step 2: Показать прогресс и убрать прямую перезапись qData в renderVarPanel**

Заменить `renderVarPanel` (строки 1014-1030):

```javascript
function renderVarPanel(s) {
  const q = qItems[qIdx];
  if (!q) { closeVarPanel(); return; }
  qData = q;
  const prog = qItems.length > 1 ? ` (${qIdx + 1}/${qItems.length})` : '';
  qpHeaderEl.textContent = (q.header || '') + prog;
  qpHeaderEl.hidden = !q.header && !prog;
  qpTitleEl.textContent = q.question;
  activeQOpts = qpOptsEl;
  renderQOpts(qpOptsEl, qpFootEl);
  if (q.multiSelect) { // мульти-выбор: клик-сабмит (на полноэкранном экране это Enter)
    const send = document.createElement('button');
    send.className = 'qp-send';
    send.textContent = qIdx + 1 < qItems.length ? 'Далее' : 'Отправить';
    send.addEventListener('click', submitQ);
    qpFootEl.appendChild(send);
  }
}
```

Примечание: `renderVarBtn` (строки 1004-1012) при `varOpen` дёргает
`renderVarPanel(s)` на каждом `render()`. Чтобы стейт визарда не сбрасывался
живыми пушами демона, `renderVarBtn` НЕ должен трогать `qItems/qIdx/qAnswers` —
он и не трогает (инициализация только в `openVarPanel`). Оставить как есть.

- [ ] **Step 3: Проверить вручную**

Run: `npm start`
Expected: в чате с активным вопросом кнопка вариантов открывает панель;
переключение вопросов работает (полная проверка — Task 8).

- [ ] **Step 4: Коммит**

```bash
git add ui/renderer.js
git commit -m "feat(ui): визард по вопросам в слайд-овере вариантов чата"
```

---

## Task 7: Тост — несколько вопросов и прозрачный мост

**Files:**
- Modify: `src-tauri/src/daemon.rs:418-430` (добавить `count` в payload тоста)
- Modify: `ui/toast-bridge.js:29-30`
- Modify: `ui/toast.js:158-196`

ВАЖНО: payload тоста сейчас **плоский** — `d.question = {multiSelect, question, options}`
только по ПЕРВОМУ вопросу (`daemon.rs:418-430` берёт `q.questions.into_iter().next()`).
Массива `questions[]` в тосте нет. Поэтому сначала добавляем в payload `count`
(сколько всего вопросов), затем тост по `count` решает: чипы или подсказка.

- [ ] **Step 1: Добавить `count` в payload тоста (daemon.rs)**

Заменить блок формирования `qitem`/`question` (`daemon.rs:418-430`):

```rust
        let qfull = session_id
            .and_then(|sid| self.session(sid))
            .and_then(|s| s.question);
        let qcount = qfull.as_ref().map(|q| q.questions.len()).unwrap_or(0);
        let qitem = qfull.and_then(|q| q.questions.into_iter().next());
        let question = qitem.as_ref().map(|qi| {
            serde_json::json!({
                "multiSelect": qi.multi_select,
                "question": qi.question,
                "count": qcount,
                "options": qi.options.iter()
                    .map(|o| serde_json::json!({ "label": o.label, "description": o.description }))
                    .collect::<Vec<_>>(),
            })
        });
```

Собрать: `cargo build --manifest-path src-tauri/Cargo.toml` → PASS.

- [ ] **Step 2: Прозрачная передача choice в toast-bridge**

Заменить `ui/toast-bridge.js:29-30`:

```javascript
    answerQuestion: (sessionId, choice) =>
      invoke('question_answer', { sessionId, choice }),
```

- [ ] **Step 3: В toast.js — чипы только для одиночного вопроса**

В `ui/toast.js` заменить блок вариантов (строки 158-196). Используем плоский
payload (`d.question.options`, `d.question.multiSelect`, `d.question.count`).
Инлайн-чипы — только если в опросе ровно один вопрос (`count <= 1`); иначе —
подсказка отвечать в приложении.

```javascript
    // варианты вопроса (AskUserQuestion). Payload плоский: первый вопрос +
    // count. Инлайн-чипы — только для одиночного вопроса; мульти-вопрос
    // отвечается в приложении (визард).
    const qq = d.question || null;
    const count = qq && typeof qq.count === 'number' ? qq.count : (qq && qq.options ? 1 : 0);
    const opts = qq && Array.isArray(qq.options) ? qq.options : null;
    if (count > 1) {
      sticky = true;
      card.classList.add('sticky');
      const note = document.createElement('div');
      note.className = 'body';
      note.textContent = `Несколько вопросов (${count}) — ответь в приложении`;
      card.appendChild(note);
    } else if (opts && opts.length) {
      sticky = true; // ждём выбор — карточка не тикает по TTL
      card.classList.add('sticky');
      const list = document.createElement('div');
      list.className = 'opts';
      opts.slice(0, 9).forEach((o, i) => {
        const opt = document.createElement('div');
        opt.className = 'opt';
        const num = document.createElement('span');
        num.className = 'num';
        const key = document.createElement('span');
        key.className = 'key';
        key.textContent = '⌘⌥';
        num.append(key, document.createTextNode(String(i + 1)));
        const otext = document.createElement('div');
        otext.className = 'otext';
        const ol = document.createElement('div');
        ol.className = 'olabel';
        ol.textContent = o.label || '';
        otext.appendChild(ol);
        if (o.description) {
          const od = document.createElement('div');
          od.className = 'odesc';
          od.textContent = o.description;
          otext.appendChild(od);
        }
        opt.append(num, otext);
        opt.addEventListener('click', (e) => {
          e.stopPropagation();
          window.toast.answerQuestion(d.sessionId, { answers: [[i + 1]] });
          if (!qq.multiSelect) removeCard(d.id);
        });
        list.appendChild(opt);
      });
      card.appendChild(list);
    }
```

Обновить проверку «застрявшей сессии» ниже (строка 201): заменить
`!(opts && opts.length)` на `!(count > 0)`, чтобы «Продолжить» не появлялось
ни для одиночного, ни для мульти-вопроса:

```javascript
    if (d.sessionId && d.kind !== 'done' && !(count > 0)) {
```

- [ ] **Step 4: Проверить вручную**

Run: `npm start`
Expected: тост одиночного вопроса показывает чипы и отвечает; тост мульти-вопроса
показывает подсказку (полная проверка — Task 8).

- [ ] **Step 5: Коммит**

```bash
git add src-tauri/src/daemon.rs ui/toast.js ui/toast-bridge.js
git commit -m "feat(toast): count в payload, одиночные чипы / подсказка для мульти-вопроса"
```

---

## Task 8: Живая верификация раскладки клавиш (с пользователем)

**Files:**
- Modify (при расхождении): `src-tauri/src/tmux.rs` (константы `CLAUDE_ADVANCE`,
  `CLAUDE_SUBMIT`, `CLAUDE_SUBMIT_RIGHT`, Codex Down/Space/Enter) + соответствующие
  ожидания в `answer_keys_tests`.

Ручной прогон против живых пикеров. Цель — подтвердить или исправить клавиши.

- [ ] **Step 1: Подготовить сценарии**

Попросить пользователя инициировать в живой сессии:
1. Claude `AskUserQuestion` с ОДНИМ single-select вопросом.
2. Claude `AskUserQuestion` с ОДНИМ multiSelect вопросом.
3. Claude `AskUserQuestion` с НЕСКОЛЬКИМИ вопросами (single и multi вперемешку).
4. Codex single-select пикер.
5. Codex multiSelect пикер (если бывает).

- [ ] **Step 2: Прогнать каждый сценарий через UI Jarvis**

Run: `npm start`, отвечать через визард; параллельно наблюдать пану:
`tmux capture-pane -p -t <pane>` до/после.
Expected: выбор применяется ровно как в UI, экран агента не засоряется, опрос
закрывается.

- [ ] **Step 3: При расхождении — поправить константы и тесты вместе**

Если навигация «уплывает» (особенно переход между вопросами Claude):
- скорректировать `CLAUDE_ADVANCE` / `CLAUDE_SUBMIT` / Codex-клавиши в `tmux.rs`;
- синхронно обновить ожидания в `answer_keys_tests`;
- при стойком дрейфе перехода между вопросами — добавить точечный read-back
  (`tmux capture-pane`) ТОЛЬКО на шаг перехода (запасной план из спека),
  локализовав его в `answer_question`, не трогая чистую `answer_keys`.

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS.

- [ ] **Step 4: Коммит (если были правки)**

```bash
git add -A
git commit -m "fix(tmux): выверенная вживую раскладка клавиш пикеров"
```

---

## Self-Review

**Покрытие спека:**
- Контракт `{answers}` + совместимость — Task 3. ✅
- `answer_question` ветвление по агенту/позиции — Task 1 (логика) + Task 2 (исполнитель). ✅
- Codex стрелки/Space/Enter вместо цифр — Task 1 (Codex-ветка). ✅
- UI-визард, индикатор «2/4», авто-переход single-select — Task 5, 6. ✅
- Тост: одиночные чипы / подсказка для мульти — Task 7. ✅
- Гард скрин-скрейпера, «липкая» карточка, пустой выбор — сохранены в Task 3/7. ✅
- Юнит-тесты раскладки + ручной/живой прогон — Task 1, 8. ✅

**Скан плейсхолдеров:** помётки «УТОЧНИТЬ ВЖИВУЮ» — сознательная часть дизайна
(значения констант), а не пропуск; Task 8 их закрывает. Прочих TBD нет.

**Согласованность имён:** `answer_keys`, `answer_question`, `question_answer`,
`commitCurrentQ`/`advanceQ`/`finalizeQ`/`submitQ`, `qItems`/`qIdx`/`qAnswers`,
константы `CLAUDE_ADVANCE`/`CLAUDE_SUBMIT`/`CLAUDE_SUBMIT_RIGHT` — единообразны
по задачам.
