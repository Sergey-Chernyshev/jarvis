//! Авто-цепочка: Джарвис сам замечает, что запущенная сессия закончила ход,
//! забирает итог и заводит следующий заход — вместо человека, который смотрел
//! глазами и дописывал руками.
//!
//! ПОДПИСКА, а не ожидание. Блокирующая `sessions.wait` тут уже была и откачена:
//! пока агент ждёт, он не агент. Цепочку двигает СОБЫТИЕ — ветка `stop` в
//! редьюсере демона (единственное место, где ход сессии объявляется законченным)
//! зовёт `on_session_done`, и вся работа идёт уже вне чужого хода.
//!
//! Состояние цепочки — процессное (как `turns_in_flight`), а РЕЖИМ — свойство
//! чата и живёт в `ChatBook`: у владельца несколько чатов по проектам, и «сам
//! себе продолжай» уместен не в каждом.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::daemon::Daemon;
use crate::turns::{FileTouch, TurnFacts};
use crate::util::{ellipsize, now_ms, one_line};

/// Потолок глубины авто-цепочки. Типовой цикл «доведи до зелёного» — 3–5
/// заходов; десять дают двойной запас и при ходе в 5–10 минут это примерно час
/// работы без взгляда человека — столько отдать не глядя ещё можно, больше уже
/// страшно. Дальше цепочка встаёт САМА и говорит об этом словами.
pub const MAX_STEPS: u32 = 10;

/// Сколько неудач подряд ПО ОДНОЙ причине рвут цепочку. Две: первая бывает
/// случайной (моргнул tmux, перегрелся API), вторая та же — уже система, и
/// третья попытка ничего не добавит, кроме сожжённого хода.
pub const MAX_SAME_FAILS: u32 = 2;

/// Режим чата: кто пишет следующий заход.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// «Спросить меня»: хост предлагает текст и ждёт кнопки. Дефолт — молчаливый
    /// авто-режим никто не включал.
    #[default]
    Ask,
    /// «Продолжать самому»: хост формулирует заход и отправляет его сам.
    Auto,
}

/// Что цепочка делает прямо сейчас — для шапки чата.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Заход в работе: ждём, когда сессия закончит.
    Watching,
    /// Текст следующего захода предложен, ждём кнопки.
    Proposed,
    /// Прямо сейчас отправляем заход в сессию.
    Sending,
    /// Цепочки нет (оборвана или не заводилась).
    Stopped,
}

/// Одна живая цепочка. Ключ — id ЧАТА: цепочка принадлежит разговору, а не
/// сессии, иначе два чата про один проект дрались бы за один «закончил».
#[derive(Debug, Clone)]
struct Chain {
    session_id: String,
    mode: Mode,
    /// Номер захода: растёт на каждой отправке (0 — ещё ни одного).
    step: u32,
    phase: Phase,
    /// Ключ идемпотентности — последний отработанный «закончил».
    last_done: Option<String>,
    /// Причина последней неудачи и сколько раз подряд она повторилась.
    fail_code: String,
    fail_count: u32,
    /// Предложенный текст (режим «спросить меня»).
    proposal: Option<String>,
    note: String,
}

/// Срез цепочки для шапки чата.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainState {
    pub chat_id: String,
    pub active: bool,
    pub mode: Mode,
    pub session_id: Option<String>,
    pub step: u32,
    pub max_steps: u32,
    pub phase: Phase,
    pub note: String,
    pub proposal: Option<String>,
}

impl ChainState {
    /// Чат без цепочки: режим виден всегда (шапке есть что нарисовать), остальное пусто.
    fn idle(chat_id: &str, mode: Mode) -> Self {
        ChainState {
            chat_id: chat_id.to_string(),
            active: false,
            mode,
            session_id: None,
            step: 0,
            max_steps: MAX_STEPS,
            phase: Phase::Stopped,
            note: String::new(),
            proposal: None,
        }
    }
}

/// Решение по событию «сессия закончила ход».
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Тот же «закончил» уже отработан — молчим (главная защита от разгона).
    Skip,
    /// Собрать итог, положить карточку и ждать кнопки.
    Propose,
    /// Собрать итог, сформулировать и отправить заход с этим номером.
    Send(u32),
    /// Карточку положить, цепочку оборвать: упёрлись в потолок глубины.
    Depth,
}

/// Что делать после неудачи.
#[derive(Debug, Clone, PartialEq)]
pub enum FailAction {
    /// Сказать словами, цепочка жива.
    Note,
    /// Оборвать цепочку: то же самое второй раз подряд.
    Stop,
}

/// Реестр цепочек. Отдельный тип (а не поле Daemon) — чтобы тесты гоняли его
/// без живого демона и окна.
#[derive(Default)]
pub struct Chains {
    map: Mutex<HashMap<String, Chain>>,
}

impl Chains {
    pub fn new() -> Self {
        Chains::default()
    }

    /// Завести (или перенаправить) цепочку чата на сессию.
    pub fn watch(&self, chat_id: &str, session_id: &str, mode: Mode) -> ChainState {
        let mut m = self.map.lock().unwrap();
        let c = m.entry(chat_id.to_string()).or_insert_with(|| Chain {
            session_id: session_id.to_string(),
            mode,
            step: 0,
            phase: Phase::Watching,
            last_done: None,
            fail_code: String::new(),
            fail_count: 0,
            proposal: None,
            note: String::new(),
        });
        // Сессия сменилась — это новая цепочка: и счётчик заходов, и серия
        // неудач, и ключ идемпотентности принадлежали прошлой.
        if c.session_id != session_id {
            c.session_id = session_id.to_string();
            c.step = 0;
            c.last_done = None;
            c.fail_code.clear();
            c.fail_count = 0;
            c.proposal = None;
        }
        c.mode = mode;
        c.phase = Phase::Watching;
        c.note = "сессия работает".into();
        state_of(chat_id, Some(c))
    }

    pub fn state(&self, chat_id: &str, mode: Mode) -> ChainState {
        let m = self.map.lock().unwrap();
        match m.get(chat_id) {
            Some(c) => state_of(chat_id, Some(c)),
            None => ChainState::idle(chat_id, mode),
        }
    }

    /// Сменить режим на лету: цепочка идёт, человек передумал.
    pub fn set_mode(&self, chat_id: &str, mode: Mode) {
        if let Some(c) = self.map.lock().unwrap().get_mut(chat_id) {
            c.mode = mode;
        }
    }

    /// Стоп рвёт ЦЕПОЧКУ, а не текущий ход: запись исчезает, и следующее
    /// «закончил» этой сессии уже никого не разбудит.
    pub fn stop(&self, chat_id: &str) -> bool {
        self.map.lock().unwrap().remove(chat_id).is_some()
    }

    /// Чаты, чьи цепочки смотрят на эту сессию.
    pub fn chats_of(&self, session_id: &str) -> Vec<String> {
        let m = self.map.lock().unwrap();
        let mut out: Vec<String> = m
            .iter()
            .filter(|(_, c)| c.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        out.sort();
        out
    }

    /// «Сессия закончила ход» → что делать каждой цепочке на ней.
    ///
    /// Ключ идемпотентности — `<sid>#<момент стопа>`: `done_at` демон ставит
    /// ровно в ветке `stop` и больше нигде, поэтому одинаковая пара значит
    /// буквально то же самое завершение, а не следующее. Ключ проставляется
    /// СИНХРОННО под локом, до всякой асинхронной работы, — иначе два
    /// одновременных стопа успели бы пройти проверку оба.
    pub fn on_done(&self, session_id: &str, at: i64) -> Vec<(String, Decision)> {
        let key = format!("{session_id}#{at}");
        let mut m = self.map.lock().unwrap();
        let mut out: Vec<(String, Decision)> = Vec::new();
        for (chat_id, c) in m.iter_mut() {
            if c.session_id != session_id {
                continue;
            }
            if c.last_done.as_deref() == Some(key.as_str()) {
                out.push((chat_id.clone(), Decision::Skip));
                continue;
            }
            c.last_done = Some(key.clone());
            c.proposal = None;
            let d = match c.mode {
                Mode::Ask => {
                    c.phase = Phase::Proposed;
                    c.note = "жду решения".into();
                    Decision::Propose
                }
                Mode::Auto if c.step >= MAX_STEPS => {
                    c.phase = Phase::Stopped;
                    Decision::Depth
                }
                Mode::Auto => {
                    c.step += 1;
                    c.phase = Phase::Sending;
                    c.note = format!("готовлю заход {}", c.step);
                    Decision::Send(c.step)
                }
            };
            out.push((chat_id.clone(), d));
        }
        // Потолок обрывает цепочку — карточку положим, но следующего «закончил»
        // эта запись уже не увидит.
        for (chat_id, d) in &out {
            if *d == Decision::Depth {
                m.remove(chat_id);
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Заход ушёл в сессию: цепочка снова ждёт события, серия неудач сброшена.
    pub fn mark_sent(&self, chat_id: &str, step: u32) {
        if let Some(c) = self.map.lock().unwrap().get_mut(chat_id) {
            c.step = c.step.max(step);
            c.phase = Phase::Watching;
            c.note = format!("заход {} в работе", c.step);
            c.proposal = None;
            c.fail_code.clear();
            c.fail_count = 0;
        }
    }

    /// Предложить текст и ждать кнопки.
    pub fn mark_proposed(&self, chat_id: &str, prompt: &str) {
        if let Some(c) = self.map.lock().unwrap().get_mut(chat_id) {
            c.phase = Phase::Proposed;
            c.note = "жду решения".into();
            c.proposal = Some(prompt.to_string());
        }
    }

    /// Текст, предложенный человеку (кнопка «отправить» шлёт именно его).
    pub fn proposal(&self, chat_id: &str) -> Option<String> {
        self.map
            .lock()
            .unwrap()
            .get(chat_id)
            .and_then(|c| c.proposal.clone())
    }

    /// Номер следующего захода при ручной отправке.
    pub fn next_step(&self, chat_id: &str) -> u32 {
        self.map
            .lock()
            .unwrap()
            .get(chat_id)
            .map_or(1, |c| c.step + 1)
    }

    /// Неудача по цепочке. Две подряд по ОДНОЙ причине — стоп: запись
    /// удаляется, дальше нужна рука человека.
    pub fn on_fail(&self, chat_id: &str, code: &str) -> FailAction {
        let mut m = self.map.lock().unwrap();
        let Some(c) = m.get_mut(chat_id) else {
            return FailAction::Note;
        };
        if c.fail_code == code {
            c.fail_count += 1;
        } else {
            c.fail_code = code.to_string();
            c.fail_count = 1;
        }
        if c.fail_count >= MAX_SAME_FAILS {
            m.remove(chat_id);
            return FailAction::Stop;
        }
        c.phase = Phase::Watching;
        c.note = "заход не удался".into();
        FailAction::Note
    }
}

fn state_of(chat_id: &str, c: Option<&Chain>) -> ChainState {
    match c {
        Some(c) => ChainState {
            chat_id: chat_id.to_string(),
            active: true,
            mode: c.mode,
            session_id: Some(c.session_id.clone()),
            step: c.step,
            max_steps: MAX_STEPS,
            phase: c.phase,
            note: c.note.clone(),
            proposal: c.proposal.clone(),
        },
        None => ChainState::idle(chat_id, Mode::Ask),
    }
}

/// Процессный реестр цепочек — один на приложение (как `turns_in_flight`).
pub fn chains() -> &'static Chains {
    static C: std::sync::OnceLock<Chains> = std::sync::OnceLock::new();
    C.get_or_init(Chains::new)
}

// ── Итог хода ─────────────────────────────────────────────────────────────

/// Вердикт по тестам последнего хода.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tests {
    pub ok: bool,
    /// Строка, по которой судили, — человек должен видеть основание.
    pub line: String,
}

/// Итог хода сессии: то, что человек читал глазами в терминале.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub session_id: String,
    pub project: String,
    /// Сводка хода (карточка `turns.rs`) либо строка списка сессии.
    pub summary: String,
    /// Последний ответ агента.
    pub reply: String,
    pub files: Vec<FileTouch>,
    pub commands: Vec<String>,
    pub tests: Option<Tests>,
    pub turn_key: String,
}

impl Outcome {
    /// Есть ли вообще о чём говорить. Пустой итог — не «всё хорошо», а «не
    /// собралось»: цепочке на таком строить следующий заход не из чего.
    pub fn is_empty(&self) -> bool {
        self.summary.trim().is_empty()
            && self.reply.trim().is_empty()
            && self.files.is_empty()
            && self.commands.is_empty()
    }
}

/// Число прямо перед словом (последнее вхождение): «1028 passed» → 1028.
/// Свой разбор вместо регулярки — дешевле и не тащит зависимость в горячий путь.
fn count_before(s: &str, word: &str) -> Option<u32> {
    let mut out = None;
    for (i, _) in s.match_indices(word) {
        let head = s[..i].trim_end();
        let digits: String = head
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<char>>()
            .into_iter()
            .rev()
            .collect();
        if let Ok(n) = digits.parse::<u32>() {
            out = Some(n);
        }
    }
    out
}

/// Зелёные тесты или красные — детерминированно, из фактов хода.
///
/// Судим по СЧЁТЧИКАМ, а не по наличию слова «failed»: «0 failed» и «10 failed»
/// отличаются одной цифрой, и подстрочный поиск их путает. Красное перевешивает
/// зелёное: в одном ходе бывает и то и другое, а важна плохая новость.
pub fn tests_verdict(facts: &TurnFacts) -> Option<Tests> {
    let mut lines: Vec<String> = Vec::new();
    lines.extend(facts.commands.iter().cloned());
    lines.extend(facts.tool_log.iter().cloned());
    lines.extend(facts.final_reply.lines().map(str::to_string));

    let mut green: Option<String> = None;
    for raw in lines {
        let l = one_line(&raw);
        let s = l.to_lowercase();
        if !(s.contains("test") || s.contains("тест") || s.contains("passed") || s.contains("failed"))
        {
            continue;
        }
        let failed = count_before(&s, " failed").or_else(|| count_before(&s, " failures"));
        let passed = count_before(&s, " passed").or_else(|| count_before(&s, " ok"));
        let red = failed.is_some_and(|n| n > 0)
            || s.contains("test result: failed")
            || s.contains("тесты красные");
        if red {
            return Some(Tests { ok: false, line: ellipsize(&l, 200) });
        }
        let is_green = s.contains("test result: ok")
            || s.contains("тесты зелёные")
            || passed.is_some_and(|n| n > 0)
            || failed == Some(0);
        if is_green && green.is_none() {
            green = Some(ellipsize(&l, 200));
        }
    }
    green.map(|line| Tests { ok: true, line })
}

/// Итог из уже собранных кусков. Отдельно от сбора, чтобы тестировать без диска.
pub fn build_outcome(
    session_id: &str,
    project: &str,
    turn_key: &str,
    summary: &str,
    facts: Option<&TurnFacts>,
) -> Outcome {
    let facts = facts.cloned().unwrap_or_default();
    Outcome {
        session_id: session_id.to_string(),
        project: project.to_string(),
        summary: ellipsize(&one_line(summary), 600),
        reply: ellipsize(&facts.final_reply, 2000),
        tests: tests_verdict(&facts),
        files: facts.files.iter().take(20).cloned().collect(),
        commands: facts.commands.iter().take(10).cloned().collect(),
        turn_key: turn_key.to_string(),
    }
}

/// Следующий заход из итога — детерминированный шаблон.
///
/// Он же фолбэк, когда служебного LLM нет: заход обязан быть всегда, иначе
/// цепочка встанет молча — ровно та беда, ради которой всё это и делалось.
pub fn next_prompt(o: &Outcome, step: u32) -> String {
    let mut p = format!("Заход {step} из {MAX_STEPS} (авто-цепочка Джарвиса).\n");
    if !o.summary.is_empty() {
        p.push_str(&format!("Итог прошлого хода: {}\n", o.summary));
    }
    if !o.files.is_empty() {
        let list: Vec<&str> = o.files.iter().take(8).map(|f| f.path.as_str()).collect();
        p.push_str(&format!("Тронуто: {}\n", list.join(", ")));
    }
    match &o.tests {
        Some(t) if !t.ok => p.push_str(&format!(
            "Тесты КРАСНЫЕ: {}\nПочини причину и прогони проверку снова.\n",
            t.line
        )),
        Some(t) => p.push_str(&format!("Тесты зелёные: {}\n", t.line)),
        None => p.push_str("Проверку в прошлом ходе не прогоняли.\n"),
    }
    p.push_str(
        "Продолжай сам: возьми следующий незакрытый кусок этой же работы, доведи до конца \
         и прогони проверку. Если работа закончена — ответь одной строкой «готово» и не \
         начинай новую.",
    );
    p
}

// ── Связка с демоном и окном ──────────────────────────────────────────────

/// Режим чата из настроек. Источник истины — `ChatBook`: у владельца несколько
/// чатов, и глобальный тумблер включал бы авто-режим там, где о нём не просили.
pub fn mode_of(app: &AppHandle, chat_id: &str) -> Mode {
    super::chat_book(app).mode_of(chat_id)
}

/// Срез цепочки для шапки (режим — из настроек, остальное — из реестра).
pub fn state(app: &AppHandle, chat_id: &str) -> ChainState {
    chains().state(chat_id, mode_of(app, chat_id))
}

/// Событие цепочки наружу. Канал свой (`agent:chain`), но правило то же, что у
/// `agent:event`: метка чата обязательна — без неё карточка легла бы в чужой
/// разговор, стоит человеку уйти в соседний проект.
fn emit(app: &AppHandle, chat_id: &str, kind: &str, extra: Value) {
    let mut payload = json!({
        "chatId": chat_id,
        "kind": kind,
        "at": now_ms(),
        "state": state(app, chat_id),
    });
    if let (Some(obj), Some(add)) = (payload.as_object_mut(), extra.as_object()) {
        for (k, v) in add {
            obj.insert(k.clone(), v.clone());
        }
    }
    if let Err(e) = app.emit("agent:chain", payload) {
        crate::log::line(&format!("[chain] emit error: {e}"));
    }
}

/// Разослать шапке свежий срез (режим переключили, цепочку оборвали): окон с
/// чатом может быть несколько, и ответ одной команды видит только одно из них.
pub fn push_state(app: &AppHandle, chat_id: &str) {
    emit(app, chat_id, "state", json!({}));
}

/// Текст отказа. Причину НЕ переписываем своими словами: что сказал гейт, tmux
/// или хост — то человек и должен прочитать; от нас только вывод про цепочку.
pub fn fail_text(text: &str, stopped: bool) -> String {
    let text = one_line(text);
    if stopped {
        format!("{text}. Это вторая неудача подряд по одной причине — цепочку останавливаю")
    } else {
        text
    }
}

/// Отказ наружу словами — единая точка, чтобы «тихих» веток не заводилось.
fn refuse(app: &AppHandle, chat_id: &str, code: &str, text: &str) {
    crate::log::line(&format!("[chain] {chat_id}: {code} — {text}"));
    let stopped = chains().on_fail(chat_id, code) == FailAction::Stop;
    let text = fail_text(text, stopped);
    emit(
        app,
        chat_id,
        if stopped { "stopped" } else { "failed" },
        json!({ "reason": code, "text": text }),
    );
}

/// Наблюдение за потоком главного агента — ЕДИНСТВЕННОГО эмита `agent:event`,
/// через который проходят оба хоста (claude и codex). Отсюда цепочка узнаёт две
/// вещи: на какую сессию смотреть и что хост упал.
pub(crate) fn observe(app: &AppHandle, chat_id: &str, ev: &super::AgentEvent) {
    match ev {
        // Джарвис отправил промпт в сессию — значит следить надо за ней.
        // Привязка отсюда, а не из гейта: гейт не знает, из какого чата пришёл
        // вызов, а поток знает — ход помечен чатом при отправке.
        super::AgentEvent::ToolUse { name, input } => {
            if !(name.ends_with("sessions_reply") || name.ends_with("sessions.reply")) {
                return;
            }
            let Some(sid) = input
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return;
            };
            let mode = mode_of(app, chat_id);
            chains().watch(chat_id, sid, mode);
            emit(app, chat_id, "watching", json!({ "sessionId": sid }));
        }
        // «claude не найден», обрыв без ответа, убит за изоляцию — всё приходит
        // сюда одним Failed. Цепочке об этом надо знать: она ждёт хода, которого
        // уже не будет.
        super::AgentEvent::Failed { message, .. }
            if chains().state(chat_id, Mode::Ask).active =>
        {
            refuse(app, chat_id, "host", message);
        }
        _ => {}
    }
}

/// Точка подписки: ход сессии закончен (ветка `stop` редьюсера).
pub fn on_session_done(d: &Arc<Daemon>, session_id: &str, at: i64) {
    let plan = chains().on_done(session_id, at);
    if plan.is_empty() {
        return;
    }
    for (chat_id, decision) in plan {
        if decision == Decision::Skip {
            continue; // тот же «закончил» уже отработан
        }
        let d = d.clone();
        let sid = session_id.to_string();
        tauri::async_runtime::spawn(async move {
            run_step(&d, &chat_id, &sid, decision).await;
        });
    }
}

async fn run_step(d: &Arc<Daemon>, chat_id: &str, sid: &str, decision: Decision) {
    let app = d.app.clone();
    let outcome = collect_outcome(d, sid).await;
    emit(
        &app,
        chat_id,
        "done",
        json!({ "sessionId": sid, "outcome": outcome }),
    );

    if let Decision::Depth = decision {
        emit(
            &app,
            chat_id,
            "stopped",
            json!({
                "reason": "depth",
                "text": format!(
                    "Потолок авто-цепочки — {MAX_STEPS} заходов подряд. Дальше нужен твой взгляд"
                ),
            }),
        );
        return;
    }

    if outcome.is_empty() {
        refuse(
            &app,
            chat_id,
            "no-outcome",
            "Итог хода не собрался — транскрипт сессии не прочитан",
        );
        return;
    }

    match decision {
        Decision::Propose => {
            let prompt = formulate(&outcome, chains().next_step(chat_id)).await;
            chains().mark_proposed(chat_id, &prompt);
            emit(
                &app,
                chat_id,
                "proposed",
                json!({ "sessionId": sid, "prompt": prompt }),
            );
        }
        Decision::Send(step) => {
            let prompt = formulate(&outcome, step).await;
            let _ = deliver(d, chat_id, sid, &prompt, step).await; // отказ уже сказан словами
        }
        Decision::Skip | Decision::Depth => {}
    }
}

/// Отправка захода в сессию через ГЕЙТ, потребителем «панель»: заход — это
/// действие человека, который включил режим (или нажал кнопку), а не самоволка
/// агента. Подтверждение на каждый заход убило бы саму идею, аудит и отказы
/// гейта остаются на месте и уходят словами.
pub(crate) async fn deliver(
    d: &Arc<Daemon>,
    chat_id: &str,
    sid: &str,
    prompt: &str,
    step: u32,
) -> Result<(), String> {
    let app = d.app.clone();
    let out = crate::ipc::via_gate_panel(
        d,
        "sessions.reply",
        json!({ "session_id": sid, "text": prompt }),
    )
    .await;
    if out.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        chains().mark_sent(chat_id, step);
        emit(
            &app,
            chat_id,
            "sent",
            json!({ "sessionId": sid, "prompt": prompt, "step": step }),
        );
        return Ok(());
    }
    let text = format!(
        "Заход не ушёл: {}",
        out.get("error")
            .and_then(Value::as_str)
            .unwrap_or("сессия не приняла заход")
    );
    refuse(&app, chat_id, "send", &text);
    Err(text)
}

/// Формулировка следующего захода. Служебный LLM — украшение: если его нет или
/// он ответил мусором, идёт детерминированный шаблон. Пустого захода не бывает.
async fn formulate(o: &Outcome, step: u32) -> String {
    let base = next_prompt(o, step);
    if !crate::claude_bin::any_service_bin() {
        return base;
    }
    let ask = format!(
        "Ты ведёшь цепочку работ. Ниже — итог последнего хода агента в репозитории.\n\
         Напиши ОДИН следующий заход для этого агента: что доделать дальше, коротко и по делу.\n\
         Только текст промпта, без преамбул, без кавычек, по-русски, не длиннее 6 строк.\n\
         Если работа выглядит законченной — попроси прогнать проверку и ответить «готово».\n\n\
         ИТОГ:\n{base}"
    );
    match crate::claude_bin::run_service_llm(&ask, Duration::from_secs(45)).await {
        Some(t) if t.trim().len() > 20 && crate::ru::has_cyrillic(&t) => {
            format!("Заход {step} из {MAX_STEPS} (авто-цепочка Джарвиса).\n{}", t.trim())
        }
        _ => base,
    }
}

/// Итог хода: сводка + последний ответ + тронутые файлы + тесты. Второго
/// сборщика не заводим — берём готовые `turns.rs` (факты) и `turnsum` (карточка).
pub(crate) async fn collect_outcome(d: &Arc<Daemon>, sid: &str) -> Outcome {
    let s = d.session(sid);
    let project = s
        .as_ref()
        .and_then(|s| s.project.clone())
        .unwrap_or_else(|| "?".into());
    let list_line = s
        .as_ref()
        .and_then(|s| s.summary.clone())
        .filter(|t| !t.is_empty())
        .or_else(|| s.as_ref().map(|s| s.detail.clone()))
        .unwrap_or_default();

    let Some((be, entries)) = d.turn_entries(sid).await else {
        return build_outcome(sid, &project, "", &list_line, None);
    };
    let (_items, turns) = crate::turns::segment(be, &entries);
    let Some(t) = turns.iter().rev().find(|t| t.span.complete) else {
        return build_outcome(sid, &project, "", &list_line, None);
    };
    // Карточка хода уже могла быть сгенерирована на этот же Stop — берём её,
    // иначе просим ту же самую генерацию (кэш и эмит внутри неё).
    let cached = crate::turnsum::load_cards(sid).get(&t.span.key).cloned();
    let card = match cached {
        Some(c) => Some(c),
        None => d.turn_generate(sid, t).await,
    };
    let summary = card
        .map(|c| c.summary)
        .filter(|t| !t.is_empty())
        .unwrap_or(list_line);
    build_outcome(sid, &project, &t.span.key, &summary, Some(&t.facts))
}

/// Сессия исчезла (session-end, убитый терминал, снятая за изоляцию пана):
/// цепочке ждать больше нечего, и молчать об этом нельзя.
pub fn on_session_gone(d: &Arc<Daemon>, session_id: &str, why: &str) {
    for chat_id in chains().chats_of(session_id) {
        chains().stop(&chat_id);
        emit(
            &d.app,
            &chat_id,
            "stopped",
            json!({
                "reason": "gone",
                "sessionId": session_id,
                "text": format!("Сессия {} — {why}; цепочка оборвана", ellipsize(session_id, 8)),
            }),
        );
    }
}

/// Ход сессии сорвался ошибкой (stop-failure): лимит, перегрузка, биллинг.
/// Причина входит в код неудачи — два лимита подряд остановят цепочку.
pub fn on_session_failed(d: &Arc<Daemon>, session_id: &str, payload: &Value) {
    let kind = crate::limits::classify_failure(payload);
    let text = match kind {
        "rate_limit" => "Сессия упёрлась в лимит — ход не состоялся",
        "billing" => "Сессия встала на ошибке биллинга",
        "overloaded" => "API перегружен — ход сорвался",
        _ => "Ход сессии прервался ошибкой",
    };
    for chat_id in chains().chats_of(session_id) {
        refuse(&d.app, &chat_id, &format!("stop-failure:{kind}"), text);
    }
}

// ── Тесты ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(mode: Mode) -> Chains {
        let c = Chains::new();
        c.watch("c1", "s1", mode);
        c
    }

    /// Главная защита от разгона: один и тот же «закончил» не пинает дважды.
    #[test]
    fn the_same_finish_never_fires_twice() {
        let c = chain(Mode::Auto);
        assert_eq!(c.on_done("s1", 100), vec![("c1".into(), Decision::Send(1))]);
        c.mark_sent("c1", 1);
        // тот же момент стопа — повтор хука, ретрай редьюсера, что угодно
        assert_eq!(c.on_done("s1", 100), vec![("c1".into(), Decision::Skip)]);
        assert_eq!(c.on_done("s1", 100), vec![("c1".into(), Decision::Skip)]);
        // следующее завершение — уже другое событие
        assert_eq!(c.on_done("s1", 101), vec![("c1".into(), Decision::Send(2))]);
        // чужая сессия цепочку не трогает
        assert!(c.on_done("s2", 102).is_empty());
    }

    #[test]
    fn depth_ceiling_stops_the_chain() {
        let c = chain(Mode::Auto);
        for i in 1..=MAX_STEPS {
            let at = 100 + i as i64;
            assert_eq!(c.on_done("s1", at), vec![("c1".into(), Decision::Send(i))]);
            c.mark_sent("c1", i);
        }
        // потолок: карточку положим, цепочку рвём
        assert_eq!(
            c.on_done("s1", 999),
            vec![("c1".into(), Decision::Depth)],
            "на {MAX_STEPS}+1 заходе цепочка обязана встать"
        );
        assert!(!c.state("c1", Mode::Auto).active, "после потолка цепочки нет");
        assert!(c.on_done("s1", 1000).is_empty(), "оборванная не просыпается");
    }

    #[test]
    fn stop_breaks_the_chain_not_just_the_turn() {
        let c = chain(Mode::Auto);
        assert_eq!(c.on_done("s1", 100), vec![("c1".into(), Decision::Send(1))]);
        assert!(c.stop("c1"));
        // завершение сессии больше никого не будит
        assert!(c.on_done("s1", 200).is_empty(), "стоп рвёт цепочку, а не ход");
        assert!(c.on_done("s1", 300).is_empty());
        assert!(!c.state("c1", Mode::Auto).active);
        assert!(!c.stop("c1"), "повторный стоп — не ошибка, но и не находка");
        // человек включил режим снова — цепочка заводится с нуля
        c.watch("c1", "s1", Mode::Auto);
        assert_eq!(c.on_done("s1", 400), vec![("c1".into(), Decision::Send(1))]);
    }

    #[test]
    fn two_failures_of_one_kind_in_a_row_stop_the_chain() {
        let c = chain(Mode::Auto);
        assert_eq!(c.on_fail("c1", "send"), FailAction::Note);
        assert_eq!(c.on_fail("c1", "send"), FailAction::Stop);
        assert!(!c.state("c1", Mode::Auto).active, "серия по одной причине — стоп");

        // разные причины подряд серией не считаются
        let c = chain(Mode::Auto);
        assert_eq!(c.on_fail("c1", "send"), FailAction::Note);
        assert_eq!(c.on_fail("c1", "stop-failure:rate_limit"), FailAction::Note);
        assert_eq!(c.on_fail("c1", "send"), FailAction::Note);
        assert!(c.state("c1", Mode::Auto).active);

        // удачный заход обнуляет серию
        let c = chain(Mode::Auto);
        assert_eq!(c.on_fail("c1", "send"), FailAction::Note);
        c.mark_sent("c1", 1);
        assert_eq!(c.on_fail("c1", "send"), FailAction::Note, "серия сброшена успехом");
    }

    #[test]
    fn ask_mode_proposes_and_never_sends_itself() {
        let c = chain(Mode::Ask);
        assert_eq!(c.on_done("s1", 100), vec![("c1".into(), Decision::Propose)]);
        c.mark_proposed("c1", "почини тесты");
        let st = c.state("c1", Mode::Ask);
        assert_eq!(st.phase, Phase::Proposed);
        assert_eq!(st.proposal.as_deref(), Some("почини тесты"));
        assert_eq!(st.step, 0, "в ручном режиме номер захода растёт только при отправке");
        // режим меняется на лету
        c.set_mode("c1", Mode::Auto);
        assert_eq!(c.on_done("s1", 101), vec![("c1".into(), Decision::Send(1))]);
    }

    #[test]
    fn state_shows_the_header_what_it_needs() {
        let c = Chains::new();
        let idle = c.state("c1", Mode::Auto);
        assert!(!idle.active);
        assert_eq!(idle.mode, Mode::Auto, "режим виден и без цепочки");
        assert_eq!(idle.max_steps, MAX_STEPS);

        c.watch("c1", "s1", Mode::Auto);
        c.on_done("s1", 100);
        c.mark_sent("c1", 1);
        let st = c.state("c1", Mode::Auto);
        assert_eq!((st.active, st.step, st.phase), (true, 1, Phase::Watching));
        assert_eq!(st.session_id.as_deref(), Some("s1"));
        assert!(!st.note.is_empty(), "шапке нужно словами, что сейчас в работе");
    }

    #[test]
    fn switching_the_session_starts_a_new_count() {
        let c = chain(Mode::Auto);
        c.on_done("s1", 100);
        c.mark_sent("c1", 1);
        c.watch("c1", "s2", Mode::Auto);
        let st = c.state("c1", Mode::Auto);
        assert_eq!((st.step, st.session_id.as_deref()), (0, Some("s2")));
        // ключ идемпотентности принадлежал прошлой сессии
        assert_eq!(c.on_done("s2", 100), vec![("c1".into(), Decision::Send(1))]);
    }

    // ── итог хода ─────────────────────────────────────────────────────────

    fn facts(cmds: &[&str], reply: &str) -> TurnFacts {
        TurnFacts {
            commands: cmds.iter().map(|s| s.to_string()).collect(),
            final_reply: reply.into(),
            ..Default::default()
        }
    }

    #[test]
    fn tests_verdict_reads_counters_not_words() {
        // зелёное
        let v = tests_verdict(&facts(&[], "test result: ok. 1028 passed; 0 failed")).unwrap();
        assert!(v.ok, "0 failed — это зелёное");
        // красное: цифра, а не подстрока
        let v = tests_verdict(&facts(&[], "test result: FAILED. 1020 passed; 10 failed")).unwrap();
        assert!(!v.ok, "«10 failed» не должно читаться как «0 failed»");
        assert!(v.line.contains("10 failed"), "основание видно: {}", v.line);
        // красное перевешивает зелёное в том же ходе
        let f = facts(&["cargo test — 939 passed"], "потом: 2 failed в clippy-тестах");
        assert!(!tests_verdict(&f).unwrap().ok);
        // тестов не было
        assert_eq!(tests_verdict(&facts(&["git status"], "поправил README")), None);
    }

    /// Тихих отказов не бывает: причина доходит СЛОВАМИ и не переписывается —
    /// «claude не найден», отказ гейта, мёртвая пана читаются как есть.
    #[test]
    fn refusals_reach_the_chat_in_words() {
        let c = chain(Mode::Auto);
        for reason in [
            "claude не найден — агент не запустился",
            "грант 'agent' не разрешает sessions.reply (control)",
            "Заход не ушёл: Агент не подтвердил получение — проверь терминал",
        ] {
            let t = fail_text(reason, false);
            assert!(t.contains(reason), "причину переписали: {t}");
            assert!(!t.contains("останавливаю"), "одна неудача цепочку не рвёт: {t}");
        }
        // вторая подряд по той же причине — стоп, и об этом тоже словами
        assert_eq!(c.on_fail("c1", "send"), FailAction::Note);
        let stopped = c.on_fail("c1", "send") == FailAction::Stop;
        let t = fail_text("tmux: пана мертва", stopped);
        assert!(t.starts_with("tmux: пана мертва"), "{t}");
        assert!(t.contains("вторая неудача подряд"), "человек должен понять причину стопа: {t}");
        // перевод строки в чужой ошибке не должен ломать карточку
        assert_eq!(fail_text("сбой\nвторая строка", false), "сбой вторая строка");
    }

    #[test]
    fn outcome_is_empty_when_nothing_was_collected() {
        assert!(build_outcome("s1", "jarvis", "", "", None).is_empty());
        assert!(!build_outcome("s1", "jarvis", "k", "починил", None).is_empty());
    }

    #[test]
    fn next_prompt_is_built_from_the_outcome() {
        let f = TurnFacts {
            files: vec![FileTouch { path: "src/a.rs".into(), kind: "edited".into() }],
            final_reply: "готово, 3 failed".into(),
            ..Default::default()
        };
        let o = build_outcome("s1", "jarvis", "k", "починил ретраи", Some(&f));
        let p = next_prompt(&o, 2);
        assert!(p.contains("Заход 2"), "номер захода в тексте: {p}");
        assert!(p.contains("починил ретраи"), "итог прошлого хода в тексте: {p}");
        assert!(p.contains("src/a.rs"), "что тронуто — в тексте: {p}");
        assert!(p.contains("КРАСНЫЕ"), "красные тесты обязаны попасть в заход: {p}");
        // без тестов заход всё равно осмысленный и непустой
        let o = build_outcome("s1", "jarvis", "k", "переписал доку", None);
        let p = next_prompt(&o, 1);
        assert!(p.contains("Проверку в прошлом ходе не прогоняли"));
        assert!(p.len() > 60);
    }
}
