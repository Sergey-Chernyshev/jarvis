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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use super::journal;
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

/// То же НОЧЬЮ — с первой. Некому посмотреть и поправить, а вторая попытка
/// вслепую стоит ровно столько же, сколько первая.
pub const MAX_SAME_FAILS_NIGHT: u32 = 1;

/// Сколько заходов подряд БЕЗ ПРОДВИЖЕНИЯ рвут цепочку днём. Один ход без следа
/// бывает законным (агент читал и разбирался), два — уже подозрительно, три —
/// карусель, и четвёртый её заход ничем не будет отличаться от третьего.
pub const MAX_STALE: u32 = 3;

/// То же ночью: круг стоит столько же, а заметить его некому.
pub const MAX_STALE_NIGHT: u32 = 2;

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
    /// Цепочка уступила сессию человеку и ждёт его решения.
    ///
    /// Заведено по живому случаю: Джарвис поставил сессии исследование по
    /// просьбе человека, оно встало в очередь, а следом в ту же сессию пришёл
    /// «Заход 2 из 10», и сессия ушла разбирать другую тему. У сессии не было
    /// владельца — в неё писали двое, и ни один не знал о другом.
    ///
    /// Пауза, а не стоп: цепочку человек заводил осознанно, и молча её ронять
    /// из-за одной своей реплики — потерять работу другим способом.
    Paused,
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
    /// Отпечаток прошлого хода и сколько заходов подряд он не менялся.
    mark: String,
    stale: u32,
    /// Про дневной потолок этой цепочке уже сказали. Днём потолок не рвёт
    /// работу (см. `CapAction`), но и повторять одно и то же на каждом заходе
    /// не должен: человек прочитал — дальше решать ему.
    cap_warned: bool,
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
    /// Отложенное до утра по этому чату. Состояние, а не строка в логе: «ждёт
    /// тебя» кто-то обязан показать, и шапка — первое место, куда человек смотрит.
    pub waiting: Vec<Waiting>,
    /// Журнал заходов этого чата — след, по которому видно, куда он ушёл.
    pub visits: Vec<Visit>,
    /// Расход именно этого чата: за ночь, за сутки, со своими и общими потолками.
    pub spend: Spend,
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
            waiting: Vec::new(),
            visits: Vec::new(),
            spend: Spend::default(),
        }
    }

    /// Приклеить отложенное. Отдельным шагом, на границе с приложением: реестр
    /// цепочек про ночной журнал не знает и знать не должен.
    fn with_waiting(mut self, waiting: Vec<Waiting>) -> Self {
        self.waiting = waiting;
        self
    }

    /// То же для журнала заходов и расхода: они переживают саму цепочку —
    /// оборванная ночью карусель обязана остаться видимой утром.
    fn with_log(mut self, visits: Vec<Visit>, spend: Spend) -> Self {
        self.visits = visits;
        self.spend = spend;
        self
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
    /// Оборвать цепочку: то же самое второй раз подряд (ночью — первый).
    Stop,
}

/// Сдвинулась ли работа с прошлого захода.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    /// Мир изменился — цепочка идёт дальше.
    Moved,
    /// Тот же ход повторился, но запас ещё есть (сколько раз подряд).
    Stale(u32),
    /// Карусель: столько заходов подряд без единого следа — стоп.
    Stuck(u32),
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
            mark: String::new(),
            stale: 0,
            cap_warned: false,
            proposal: None,
            note: String::new(),
        });
        // Сессия сменилась — это новая цепочка: и счётчик заходов, и серия
        // неудач, и ключ идемпотентности, и след продвижения принадлежали прошлой.
        if c.session_id != session_id {
            c.session_id = session_id.to_string();
            c.step = 0;
            c.last_done = None;
            c.fail_code.clear();
            c.fail_count = 0;
            c.mark.clear();
            c.stale = 0;
            c.cap_warned = false;
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

    /// Уступить сессию человеку: цепочки на ней встают на паузу.
    ///
    /// Промпт человека (в том числе переданный Джарвисом) ВСЕГДА главнее захода
    /// цепочки. Не потому, что он важнее по смыслу, а потому, что за ним стоит
    /// решение, а за заходом — догадка машины. Соревноваться за очередь они не
    /// должны: очередь решает «кто успел», а это не то правило, по которому
    /// человек хочет жить.
    ///
    /// Возвращает чаты, которые действительно встали, — по ним пойдут карточки.
    /// Уже стоящие на паузе не считаются: вторая карточка про то же самое хуже
    /// молчания.
    pub fn pause_for_human(&self, session_id: &str) -> Vec<String> {
        let mut m = self.map.lock().unwrap();
        let mut out = Vec::new();
        for (chat_id, c) in m.iter_mut() {
            if c.session_id != session_id || c.phase == Phase::Paused {
                continue;
            }
            c.phase = Phase::Paused;
            c.note = "пауза: пришла задача от человека".into();
            // Предложение снимаем: оно сочинялось под прежний ход сессии, а тот
            // человек сейчас перебьёт. Отправить его позже значило бы отправить
            // заход, построенный на устаревшем итоге.
            c.proposal = None;
            out.push(chat_id.clone());
        }
        out.sort();
        out
    }

    /// Вернуть цепочку в работу после паузы. `false` — цепочки уже нет.
    pub fn resume(&self, chat_id: &str) -> bool {
        let mut m = self.map.lock().unwrap();
        match m.get_mut(chat_id) {
            Some(c) if c.phase == Phase::Paused => {
                c.phase = Phase::Watching;
                c.note = "продолжаю".into();
                true
            }
            _ => false,
        }
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
            // Сессия занята человеком. «Закончил» относится к ЕГО задаче, и
            // строить на нём следующий заход — это ровно тот случай, где
            // цепочка перебивала работу и уводила сессию в другую тему.
            //
            // `last_done` при этом НЕ ставим: заход по этому «закончил» ещё не
            // сделан, и после «продолжить» цепочке должно быть от чего
            // оттолкнуться. Иначе пауза молча съедала бы один шаг.
            if c.phase == Phase::Paused {
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
    /// удаляется, дальше нужна рука человека. Ночью хватает первой.
    pub fn on_fail(&self, chat_id: &str, code: &str, night: bool) -> FailAction {
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
        let limit = if night { MAX_SAME_FAILS_NIGHT } else { MAX_SAME_FAILS };
        if c.fail_count >= limit {
            m.remove(chat_id);
            return FailAction::Stop;
        }
        c.phase = Phase::Watching;
        c.note = "заход не удался".into();
        FailAction::Note
    }

    /// Сдвинулось ли что-нибудь с прошлого захода. `mark` — отпечаток хода
    /// (`progress_mark`); совпал с прошлым — заход прошёл впустую.
    ///
    /// Потолок глубины ловит долгую работу, этот счётчик — БЕСПЛОДНУЮ: десять
    /// одинаковых заходов упрутся в глубину только через час, а стоят как час.
    pub fn note_progress(&self, chat_id: &str, mark: &str, night: bool) -> Progress {
        let mut m = self.map.lock().unwrap();
        let Some(c) = m.get_mut(chat_id) else {
            return Progress::Moved;
        };
        // Первый заход сравнивать не с чем — это ещё не топтание на месте.
        if c.mark.is_empty() || c.mark != mark {
            c.mark = mark.to_string();
            c.stale = 0;
            return Progress::Moved;
        }
        c.stale += 1;
        let limit = if night { MAX_STALE_NIGHT } else { MAX_STALE };
        if c.stale >= limit {
            let n = c.stale;
            m.remove(chat_id);
            return Progress::Stuck(n);
        }
        c.phase = Phase::Watching;
        c.note = format!("заход {} ничего не изменил", c.step);
        Progress::Stale(c.stale)
    }

    /// Сказать про потолок, который днём не рвёт цепочку, — ОДИН раз за
    /// цепочку. `true` — говорим, `false` — уже сказали (или цепочки нет).
    ///
    /// Признак живёт в записи цепочки, а не в журнале: «уже предупредили»
    /// относится к этой конкретной работе. Новая цепочка — новое решение
    /// человека продолжать, и предупредить его надо снова.
    pub fn note_cap_warning(&self, chat_id: &str) -> bool {
        let mut m = self.map.lock().unwrap();
        let Some(c) = m.get_mut(chat_id) else {
            return false;
        };
        !std::mem::replace(&mut c.cap_warned, true)
    }

    /// Тот же отпечаток, что и в прошлый раз? ЧТЕНИЕ поля `mark`, которым
    /// владеет `note_progress` (Stuck) — второго хранилища для одного и того
    /// же следа заводить незачем. Разница с `note_progress` в том, что этот
    /// метод ничего не мутирует и не считает счётчик подряд: он отвечает
    /// ровно на один вопрос — «есть ли в этом ходе новый факт про мир» — и
    /// нужен `exhausted`, а не карусели.
    ///
    /// Первый заход сравнивать не с чем (`mark` пуст) — и это `false`,
    /// «неизвестно», а не «не менялось»: на пустой истории объявлять ход
    /// исчерпанным было бы чистой выдумкой.
    fn mark_unchanged(&self, chat_id: &str, mark: &str) -> bool {
        let m = self.map.lock().unwrap();
        match m.get(chat_id) {
            Some(c) => !c.mark.is_empty() && c.mark == mark,
            None => false,
        }
    }

    /// Ход не добавил ценности: ни файла, ни команды, ни нового вердикта
    /// проверки, ни незакрытого вопроса в ответе. Это ДРУГОЙ вопрос, чем
    /// `Progress::Stuck` рядом: там несколько заходов ПОДРЯД топчутся на
    /// одном и том же — карусель, и это СБОЙ, обрыв на счётчике. Здесь —
    /// ОДИН ход, который сам расписался как законченный: агенту нечего
    /// добавить, и это УСПЕХ, а не сбой. Смешивать их в один детектор нельзя:
    /// у карусели и у законченной работы разный смысл и разное сообщение
    /// человеку, а разное надо разносить, а не сливать через месяц забывчиво.
    ///
    /// Порядок проверок — от дешёвой и однозначной к дорогой и нечёткой:
    /// файлы и команды видны без разбора текста, вердикт теста — счётчик, а
    /// «нет ли незакрытого вопроса» — это разбор слов ответа, самый ненадёжный
    /// шаг, и он идёт последним и с намеренным перекосом: нет уверенности —
    /// `false` (не исчерпано). Ложная остановка стоит дороже, чем кажется:
    /// «ложное продолжение» — лишний заход ценой в четверть доллара под
    /// взглядом человека (день) или до утра (ночь, где уже стоят потолок
    /// глубины и Stuck), а «ложная остановка» отдаёт цепочку человеку с
    /// подписью «сделано», хотя работа брошена на середине, — и это дороже
    /// любого лишнего захода.
    pub fn exhausted(&self, chat_id: &str, o: &Outcome) -> bool {
        if !o.files.is_empty() || !o.commands.is_empty() {
            return false;
        }
        let tests_ok = o.tests.as_ref().map_or(true, |t| t.ok);
        if !tests_ok {
            return false;
        }
        if !self.mark_unchanged(chat_id, &progress_mark(o)) {
            return false;
        }
        reply_reads_as_done(&o.reply)
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
            waiting: Vec::new(),
            visits: Vec::new(),
            spend: Spend::default(),
        },
        None => ChainState::idle(chat_id, Mode::Ask),
    }
}

/// Процессный реестр цепочек — один на приложение (как `turns_in_flight`).
pub fn chains() -> &'static Chains {
    static C: std::sync::OnceLock<Chains> = std::sync::OnceLock::new();
    C.get_or_init(Chains::new)
}

// ── Ночь ──────────────────────────────────────────────────────────────────
//
// Ночью Джарвис РАБОТАЕТ, а не копит до утра: цепочка идёт сама. Меняются три
// вещи. Ограничители строже — поправить некому. Необратимое не делается вовсе —
// оно откладывается в «ждёт тебя», а не проскакивает по принципу «спросить
// некого, значит можно». И тихо: карточки в чат ложатся, но будить звуком
// некого, поэтому уведомления копятся и выходят утром одной сводкой.

/// Сколько ночных уведомлений держим. Больше двух сотен за ночь — это уже не
/// сводка, а лента; храним хвост и ЧЕСТНО говорим, сколько отброшено: тишина не
/// имеет права съедать сигнал молча.
const MAX_NOTICES: usize = 200;

/// Сколько живёт непоказанное уведомление. Копилка переживает перезапуск, а
/// значит переживёт и неделю простоя — и тогда утренняя сводка рассказала бы
/// про позапрошлую ночь как про эту. Двое суток: ночь с перезапусками в них
/// укладывается целиком, а забытая неделя — нет. Отложенное (`waiting`) под этот
/// нож не идёт: оно ждёт РЕШЕНИЯ человека, а решение не протухает.
const NOTICE_TTL_MS: i64 = 2 * DAY_MS;

/// Сколько отложенных решений держим. Ночь их даёт единицы (каждое ОБРЫВАЕТ
/// цепочку), так что сотня — это уже не очередь решений, а склад.
const MAX_WAITING: usize = 100;

/// Отложенное до утра: необратимое, которое ночью не делается вовсе.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Waiting {
    pub chat_id: String,
    pub session_id: String,
    pub at: i64,
    /// Что именно необратимо — словами («пуш в main», «слияние веток»).
    pub kind: String,
    /// Заход целиком: утром человек отправляет его кнопкой, ничего не переписывая.
    pub prompt: String,
}

/// Уведомление, которое ночью не показали.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Notice {
    pub at: i64,
    pub chat_id: String,
    pub kind: String,
    pub text: String,
}

/// Ночь целиком: что не показали, что ждёт человека, сколько потеряли по потолку.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NightState {
    pub notices: Vec<Notice>,
    pub waiting: Vec<Waiting>,
    pub dropped: u32,
}

/// Ночной журнал. Отдельный тип (как `Chains`) — чтобы тесты гоняли его без
/// живого приложения и не дрались за один процессный экземпляр.
///
/// ПЕРЕЖИВАЕТ ПЕРЕЗАПУСК: ночь — это не время жизни процесса. Приложение,
/// перезапустившееся в три часа, обязано помнить, что копилось до этого, иначе
/// утренняя сводка отвечает на «что было ночью» пустотой — то есть врёт.
#[derive(Default)]
pub struct NightLog {
    inner: Mutex<NightState>,
    /// Куда класть. `None` — копилка живёт только в памяти (тесты).
    file: Option<PathBuf>,
}

impl NightLog {
    /// Копилка без диска: тесты гоняют её, не трогая `~/.jarvis`.
    pub fn new() -> Self {
        NightLog::default()
    }

    /// Копилка, которая переживает перезапуск. Протухшие уведомления с диска не
    /// поднимаем (см. `NOTICE_TTL_MS`), но и не съедаем молча — говорим в лог.
    pub fn at(path: PathBuf, now: i64) -> Self {
        let mut st: NightState = journal::read_at(&path);
        let before = st.notices.len();
        st.notices.retain(|n| now - n.at <= NOTICE_TTL_MS);
        let stale = before - st.notices.len();
        if stale > 0 {
            crate::log::line(&format!(
                "[chain] ночная копилка: {stale} уведомлений старше двух суток — не показываю"
            ));
        }
        NightLog { inner: Mutex::new(st), file: Some(path) }
    }

    fn save(&self, st: &NightState) {
        journal::save(self.file.as_ref(), "ночная копилка", st);
    }

    /// Не показать сейчас — показать утром. Ровно та строка, которую человек
    /// прочитал бы на карточке: пересказ по памяти утром уже не восстановить.
    pub fn hush(&self, chat_id: &str, kind: &str, text: &str) {
        let st = {
            let mut st = self.inner.lock().unwrap();
            st.notices.push(Notice {
                at: now_ms(),
                chat_id: chat_id.to_string(),
                kind: kind.to_string(),
                text: text.to_string(),
            });
            if st.notices.len() > MAX_NOTICES {
                st.notices.remove(0);
                st.dropped += 1;
            }
            st.clone()
        };
        self.save(&st);
    }

    /// Отложить необратимое. Повтор того же захода по тому же чату не плодит
    /// вторую строчку: человеку решать один раз.
    pub fn defer(&self, w: Waiting) {
        let st = {
            let mut st = self.inner.lock().unwrap();
            if st
                .waiting
                .iter()
                .any(|x| x.chat_id == w.chat_id && x.prompt == w.prompt)
            {
                return;
            }
            st.waiting.push(w);
            // Отложенное ждёт решения человека и само не протухает — но и расти
            // бесконечно в файле не может. Сотня нерешённых необратимых значит,
            // что решать их давно перестали; самое давнее уходит со строкой в
            // логе, потому что молча терять решение нельзя.
            while st.waiting.len() > MAX_WAITING {
                let old = st.waiting.remove(0);
                crate::log::line(&format!(
                    "[chain] отложенных больше {MAX_WAITING} — вытеснено самое давнее ({}, {})",
                    old.kind, old.chat_id
                ));
            }
            st.clone()
        };
        self.save(&st);
    }

    /// Что ждёт решения по этому чату.
    pub fn for_chat(&self, chat_id: &str) -> Vec<Waiting> {
        let st = self.inner.lock().unwrap();
        st.waiting
            .iter()
            .filter(|w| w.chat_id == chat_id)
            .cloned()
            .collect()
    }

    /// Человек разобрался (отправил заход или оборвал цепочку) — снять «ждёт тебя».
    pub fn resolve(&self, chat_id: &str) -> usize {
        let (gone, st) = {
            let mut st = self.inner.lock().unwrap();
            let before = st.waiting.len();
            st.waiting.retain(|w| w.chat_id != chat_id);
            (before - st.waiting.len(), st.clone())
        };
        if gone > 0 {
            self.save(&st);
        }
        gone
    }

    /// Копилось ли вообще что-нибудь. Самая дешёвая проверка журнала — её и
    /// зовут на горячем пути, до всяких вопросов «а ночь ли сейчас».
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().notices.is_empty()
    }

    /// Забрать накопленное для утренней сводки.
    ///
    /// Уведомления ЗАБИРАЕМ (сводка одна, второй такой же быть не должно), а
    /// «ждёт тебя» ОСТАВЛЯЕМ: оно снимается решением человека, а не рассказом о
    /// нём. Пустой журнал даёт None — утром без ночи сводке взяться неоткуда.
    pub fn drain(&self) -> Option<NightState> {
        let (out, left) = {
            let mut st = self.inner.lock().unwrap();
            if st.notices.is_empty() {
                return None;
            }
            let out = NightState {
                notices: std::mem::take(&mut st.notices),
                waiting: st.waiting.clone(),
                dropped: std::mem::take(&mut st.dropped),
            };
            (out, st.clone())
        };
        // Осушённую копилку кладём на диск сразу: перезапуск сразу после сводки
        // не имеет права выкатить её второй раз.
        self.save(&left);
        Some(out)
    }
}

/// Процессный ночной журнал — один на приложение, с файлом за спиной.
pub fn night_log() -> &'static NightLog {
    static N: std::sync::OnceLock<NightLog> = std::sync::OnceLock::new();
    N.get_or_init(|| NightLog::at(journal::night_file(), now_ms()))
}

// ── Журнал заходов и расход по чату ───────────────────────────────────────
//
// «Уехать не туда» лечится не запретом, а СЛЕДОМ: у автономного чата обязан
// быть журнал заходов — что решил, что запустил, что изменилось и во сколько
// обошлось. Без него «дорого» остаётся ощущением, а утро начинается с вопроса
// «куда он ушёл, пока я спал», на который ответить нечем.

/// Сутки — окно счётчика расхода. Ровно те же сутки, что у человека: «за ночь»
/// и «за сутки» — это про одно засыпание, а не про календарь.
const DAY_MS: i64 = 86_400_000;

/// Сколько заходов помним по чату. Ночь автономного чата — это десятки заходов,
/// а не сотни: потолок глубины рвёт цепочку на десятом. Двести хватает на ночь
/// с перезапусками, а хвост старше суток из счёта выпадает и так.
const MAX_VISITS: usize = 200;

/// Ночной потолок ОДНОГО автономного чата, доллары — ДЕФОЛТ. Заход стоит
/// порядка четверти доллара, потолок глубины — десять заходов: три доллара это
/// полная цепочка с запасом, то есть ровно та работа, которую отдают на ночь.
///
/// Дефолт, а не константа поведения: владелец меняет режимы по ситуации, значит
/// и потолки захочет — числа живут в настройках (`Caps`), здесь только то, с
/// чего Джарвис начинает.
pub const CHAT_NIGHT_USD: f64 = 3.0;

/// Дневная норма одного автономного чата. Днём человек рядом и видит, куда
/// уходит время, — норма втрое шире ночной и служит ОТМЕТКОЙ («столько уже
/// ушло»), а не стопом: днём этот потолок предупреждает, см. `CapAction`.
pub const CHAT_DAY_USD: f64 = 10.0;

/// Общий ночной потолок ВСЕХ автономных чатов. Свой потолок держит один чат в
/// рамках, но трое таких, каждый «в своих рамках», съедают втрое больше — и
/// именно это владелец назвал недельным бюджетом. Две полные цепочки за ночь на
/// всех: третья встаёт и говорит об этом словами.
pub const ALL_NIGHT_USD: f64 = 6.0;

/// То же на сутки.
pub const ALL_DAY_USD: f64 = 20.0;

/// Выше этого потолок перестаёт быть потолком: столько автономия не тратит и за
/// неделю, а лишний ноль в поле — обычная опечатка.
const MAX_CAP_USD: f64 = 1000.0;

/// Потолки автономии: блок `autonomy` в `~/.jarvis/settings.json`.
///
/// Агенту на запись НЕ отдаются — ключа нет в `SETTINGS_ALLOWLIST`, ровно как у
/// `budget`: агент, вправе поднявший себе потолок, потолка не имеет. Правит их
/// человек — панелью или файлом.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Caps {
    pub chat_night: f64,
    pub chat_day: f64,
    pub all_night: f64,
    pub all_day: f64,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            chat_night: CHAT_NIGHT_USD,
            chat_day: CHAT_DAY_USD,
            all_night: ALL_NIGHT_USD,
            all_day: ALL_DAY_USD,
        }
    }
}

impl Caps {
    /// Поле за полем с фолбэком на дефолт — как у бюджета: `settings.json`
    /// мержится только по верхнему уровню, и частичный блок иначе снёс бы
    /// остальные числа.
    ///
    /// Ноль — ЗАКОННОЕ значение («автономии денег не даю»), поэтому дефолт
    /// подставляется только там, где числа нет вовсе или оно не число. Мусор
    /// (минус, NaN) не отменяет потолок, а прижимается к нулю: настройка, из
    /// которой получился запрет, честнее настройки, которая молча исчезла.
    pub fn from_settings(s: &Value) -> Self {
        let d = Caps::default();
        let num = |k: &str, def: f64| {
            s.pointer(&format!("/autonomy/{k}"))
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite())
                .unwrap_or(def)
                .clamp(0.0, MAX_CAP_USD)
        };
        Caps {
            chat_night: num("chatNightUsd", d.chat_night),
            chat_day: num("chatDayUsd", d.chat_day),
            all_night: num("allNightUsd", d.all_night),
            all_day: num("allDayUsd", d.all_day),
        }
    }
}

/// Окно счёта: «сейчас», начало ночи, к которой это «сейчас» относится, и
/// потолки. Считается ОДИН раз на событие и передаётся вниз: спрашивать
/// настройки и границы ночи в каждом суммировании значит показать в одном
/// ответе числа, посчитанные по-разному.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub now: i64,
    /// С какого момента идёт ночь, к которой относится `now` (а если сейчас
    /// день — последняя прошедшая).
    pub night_since: i64,
    pub caps: Caps,
}

impl Window {
    pub fn at(now: i64, night_since: i64, caps: Caps) -> Self {
        Window { now, night_since, caps }
    }

    /// Границ ночи не спросить (демона нет, в настройках мусор) — считаем
    /// сутки, как считалось до окна. Ошибка идёт в сторону «посчитали лишнего»:
    /// потолок сработает раньше, а не позже.
    pub fn rolling(now: i64, caps: Caps) -> Self {
        Window::at(now, now - DAY_MS, caps)
    }
}

/// Шаг поиска границы ночи: грубый и точный.
const PROBE_MS: i64 = 5 * 60_000;
const MINUTE_MS: i64 = 60_000;

/// Когда началась ночь, к которой относится момент `now` (а если сейчас день —
/// последняя прошедшая).
///
/// Ночь — ОКНО, а не время жизни процесса. Перезапуск в три часа не имеет права
/// обнулить ночной счёт, а позавчерашняя ночь не имеет права в него попасть;
/// «за последние сутки» не годится ни для того, ни для другого.
///
/// Границы окна здесь НЕ вычисляются: второго определения ночи в проекте нет и
/// не будет (на это есть сторожевой тест). Мы шагаем назад и спрашиваем ТОТ ЖЕ
/// предикат бюджета, пока он не скажет «уже не ночь». Ответ точен до минуты —
/// ровно с той точностью, с какой границы и заданы.
///
/// `None` — ночи в последних сутках не нашлось (окно выключено, границы
/// мусорные или Джарвиса не было целый день): считать тогда нечего, и счёт
/// идёт по суткам, см. `Window::rolling`.
pub fn night_since(now: i64, is_night: impl Fn(i64) -> bool) -> Option<i64> {
    let mut t = now;
    while !is_night(t) {
        t -= PROBE_MS;
        if now - t > DAY_MS {
            return None;
        }
    }
    while now - t <= DAY_MS && is_night(t - PROBE_MS) {
        t -= PROBE_MS;
    }
    for _ in 0..(PROBE_MS / MINUTE_MS) {
        if !is_night(t - MINUTE_MS) {
            break;
        }
        t -= MINUTE_MS;
    }
    Some(t)
}

/// Один заход в журнале: не строка в логе, а структура — её показывают.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Visit {
    pub at: i64,
    pub chat_id: String,
    pub session_id: String,
    pub step: u32,
    /// Чем кончился заход: `sent`, `deferred`, `stopped`.
    pub kind: String,
    /// Что решил — заход одной строкой.
    pub decided: String,
    /// Что запустил — команды хода, по которому заход и построен.
    pub ran: Vec<String>,
    /// Что изменилось — файлы и вердикт проверки.
    pub changed: String,
    /// Заход был ночным. Границы ночи спрашиваются у бюджета, здесь только след.
    pub night: bool,
    /// Прирост расхода сессии с прошлого замера, доллары. `None` — сравнивать
    /// не с чем или `usage` промолчал; ноль вместо этого был бы ложью.
    pub usd: Option<f64>,
}

/// Расход чата с потолками — то, что видит человек.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Spend {
    pub night: f64,
    pub day: f64,
    /// Заходов за сутки — расход без их числа ни о чём не говорит.
    pub visits: u32,
    /// Считали ли вообще. `false` — чисел нет, и «0.00$» соврало бы точностью.
    pub known: bool,
    pub night_cap: f64,
    pub day_cap: f64,
    /// Он же по всем автономным чатам разом — второй, независимый ограничитель.
    pub all_night: f64,
    pub all_night_cap: f64,
    pub all_day: f64,
    pub all_day_cap: f64,
}

/// Сумма расхода за окно. Второй ответ — считали ли вообще: ноль из нулей и
/// ноль из пустоты значат разное, и путать их нельзя.
///
/// НОЧЬ считается от начала текущей ночи (`Window::night_since`), а не «за
/// последние сутки»: иначе после полуночи в сегодняшнюю ночь попадала бы
/// вчерашняя, а перезапуск — наоборот, обнулял бы всё. Сутки остаются
/// скользящими: «сколько за сегодня» человек и имеет в виду как «за 24 часа».
fn sum_usd(list: &[Visit], w: &Window, night_only: bool) -> (f64, bool) {
    let mut usd = 0.0;
    let mut known = false;
    let fits = |v: &Visit| match night_only {
        true => v.night && v.at >= w.night_since && v.at <= w.now,
        false => w.now - v.at <= DAY_MS,
    };
    for v in list.iter().filter(|v| fits(v)) {
        if let Some(x) = v.usd {
            usd += x;
            known = true;
        }
    }
    (usd, known)
}

/// Что делает потолок, в который упёрлись.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapAction {
    /// Рвёт цепочку: дальше нужна рука человека.
    Stop,
    /// Говорит словами и пропускает заход дальше.
    Warn,
}

/// Упёрлись в потолок: что говорим и что делаем.
#[derive(Debug, Clone, PartialEq)]
pub struct CapHit {
    pub action: CapAction,
    pub text: String,
}

/// Журнал заходов. Отдельный тип (как `Chains` и `NightLog`) — чтобы тесты
/// гоняли его без живого приложения и не дрались за процессный экземпляр.
///
/// ПЕРЕЖИВАЕТ ПЕРЕЗАПУСК: журнал — это ответ на «куда он ушёл, пока я спал», и
/// приложение, перезапустившееся ночью, обязано отвечать на него так же, как
/// проработавшее ночь целиком. Из него же складываются ночные счётчики: они
/// привязаны к окну ночи, а не ко времени жизни процесса.
#[derive(Default)]
pub struct Visits {
    book: Mutex<Book>,
    /// Куда класть. `None` — журнал живёт только в памяти (тесты).
    file: Option<PathBuf>,
}

/// Журнал на диске целиком.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Book {
    /// Чат → его заходы, свежие в конце. BTreeMap, чтобы файл не перемешивался
    /// от записи к записи: глазами его тоже читают.
    pub log: BTreeMap<String, Vec<Visit>>,
    /// Сессия → её стоимость на прошлом замере. Без неё первый заход после
    /// перезапуска записал бы «не знаю» вместо своей цены: `usage` считает от
    /// начала сессии и переживает перезапуск сам, а сравнивать было бы не с чем.
    pub marks: BTreeMap<String, f64>,
}

/// Сколько чатов помним. Журнал не уходит вместе с чатом (скрытие и потеря
/// указателя — не повод стирать след), поэтому ключи копятся: режем те, где
/// давно ничего не было.
const MAX_CHATS: usize = 50;

/// Убрать чаты, в которых давно ничего не происходило.
fn prune_chats(book: &mut Book) {
    let extra = book.log.len().saturating_sub(MAX_CHATS);
    if extra == 0 {
        return;
    }
    let mut by_age: Vec<(i64, String)> = book
        .log
        .iter()
        .map(|(k, l)| (l.last().map(|v| v.at).unwrap_or(0), k.clone()))
        .collect();
    by_age.sort_unstable();
    for (_, id) in by_age.into_iter().take(extra) {
        book.log.remove(&id);
    }
}

/// Отметки расхода живут ровно столько, сколько их сессии в журнале: журнал
/// ограничен потолками, значит и отметок не накопится. Чистим ПОСЛЕ записи
/// захода — до неё сессия в журнале ещё не значится, и свежая отметка попала бы
/// под собственный нож.
fn prune_marks(book: &mut Book) {
    let dead: Vec<String> = {
        let alive: HashSet<&str> = book
            .log
            .values()
            .flatten()
            .map(|v| v.session_id.as_str())
            .collect();
        book.marks
            .keys()
            .filter(|s| !alive.contains(s.as_str()))
            .cloned()
            .collect()
    };
    for s in dead {
        book.marks.remove(&s);
    }
}

impl Visits {
    /// Журнал без диска: тесты гоняют его, не трогая `~/.jarvis`.
    pub fn new() -> Self {
        Visits::default()
    }

    /// Журнал, который переживает перезапуск.
    pub fn at(path: PathBuf) -> Self {
        Visits { book: Mutex::new(journal::read_at(&path)), file: Some(path) }
    }

    fn save(&self, book: &Book) {
        journal::save(self.file.as_ref(), "журнал заходов", book);
    }

    /// Во сколько обошёлся заход. Своих чисел у цепочки нет и быть не должно:
    /// токены считает `usage`, и берём мы у него ПРИРОСТ стоимости сессии с
    /// прошлого замера. Первый замер сравнивать не с чем — он только ставит
    /// отметку (тот же приём, что у `note_progress`): сессия могла работать и до
    /// цепочки, и записать её прошлое на эту ночь значит соврать числом.
    ///
    /// Отметка ложится на диск: перезапуск не должен превращать цену первого
    /// же ночного захода в «не знаю».
    pub fn delta(&self, session_id: &str, total: Option<f64>) -> Option<f64> {
        let total = total?;
        let (prev, book) = {
            let mut b = self.book.lock().unwrap();
            let prev = b.marks.insert(session_id.to_string(), total);
            (prev, b.clone())
        };
        self.save(&book);
        prev.map(|p| (total - p).max(0.0))
    }

    /// Записать заход. Хвост старше суток в счёт не идёт, но из журнала не
    /// выпадает: утром человек читает ночь целиком.
    pub fn note(&self, v: Visit) {
        let book = {
            let mut b = self.book.lock().unwrap();
            let list = b.log.entry(v.chat_id.clone()).or_default();
            list.push(v);
            if list.len() > MAX_VISITS {
                list.remove(0);
            }
            prune_chats(&mut b);
            prune_marks(&mut b);
            b.clone()
        };
        self.save(&book);
    }

    /// Журнал чата, свежие в конце.
    pub fn for_chat(&self, chat_id: &str) -> Vec<Visit> {
        self.book.lock().unwrap().log.get(chat_id).cloned().unwrap_or_default()
    }

    /// Ночные заходы всех чатов за ЭТУ ночь, в порядке времени, — из них и
    /// складывается утренний ответ «куда он ушёл, пока я спал».
    pub fn night_visits(&self, w: &Window) -> Vec<Visit> {
        let b = self.book.lock().unwrap();
        let mut out: Vec<Visit> = b
            .log
            .values()
            .flatten()
            .filter(|v| v.night && v.at >= w.night_since && v.at <= w.now)
            .cloned()
            .collect();
        out.sort_by_key(|v| v.at);
        out
    }

    /// Расход чата — свой и общий разом: одно без другого не решает ничего.
    pub fn spend(&self, chat_id: &str, w: &Window) -> Spend {
        let b = self.book.lock().unwrap();
        let mine: &[Visit] = b.log.get(chat_id).map(Vec::as_slice).unwrap_or_default();
        let (night, kn) = sum_usd(mine, w, true);
        let (day, kd) = sum_usd(mine, w, false);
        let mut all_night = 0.0;
        let mut all_day = 0.0;
        for l in b.log.values() {
            all_night += sum_usd(l, w, true).0;
            all_day += sum_usd(l, w, false).0;
        }
        Spend {
            night,
            day,
            visits: mine.iter().filter(|v| w.now - v.at <= DAY_MS).count() as u32,
            known: kn || kd,
            night_cap: w.caps.chat_night,
            day_cap: w.caps.chat_day,
            all_night,
            all_night_cap: w.caps.all_night,
            all_day,
            all_day_cap: w.caps.all_day,
        }
    }

    /// Упёрлись ли в потолок — и что с этим делать. СВОЙ и ОБЩИЙ проверяются
    /// оба: свой держит один чат в рамках, общий — всех разом, и три автономных
    /// чата, у каждого из которых всё в порядке, вместе съедают втрое больше.
    ///
    /// Это не замена ступеням бюджета (`budget.rs`): там недельная шкала
    /// провайдера и ночной потолок в процентах на всё приложение, здесь — деньги
    /// конкретных чатов. Оба спрашиваются, и любой из них может сказать «стоп».
    ///
    /// НОЧЬЮ ПОТОЛОК ОСТАНАВЛИВАЕТ, ДНЁМ ПРЕДУПРЕЖДАЕТ. Это не разные правила
    /// для одного числа, а одно правило: цена ошибки зависит от того, есть ли
    /// рядом человек. Ночью посмотреть некому — оборванная зря цепочка ждёт до
    /// утра, а необорванная тратит до утра, и второе хуже. Днём человек рядом,
    /// расход стоит у него в шапке и в строке списка: стоп отнял бы у него
    /// решение, которое он и так принимает глазами, а цена продолжения — один
    /// заход (порядка четверти доллара) и по-прежнему конечна: её держат потолок
    /// глубины, детектор карусели и ступень провайдера, которые никуда не делись.
    ///
    /// Чисел нет — запрета нет: врать потолком, которого не посчитали, хуже, чем
    /// пропустить заход; про молчание счётчика человек узнаёт из шапки.
    pub fn cap_hit(&self, chat_id: &str, night: bool, w: &Window) -> Option<CapHit> {
        let s = self.spend(chat_id, w);
        if !s.known {
            return None;
        }
        // Ночью останавливает ЛЮБОЙ из потолков — в том числе дневной: ночь
        // идёт внутри суток, и «дневная норма кончилась» ночью значит ровно то
        // же, что ночная.
        let tail = |a: CapAction| match a {
            CapAction::Stop => "Цепочку останавливаю",
            CapAction::Warn => {
                "Днём это предупреждение, а не стоп: ты рядом и видишь расход — \
                 останови сам, если заход лишний. Ночью на этом месте цепочка встала бы"
            }
        };
        let own = |what: &str, spent: f64, cap: f64, a: CapAction| CapHit {
            action: a,
            text: format!(
                "{what} потолок этого чата — {cap:.2}$, потрачено {spent:.2}$. {}",
                tail(a)
            ),
        };
        let all = |what: &str, spent: f64, cap: f64, mine: f64, a: CapAction| CapHit {
            action: a,
            text: format!(
                "{what} потолок ВСЕХ автономных чатов — {cap:.2}$, вместе они потратили {spent:.2}$. \
                 Этот чат в своих рамках ({mine:.2}$), но общий бюджет кончился. {}",
                tail(a)
            ),
        };
        if night && s.night >= s.night_cap {
            return Some(own("Ночной", s.night, s.night_cap, CapAction::Stop));
        }
        if night && s.all_night >= s.all_night_cap {
            return Some(all("Общий ночной", s.all_night, s.all_night_cap, s.night, CapAction::Stop));
        }
        let by_day = if night { CapAction::Stop } else { CapAction::Warn };
        if s.day >= s.day_cap {
            return Some(own("Дневной", s.day, s.day_cap, by_day));
        }
        if s.all_day >= s.all_day_cap {
            return Some(all("Общий дневной", s.all_day, s.all_day_cap, s.day, by_day));
        }
        None
    }
}

/// Процессный журнал заходов — один на приложение, с файлом за спиной.
pub fn visits() -> &'static Visits {
    static V: std::sync::OnceLock<Visits> = std::sync::OnceLock::new();
    V.get_or_init(|| Visits::at(journal::visits_file()))
}

/// Ночь ли сейчас.
///
/// Предикат — ЗА БЮДЖЕТОМ: границы ночи там же, где ночной потолок расхода, и
/// второй копии этого решения быть не должно (тем более часов в коде). ЖДЁМ:
/// `crate::budget::is_night(&Arc<Daemon>) -> bool`. Здесь — только переходник:
/// цепочка везде держит в руках `AppHandle`, а не демона.
///
/// Без демона (тесты, ранний старт) ночи не бывает. Ошибиться в сторону дневных
/// правил безопаснее: ночные запреты остановили бы работу, которую человек и так
/// видит своими глазами.
pub fn is_night(app: &AppHandle) -> bool {
    let Some(d) = tauri::Manager::try_state::<Arc<Daemon>>(app) else {
        return false; // без демона правил ночи не спросить — считаем днём
    };
    crate::budget::is_night(&d)
}

/// Настройки: у демона, а если его не спросить — читаем файл сами.
///
/// Без демона (ранний старт) и БЕЗ `AppHandle` вовсе (список чатов рисуется без
/// него, `history::chats_json`) числа всё равно нужны настоящие: показать
/// дефолтный потолок там, где человек поставил свой, — соврать. Чтение здесь
/// редкое (событие цепочки, открытие списка), а Store не пишет — второй читатель
/// того же файла безопасен.
fn settings_of(app: Option<&AppHandle>) -> Value {
    match app.and_then(tauri::Manager::try_state::<Arc<Daemon>>) {
        Some(d) => d.settings.load(),
        None => crate::settings::Store::new().load(),
    }
}

/// Окно счёта на «сейчас»: потолки из настроек и начало текущей ночи.
pub fn window_of(app: Option<&AppHandle>) -> Window {
    let now = now_ms();
    let s = settings_of(app);
    let caps = Caps::from_settings(&s);
    // Границы ночи — у бюджета, и спрашиваем мы их его же предикатом: свой
    // здесь завести значило бы получить две разные ночи в одном приложении.
    let cfg = crate::budget::Cfg::from_settings(&s);
    match night_since(now, |t| crate::budget::is_night_at(&cfg, t)) {
        Some(since) => Window::at(now, since, caps),
        None => Window::rolling(now, caps),
    }
}

/// То же там, где `AppHandle` есть, — а он есть везде, кроме списка чатов.
pub fn window(app: &AppHandle) -> Window {
    window_of(Some(app))
}

/// Сколько стоила ночь — числа тоже за бюджетом: у цепочки своих нет.
/// ЖДЁМ оттуда же `report(&Arc<Daemon>)` с `providers.*.nightSpentPct` и
/// `night.capPct` — из них и складывается строка для сводки.
fn night_spent(app: &AppHandle) -> Option<String> {
    let d = tauri::Manager::try_state::<Arc<Daemon>>(app)?;
    let r = crate::budget::report(&d);
    let cap = r.get("night").and_then(|n| n.get("capPct")).and_then(serde_json::Value::as_f64);
    // По каждому провайдеру, у кого есть число: «claude 12.4% из 15%».
    let mut parts = Vec::new();
    if let Some(ps) = r.get("providers").and_then(serde_json::Value::as_object) {
        for (name, v) in ps {
            let Some(spent) = v.get("nightSpentPct").and_then(serde_json::Value::as_f64) else {
                continue;
            };
            parts.push(match cap {
                Some(c) => format!("{name} {spent:.1}% из {c:.0}%"),
                None => format!("{name} {spent:.1}%"),
            });
        }
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Слово целиком, а не подстрока: «maintainer» не должен читаться как «main».
fn word(s: &str, w: &str) -> bool {
    s.match_indices(w).any(|(i, _)| {
        let edge = |c: Option<char>| !matches!(c, Some(c) if c.is_alphanumeric() || c == '_');
        edge(s[..i].chars().next_back()) && edge(s[i + w.len()..].chars().next())
    })
}

/// Читается ли ответ агента как «работа закончена, вопросов нет» — признак, а
/// не понимание: настоящую уверенность в этом даёт только человек, а здесь
/// только слова, которыми модель обычно закрывает ход. Само слово «готово»
/// не придумано — это тот же протокол, что уже просит `next_prompt` и
/// `formulate`: «если работа закончена — ответь одной строкой «готово».
///
/// Перекос нарочно в одну сторону: НЕТ уверенности — значит НЕ готово. Вопрос
/// в конце и слова про незакрытое («осталось», «дальше», «не удалось») бьют
/// любое «готово» в том же ответе — в том числе отрицания вроде «не готово»,
/// иначе «работу не закончил» читалось бы как «закончил». Модель формулирует
/// одно и то же десятками способов, и список никогда не будет полным: это
/// сознательная плата за то, что молчаливая ошибка в эту сторону (лишний
/// заход) дешевле молчаливой ошибки в другую (цепочка отдана человеку со
/// словом «сделано» на брошенной на середине работе).
fn reply_reads_as_done(reply: &str) -> bool {
    let s = one_line(reply).to_lowercase();
    if s.is_empty() || s.contains('?') {
        return false;
    }
    let open = [
        "осталось", "дальше", "затем", "потом", "далее", "предстоит",
        "нужно ещё", "надо ещё", "не удалось", "следующим шагом", "todo",
        "не готово", "не готов", "не сделано", "не завершено", "не завершил",
        "не закончено", "не закончил", "ещё не",
    ];
    if open.iter().any(|w| word(&s, w)) {
        return false;
    }
    let done = ["готово", "сделано", "завершено", "завершил", "закончено", "закончил", "done"];
    done.iter().any(|w| word(&s, w))
}

/// Необратимое, которое ночью не делается вовсе, — словами человека.
///
/// Список ЗАКРЫТЫЙ: то, что назвал владелец (пуш в main, слияние веток,
/// удаление, публикация наружу, установка сборки человеку), и то, что проект уже
/// пометил необратимым сам, — забвение транскрипта. Шире не берём: лишний запрет
/// ночью стоит ровно столько же, сколько пропущенная работа, а «удали лишний
/// импорт» откатывается одной командой и необратимым не является.
pub fn irreversible(text: &str) -> Option<&'static str> {
    let s = one_line(text).to_lowercase();
    let any = |pats: &[&str]| pats.iter().any(|p| s.contains(p));
    let main = word(&s, "main") || word(&s, "master") || s.contains(" в мейн");
    if any(&["push --force", "push -f", "force-push", "форс-пуш"])
        || (any(&["git push", "запушь", "запушить", "пуш "]) && main)
    {
        return Some("пуш в main");
    }
    if any(&["git merge", "gh pr merge", "смерджи", "смержи", "вмерджи", "влей ветку", "слияние вет", "слей ветк"])
    {
        return Some("слияние веток");
    }
    if any(&[
        "rm -rf", "rm -r ", "git branch -d", "push --delete", "drop table", "drop database",
        "удали файл", "удалить файл", "удали ветк", "удалить ветк", "удали данные",
        "удали транскрипт", "забудь насовсем", "снеси ",
    ]) {
        return Some("удаление");
    }
    if any(&[
        "npm publish", "cargo publish", "gh release", "опубликуй", "публикация наружу",
        "выложи наружу", "выложи в прод", "задеплой", "деплой", "выкати релиз", "релиз наружу",
    ]) {
        return Some("публикация наружу");
    }
    if any(&["установи сборку", "поставь сборку", "накати сборку", "обнови приложение"]) {
        return Some("установка сборки");
    }
    None
}

/// Отпечаток продвижения: то, чем этот ход отличается от прошлого.
///
/// Продвижение — это изменение МИРА, а мир хода проект уже читает фактами
/// (`turns.rs`): что тронуто и что показала проверка. Берём ровно эти две вещи.
///
/// Почему не одни файлы: агент правит один и тот же файл кругами, список путей
/// при этом не меняется — а красное, ставшее зелёным, это продвижение, и его
/// видно только по вердикту. Почему не одни тесты: их гоняют не в каждом ходе.
/// Почему НЕ текст итога и ответа: модель перефразирует одно и то же бесконечно,
/// и «текст стал другим» — самый ненадёжный признак из возможных; на нём
/// проверка не срабатывала бы никогда. По той же причине от вердикта берём
/// СЧЁТЧИКИ, а не строку целиком (тот же принцип, что у `tests_verdict`):
/// формулировку модель меняет, цифры — нет. Пустой отпечаток (ни файлов, ни
/// проверки) — это тоже ответ: ход не оставил следа.
pub fn progress_mark(o: &Outcome) -> String {
    let mut files: Vec<String> = o
        .files
        .iter()
        .map(|f| format!("{}:{}", f.kind, f.path))
        .collect();
    files.sort();
    files.dedup();
    let tests = match &o.tests {
        Some(t) => {
            let s = t.line.to_lowercase();
            let failed = count_before(&s, " failed").or_else(|| count_before(&s, " failures"));
            let passed = count_before(&s, " passed").or_else(|| count_before(&s, " ok"));
            format!("{}|{failed:?}|{passed:?}", t.ok)
        }
        None => "-".into(),
    };
    format!("{}\u{1}{tests}", files.join(","))
}

/// «Куда он ушёл, пока я спал» — за десять секунд чтения.
///
/// По СТРОКЕ на чат, а не по строке на заход: список из сорока заходов
/// отвечает на этот вопрос ровно так же плохо, как молчание. В строке — сколько
/// заходов, во сколько обошлись, что тронуто и чем кончился последний. Подробный
/// журнал никуда не девается и лежит в шапке чата.
fn where_it_went(visits: &[Visit]) -> Vec<String> {
    let mut order: Vec<&str> = Vec::new();
    for v in visits {
        if !order.contains(&v.chat_id.as_str()) {
            order.push(&v.chat_id);
        }
    }
    order
        .iter()
        .filter_map(|chat| {
            let mine: Vec<&Visit> = visits.iter().filter(|v| v.chat_id == **chat).collect();
            let last = mine.last()?;
            let usd: f64 = mine.iter().filter_map(|v| v.usd).sum();
            let money = match mine.iter().any(|v| v.usd.is_some()) {
                true => format!("{usd:.2}$"),
                false => "расход не посчитан".into(),
            };
            Some(format!(
                "• {chat} — заходов {}, {money}; последний ({}): {} → {}",
                mine.len(),
                last.kind,
                last.decided,
                last.changed
            ))
        })
        .collect()
}

/// Утренняя сводка. Четыре вопроса и ни одним меньше: без ответа на них
/// автономия превращается в «проснулся, а тут что-то произошло». Пятый — куда
/// он ушёл: заходы автономных чатов человек не видел вовсе.
pub fn morning_digest(st: &NightState, spent: Option<&str>, visits: &[Visit]) -> String {
    let pick = |kinds: &[&str]| -> Vec<&Notice> {
        st.notices
            .iter()
            .filter(|n| kinds.contains(&n.kind.as_str()))
            .collect()
    };
    let list = |head: &str, empty: &str, items: Vec<String>| {
        if items.is_empty() {
            format!("{head}: {empty}\n")
        } else {
            format!("{head}:\n{}\n", items.join("\n"))
        }
    };

    // «finished» — тоже сделанная работа, просто её последнее слово не «заход
    // ушёл», а «продолжать нечего»: в утреннем «что сделано» это тот же ответ
    // на тот же вопрос, и второго раздела под него заводить незачем.
    let done = pick(&["sent", "finished"]);
    let stuck = pick(&["failed", "stopped"]);
    // Всё, что не легло в разделы, идёт хвостом. Хвост нужен именно затем, чтобы
    // тишина не съедала сигнал: новый вид карточки не должен пропасть молча.
    let known = ["sent", "failed", "stopped", "finished"];
    let rest = st
        .notices
        .iter()
        .filter(|n| !known.contains(&n.kind.as_str()))
        .collect::<Vec<_>>();

    let mut p = String::from("Ночная сводка Джарвиса — работа шла, показывать было некому.\n\n");
    p.push_str(&list(
        &format!("ЧТО СДЕЛАНО (заходов за ночь: {})", done.len()),
        "заходов не было",
        done.iter().map(|n| format!("• {} — {}", n.chat_id, n.text)).collect(),
    ));
    p.push_str(&format!(
        "СКОЛЬКО ПОТРАЧЕНО: {}\n",
        spent.unwrap_or("бюджет расход за ночь не назвал")
    ));
    p.push_str(&list(
        "КУДА ОН УШЁЛ, ПОКА ТЫ СПАЛ",
        "автономные чаты за ночь никуда не ходили",
        where_it_went(visits),
    ));
    p.push_str(&list(
        "ЧТО ВСТАЛО И ПОЧЕМУ",
        "ничего не вставало",
        stuck.iter().map(|n| format!("• {} — {}", n.chat_id, n.text)).collect(),
    ));
    p.push_str(&list(
        "ЧТО ЖДЁТ ТЕБЯ",
        "решений от тебя не ждёт ничего",
        st.waiting
            .iter()
            .map(|w| {
                format!(
                    "• {} — отложено до утра ({}, сессия {}): {}",
                    w.kind,
                    w.chat_id,
                    ellipsize(&w.session_id, 8),
                    ellipsize(&one_line(&w.prompt), 160)
                )
            })
            .collect(),
    ));
    if !rest.is_empty() {
        p.push_str(&list(
            "ЕЩЁ ЗА НОЧЬ",
            "",
            rest.iter().map(|n| format!("• {} — {}", n.chat_id, n.text)).collect(),
        ));
    }
    p.push_str(&format!(
        "\nНочью не показал уведомлений: {}",
        st.notices.len()
    ));
    if st.dropped > 0 {
        p.push_str(&format!("; ещё {} вытеснено потолком журнала", st.dropped));
    }
    p
}

/// Строка ночного уведомления — ровно то, что человек прочитал бы на карточке.
fn notice_text(kind: &str, extra: &Value) -> String {
    let get = |k: &str| {
        extra
            .get(k)
            .and_then(Value::as_str)
            .map(one_line)
            .unwrap_or_default()
    };
    let sid = get("sessionId");
    let with_sid = |t: String| {
        if sid.is_empty() {
            t
        } else {
            format!("{t} (сессия {})", ellipsize(&sid, 8))
        }
    };
    match kind {
        "sent" => with_sid(format!(
            "заход {}: {}",
            extra.get("step").and_then(Value::as_u64).unwrap_or(0),
            ellipsize(&get("prompt"), 140)
        )),
        "proposed" => with_sid(format!("предложен заход: {}", ellipsize(&get("prompt"), 140))),
        _ => with_sid(get("text")),
    }
}

/// Утро: ночь кончилась — выкатить ОДНУ сводку за всё, что копилось.
///
/// Отдельного будильника у цепочки нет и заводить его тут неправильно: сводку
/// тянут живые точки самой цепочки (пришло «закончил», человек написал агенту) —
/// к утру хоть одна случается первой же командой человека. Журнал осушается ДО
/// эмита, поэтому второй заход сюда сводку не повторит.
pub fn morning_check(app: &AppHandle) {
    // Дешёвое первым: пустой журнал — обычное дело, и спрашивать бюджет про
    // ночь (а это чтение настроек) на каждое событие агента незачем.
    if night_log().is_empty() || is_night(app) {
        return;
    }
    let Some(st) = night_log().drain() else { return };
    // Ночь только что кончилась — окно указывает на неё, и заходы берутся ровно
    // за ту ночь, про которую сводка.
    let text = morning_digest(
        &st,
        night_spent(app).as_deref(),
        &visits().night_visits(&window(app)),
    );
    // Якорь — чат последнего ночного уведомления: сводка одна на всю ночь, а
    // лечь она обязана туда, где ночью шла работа. Остальные чаты названы внутри.
    let Some(chat_id) = st
        .notices
        .last()
        .map(|n| n.chat_id.clone())
        .or_else(|| st.waiting.last().map(|w| w.chat_id.clone()))
    else {
        return;
    };
    crate::log::line(&format!(
        "[chain] утренняя сводка: {} уведомлений, ждёт решения {}",
        st.notices.len(),
        st.waiting.len()
    ));
    emit(
        app,
        &chat_id,
        "morning",
        json!({ "digest": text, "night": st }),
    );
    // Единственное место, где цепочка будит человека, — и оно наступает утром.
    if let Some(d) = tauri::Manager::try_state::<Arc<Daemon>>(app) {
        d.notify(
            "Ночная сводка",
            &format!(
                "за ночь {} событий, ждёт решения {}",
                st.notices.len(),
                st.waiting.len()
            ),
            None,
            "done",
        );
    }
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

/// Срез цепочки для шапки (режим — из настроек, остальное — из реестра, а
/// «ждёт тебя» и журнал заходов — из своих журналов: оба переживают саму цепочку).
pub fn state(app: &AppHandle, chat_id: &str) -> ChainState {
    let w = window(app);
    chains()
        .state(chat_id, mode_of(app, chat_id))
        .with_waiting(night_log().for_chat(chat_id))
        .with_log(visits().for_chat(chat_id), visits().spend(chat_id, &w))
}

/// Событие цепочки наружу. Канал свой (`agent:chain`), но правило то же, что у
/// `agent:event`: метка чата обязательна — без неё карточка легла бы в чужой
/// разговор, стоит человеку уйти в соседний проект.
fn emit(app: &AppHandle, chat_id: &str, kind: &str, extra: Value) {
    // Тишина. Ночью карточка в чат ложится (работа идёт и должна быть видна), но
    // будить звуком и всплывашкой некого: уведомление копится до утра. Копим
    // только то, что человеку адресовано, — служебные срезы шапки в сводке лишние.
    let night = is_night(app);
    if night && ["sent", "proposed", "failed", "stopped", "deferred", "finished"].contains(&kind) {
        night_log().hush(chat_id, kind, &notice_text(kind, &extra));
    }
    let mut payload = json!({
        "chatId": chat_id,
        "kind": kind,
        "at": now_ms(),
        "quiet": night,
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
pub fn fail_text(text: &str, stopped: bool, night: bool) -> String {
    let text = one_line(text);
    match (stopped, night) {
        (false, _) => text,
        (true, true) => format!(
            "{text}. Ночью цепочку рвёт первая же неудача — посмотреть и поправить некому, \
             а вторая попытка вслепую стоит столько же"
        ),
        (true, false) => {
            format!("{text}. Это вторая неудача подряд по одной причине — цепочку останавливаю")
        }
    }
}

/// Отказ наружу словами — единая точка, чтобы «тихих» веток не заводилось.
fn refuse(app: &AppHandle, chat_id: &str, code: &str, text: &str) {
    crate::log::line(&format!("[chain] {chat_id}: {code} — {text}"));
    let night = is_night(app);
    let stopped = chains().on_fail(chat_id, code, night) == FailAction::Stop;
    let text = fail_text(text, stopped, night);
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
    // Человек написал агенту — значит он проснулся: самое время отдать ночную
    // сводку, если она копилась.
    morning_check(app);
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
    morning_check(&d.app);
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
    let night = is_night(&app);
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
            // Исчерпание — ДРУГАЯ проверка, чем Stuck ниже, и путать их нельзя.
            // Stuck: несколько заходов подряд топчутся на одном месте — это
            // карусель, сбой. Исчерпание: ОДИН ход сам расписался законченным —
            // файлов не тронул, команд не запускал, вердикт проверки не новый,
            // ответ читается как «готово». Это успех.
            //
            // Десять — потолок, а не план. Цепочка обязана уметь закончиться на
            // третьем заходе словами «сделано, продолжать нечего», иначе она
            // добивает клетки выдуманной работой — что и наблюдалось.
            if chains().exhausted(chat_id, &outcome) {
                let text = format!(
                    "Сделано, продолжать нечего: заход не тронул файлов, не запустил команд, \
                     вердикт проверки не новый ({}), а ответ агента читается как «готово»",
                    outcome.tests.as_ref().map(|t| t.line.as_str()).unwrap_or("не гонялся")
                );
                note_visit(d, chat_id, sid, step, "finished", &outcome.reply, &outcome, night);
                chains().stop(chat_id);
                emit(&app, chat_id, "finished", json!({ "reason": "exhausted", "text": text }));
                return;
            }
            // Зацикливание — отдельная проверка и только для авто-режима: в
            // ручном каждый заход и так проходит через глаза человека.
            if let Progress::Stuck(n) = chains().note_progress(chat_id, &progress_mark(&outcome), night)
            {
                emit(
                    &app,
                    chat_id,
                    "stopped",
                    json!({
                        "reason": "stuck",
                        "text": format!(
                            "{n} захода подряд ничего не изменили — ни файлов, ни вердикта проверки. \
                             Это карусель, а не работа; дальше нужен твой взгляд"
                        ),
                    }),
                );
                return;
            }
            // Свой потолок чата — ДО общего бюджета: он про деньги именно этого
            // разговора, считается на месте и останавливает раньше, чем ступень
            // провайдера успеет заметить трёх автономных сразу.
            match visits().cap_hit(chat_id, night, &window(&app)) {
                // Ночью потолок рвёт цепочку: посмотреть и решить некому.
                Some(hit) if hit.action == CapAction::Stop => {
                    note_visit(d, chat_id, sid, step, "stopped", &hit.text, &outcome, night);
                    chains().stop(chat_id);
                    emit(&app, chat_id, "stopped", json!({ "reason": "cap", "text": hit.text }));
                    return;
                }
                // Днём — предупреждает и пропускает: человек рядом, расход у
                // него на глазах, и решение остаётся за ним.
                Some(hit) => warn_cap(&app, chat_id, &hit),
                None => {}
            }
            // Перед заходом спрашиваем бюджет свежими числами: цепочка — это
            // фон, и её очередь наступает раньше человеческой. Ночной потолок
            // сидит в той же ступени и скажет «стоп» раньше дневной нормы.
            // Чей расход — знает сессия: шкалы провайдеров не складываются, а
            // безымянного судим по claude, самому дорогому.
            let agent = d
                .session(sid)
                .and_then(|s| s.agent.clone())
                .unwrap_or_else(|| crate::budget::CLAUDE.to_string());
            if let Err(text) = crate::ipc::budget_gate(d, &agent, true, "заход авто-цепочки").await {
                refuse(&app, chat_id, "budget", &text);
                return;
            }
            let prompt = formulate(&outcome, step).await;
            // Необратимое ночью не делается ВОВСЕ. «Спросить некого» — не то же
            // самое, что «можно»: заход целиком ложится в «ждёт тебя».
            if night {
                if let Some(kind) = irreversible(&prompt) {
                    note_visit(d, chat_id, sid, step, "deferred", &prompt, &outcome, night);
                    defer(&app, chat_id, sid, kind, &prompt);
                    return;
                }
            }
            // Заход ушёл — значит он и есть строка журнала: что решил, что
            // запустил, что изменилось. Отказ уже сказан словами, и следа не
            // оставляет: заход, которого не было, в журнале заходов лишний.
            if deliver(d, chat_id, sid, &prompt, step).await.is_ok() {
                note_visit(d, chat_id, sid, step, "sent", &prompt, &outcome, night);
                // Карточка «заход ушёл» легла ДО записи в журнал — досылаем срез,
                // иначе шапка показывает журнал без только что сделанного захода.
                push_state(&app, chat_id);
            }
        }
        Decision::Skip | Decision::Depth => {}
    }
}

/// Что изменилось в мире прошлым ходом — одной строкой: файлы и вердикт
/// проверки. Ровно то, по чему цепочка судит о продвижении (`progress_mark`), и
/// ровно то, что человек утром хочет прочитать про чужую ночную работу.
fn changed_text(o: &Outcome) -> String {
    let files: Vec<&str> = o.files.iter().take(6).map(|f| f.path.as_str()).collect();
    let head = match files.is_empty() {
        true => "файлов не тронул".to_string(),
        false => format!("тронул {}", files.join(", ")),
    };
    match &o.tests {
        Some(t) if t.ok => format!("{head}; тесты зелёные"),
        Some(_) => format!("{head}; тесты КРАСНЫЕ"),
        None => format!("{head}; проверку не гонял"),
    }
}

/// Записать заход в журнал. Расход спрашиваем у `usage` — единственного, кто
/// считает токены; своих чисел у цепочки нет, и придумывать их она не станет.
fn note_visit(
    d: &Arc<Daemon>,
    chat_id: &str,
    sid: &str,
    step: u32,
    kind: &str,
    decided: &str,
    o: &Outcome,
    night: bool,
) {
    let total = d
        .usage
        .for_session(sid)
        .and_then(|v| v.get("cost").and_then(Value::as_f64));
    visits().note(Visit {
        at: now_ms(),
        chat_id: chat_id.to_string(),
        session_id: sid.to_string(),
        step,
        kind: kind.to_string(),
        decided: ellipsize(&one_line(decided), 160),
        ran: o
            .commands
            .iter()
            .take(5)
            .map(|c| ellipsize(&one_line(c), 80))
            .collect(),
        changed: changed_text(o),
        night,
        usd: visits().delta(sid, total),
    });
}

/// Сказать про потолок, который днём не рвёт цепочку. ОДИН раз за цепочку:
/// повтор на каждом заходе — шум, а не предупреждение; а новая цепочка — это
/// новое решение человека продолжать, и его предупреждают снова.
fn warn_cap(app: &AppHandle, chat_id: &str, hit: &CapHit) {
    if !chains().note_cap_warning(chat_id) {
        return;
    }
    crate::log::line(&format!("[chain] {chat_id}: cap — {}", hit.text));
    emit(app, chat_id, "cap", json!({ "reason": "cap", "text": hit.text }));
}

/// Отложить необратимое до утра. Цепочка на этом встаёт: следующий её шаг — то
/// самое действие, и обойти его, продолжая, нельзя.
fn defer(app: &AppHandle, chat_id: &str, sid: &str, kind: &str, prompt: &str) {
    let w = Waiting {
        chat_id: chat_id.to_string(),
        session_id: sid.to_string(),
        at: now_ms(),
        kind: kind.to_string(),
        prompt: prompt.to_string(),
    };
    night_log().defer(w.clone());
    chains().stop(chat_id);
    crate::log::line(&format!("[chain] {chat_id}: ночью не делаю необратимое ({kind}) — отложено"));
    emit(
        app,
        chat_id,
        "deferred",
        json!({
            "sessionId": sid,
            "reason": "irreversible",
            "text": format!(
                "Ночью необратимое не делаю: {kind}. Заход готов и ждёт тебя утром"
            ),
            "waiting": w,
        }),
    );
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
    // Происхождение едет вместе с промптом и ставится транспортом (`origin.rs`).
    // Инцидент: заход цепочки ушёл в торговую сессию с боевыми ключами биржи, а
    // Джарвис, ведущий тот чат, объявил его подделкой и заподозрил постороннего —
    // отличить сочинённое машиной от переданного по просьбе человека было нечем.
    let out = crate::ipc::via_gate_panel(
        d,
        "sessions.reply",
        json!({
            "session_id": sid,
            "text": prompt,
            "_origin": "chain",
            "_step": step,
            "_of": MAX_STEPS,
        }),
    )
    .await;
    if out.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        chains().mark_sent(chat_id, step);
        // Заход ушёл — значит утреннее «ждёт тебя» по этому чату отработано:
        // висящая карточка после решения человека хуже, чем её отсутствие.
        night_log().resolve(chat_id);
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

/// Человек написал в сессию — цепочки на ней уступают и ждут его решения.
///
/// Зовётся из транспорта (`capability/native/control.rs`), где уже известно
/// происхождение промпта: пауза ставится по промпту ЧЕЛОВЕКА (в том числе
/// переданному Джарвисом) и никогда по заходу самой цепочки — иначе она
/// останавливала бы себя же.
///
/// Карточка обязательна. Молчаливая пауза — это цепочка, которая «почему-то
/// больше не идёт»: человек либо решит, что она сломалась, либо не заметит, что
/// работа встала.
pub fn pause_for_human(d: &Arc<Daemon>, session_id: &str) {
    for chat_id in chains().pause_for_human(session_id) {
        emit(
            &d.app,
            &chat_id,
            "paused",
            json!({
                "sessionId": session_id,
                "text": "Цепочка приостановлена: в эту сессию пришла задача от человека. \
                         Её заход подождёт — продолжить цепочку или отменить?",
            }),
        );
    }
}

/// Сессия исчезла (session-end, убитый терминал, снятая за изоляцию пана):
/// цепочке ждать больше нечего, и молчать об этом нельзя.
pub fn on_session_gone(d: &Arc<Daemon>, session_id: &str, why: &str) {
    for chat_id in chains().chats_of(session_id) {
        chains().stop(&chat_id);
        // Отложенный заход адресован именно этой сессии — её больше нет, и
        // «ждёт тебя» про неё утром только запутает.
        night_log().resolve(&chat_id);
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
    /// Промпт человека главнее захода цепочки.
    ///
    /// Живой случай: Джарвис поставил сессии исследование по просьбе человека,
    /// оно встало в очередь, следом пришёл «Заход 2 из 10», и сессия ушла
    /// разбирать другую тему. У сессии не было владельца — писали двое, не зная
    /// друг о друге.
    #[test]
    fn a_human_task_takes_the_session_and_the_chain_steps_aside() {
        let c = chain(Mode::Auto);
        c.watch("c1", "s1", Mode::Auto);
        assert_eq!(c.on_done("s1", 100), vec![("c1".into(), Decision::Send(1))]);

        // Человек написал в ту же сессию.
        assert_eq!(c.pause_for_human("s1"), vec!["c1".to_string()]);
        assert_eq!(c.state("c1", Mode::Auto).phase, Phase::Paused);

        // «Закончил» теперь относится к ЕГО задаче — заход по нему не строим.
        assert_eq!(c.on_done("s1", 200), vec![("c1".into(), Decision::Skip)]);

        // Вторая реплика человека второй карточки не рождает.
        assert!(c.pause_for_human("s1").is_empty(), "паузу поставили дважды");

        // Продолжили — цепочка снова идёт, и шаг не съеден паузой.
        assert!(c.resume("c1"));
        assert_eq!(c.on_done("s1", 200), vec![("c1".into(), Decision::Send(2))],
            "после паузы цепочка потеряла ход");
    }

    /// Пауза — не стоп: цепочку человек заводил осознанно, и ронять её из-за
    /// одной своей реплики значило бы потерять работу другим способом.
    #[test]
    fn pausing_is_not_stopping_and_resuming_a_dead_chain_is_honest_about_it() {
        let c = chain(Mode::Auto);
        c.watch("c1", "s1", Mode::Auto);
        c.pause_for_human("s1");
        assert!(c.state("c1", Mode::Auto).active, "пауза убила цепочку");
        assert!(c.resume("c1"));
        assert!(!c.resume("c1"), "продолжили то, что и так шло");

        c.stop("c1");
        assert!(!c.resume("c1"), "воскресили оборванную цепочку");
    }

    /// Соседний чат на той же сессии тоже уступает: сессия одна, и владелец у
    /// неё в каждый момент один.
    #[test]
    fn every_chain_on_that_session_steps_aside_not_just_one() {
        let c = chain(Mode::Auto);
        c.watch("c1", "s1", Mode::Auto);
        c.watch("c2", "s1", Mode::Auto);
        c.watch("c3", "s2", Mode::Auto);
        assert_eq!(c.pause_for_human("s1"), vec!["c1".to_string(), "c2".to_string()]);
        assert_eq!(c.state("c3", Mode::Auto).phase, Phase::Watching, "уступил чужой чат");
    }

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
        assert_eq!(c.on_fail("c1", "send", false), FailAction::Note);
        assert_eq!(c.on_fail("c1", "send", false), FailAction::Stop);
        assert!(!c.state("c1", Mode::Auto).active, "серия по одной причине — стоп");

        // разные причины подряд серией не считаются
        let c = chain(Mode::Auto);
        assert_eq!(c.on_fail("c1", "send", false), FailAction::Note);
        assert_eq!(c.on_fail("c1", "stop-failure:rate_limit", false), FailAction::Note);
        assert_eq!(c.on_fail("c1", "send", false), FailAction::Note);
        assert!(c.state("c1", Mode::Auto).active);

        // удачный заход обнуляет серию
        let c = chain(Mode::Auto);
        assert_eq!(c.on_fail("c1", "send", false), FailAction::Note);
        c.mark_sent("c1", 1);
        assert_eq!(c.on_fail("c1", "send", false), FailAction::Note, "серия сброшена успехом");
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
            let t = fail_text(reason, false, false);
            assert!(t.contains(reason), "причину переписали: {t}");
            assert!(!t.contains("останавливаю"), "одна неудача цепочку не рвёт: {t}");
        }
        // вторая подряд по той же причине — стоп, и об этом тоже словами
        assert_eq!(c.on_fail("c1", "send", false), FailAction::Note);
        let stopped = c.on_fail("c1", "send", false) == FailAction::Stop;
        let t = fail_text("tmux: пана мертва", stopped, false);
        assert!(t.starts_with("tmux: пана мертва"), "{t}");
        assert!(t.contains("вторая неудача подряд"), "человек должен понять причину стопа: {t}");
        // перевод строки в чужой ошибке не должен ломать карточку
        assert_eq!(fail_text("сбой\nвторая строка", false, false), "сбой вторая строка");
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

    // ── ночь ──────────────────────────────────────────────────────────────

    /// Главное правило ночи: смотреть некому, поэтому вторую попытку вслепую не
    /// делаем — она стоит ровно столько же, сколько первая.
    #[test]
    fn at_night_the_first_failure_breaks_the_chain() {
        let c = chain(Mode::Auto);
        assert_eq!(c.on_fail("c1", "send", true), FailAction::Stop, "ночью хватает первой");
        assert!(!c.state("c1", Mode::Auto).active);

        // днём та же самая неудача цепочку не рвёт — рвёт вторая
        let c = chain(Mode::Auto);
        assert_eq!(c.on_fail("c1", "send", false), FailAction::Note);
        assert!(c.state("c1", Mode::Auto).active, "днём человек рядом, одна неудача не повод");
        assert_eq!(c.on_fail("c1", "send", false), FailAction::Stop);

        // причина стопа доходит словами и объясняет, почему счёт другой
        let t = fail_text("tmux: пана мертва", true, true);
        assert!(t.starts_with("tmux: пана мертва"), "причину переписали: {t}");
        assert!(t.contains("первая же неудача"), "человек должен понять ночной счёт: {t}");
    }

    /// Необратимое ночью не делается вовсе: заход не уходит, а ложится в «ждёт
    /// тебя» целиком — утром человек отправляет его, ничего не переписывая.
    #[test]
    fn irreversible_at_night_waits_for_the_morning() {
        for (text, kind) in [
            ("прогони тесты и запушь в main", "пуш в main"),
            ("git push --force origin dev", "пуш в main"),
            ("смерджи ветку в основную", "слияние веток"),
            ("gh pr merge 42", "слияние веток"),
            ("подчисти: rm -rf /tmp/build", "удаление"),
            ("удали ветку feature/x после проверки", "удаление"),
            ("cargo publish и обнови теги", "публикация наружу"),
            ("задеплой на прод", "публикация наружу"),
            ("установи сборку человеку", "установка сборки"),
        ] {
            assert_eq!(irreversible(text), Some(kind), "не поймали необратимое: {text}");
        }
        // обратимое остаётся работой, а не поводом ждать до утра
        for text in [
            "почини красные тесты и прогони проверку",
            "удали лишний импорт в src/a.rs",
            "поправь maintainer в Cargo.toml",
            "закоммить и открой PR",
        ] {
            assert_eq!(irreversible(text), None, "лишний запрет ночью: {text}");
        }

        // отложенное — структура, а не строка в логе: у него есть чат, сессия,
        // причина и сам заход
        let log = NightLog::new();
        let w = Waiting {
            chat_id: "c1".into(),
            session_id: "sess-1234567890".into(),
            at: 1,
            kind: "пуш в main".into(),
            prompt: "прогони тесты и запушь в main".into(),
        };
        log.defer(w.clone());
        log.defer(w.clone()); // повтор того же захода второй карточки не плодит
        assert_eq!(log.for_chat("c1"), vec![w.clone()]);
        assert!(log.for_chat("c2").is_empty(), "чужому чату отложенное не показываем");
        assert_eq!(log.resolve("c1"), 1, "человек разобрался — карточка снимается");
        assert!(log.for_chat("c1").is_empty());

        // и всё это действительно вшито в путь отправки: ночью проверка идёт ДО
        // deliver, иначе необратимое проскочит по принципу «спросить некого»
        let src = include_str!("chain.rs");
        let body = src
            .split("Decision::Send(step) => {")
            .nth(1)
            .and_then(|t| t.split("deliver(").next())
            .expect("ветка отправки на месте");
        assert!(body.contains("if night"), "ночь в ветке отправки не проверяется");
        assert!(body.contains("irreversible(&prompt)"), "заход не проверен на необратимое");
        assert!(body.contains("defer("), "необратимое не откладывается");
    }

    /// Зацикливание: цепочка, где каждый заход порождает следующий без
    /// продвижения, — самый дорогой способ потратить ночь.
    #[test]
    fn a_chain_that_moves_nowhere_is_stopped() {
        let red = |line: &str, files: &[&str]| {
            let f = TurnFacts {
                files: files
                    .iter()
                    .map(|p| FileTouch { path: (*p).into(), kind: "edited".into() })
                    .collect(),
                final_reply: line.into(),
                ..Default::default()
            };
            progress_mark(&build_outcome("s1", "jarvis", "k", "работаю", Some(&f)))
        };
        let same = red("1 failed в tests::a", &["src/a.rs"]);

        // ночью: два захода подряд без продвижения — стоп
        let c = chain(Mode::Auto);
        assert_eq!(c.note_progress("c1", &same, true), Progress::Moved, "первый сравнивать не с чем");
        assert_eq!(c.note_progress("c1", &same, true), Progress::Stale(1));
        assert_eq!(c.note_progress("c1", &same, true), Progress::Stuck(2));
        assert!(!c.state("c1", Mode::Auto).active, "карусель обязана встать сама");

        // днём запас на заход больше — человек рядом и увидит
        let c = chain(Mode::Auto);
        assert_eq!(c.note_progress("c1", &same, false), Progress::Moved);
        assert_eq!(c.note_progress("c1", &same, false), Progress::Stale(1));
        assert_eq!(c.note_progress("c1", &same, false), Progress::Stale(2));
        assert_eq!(c.note_progress("c1", &same, false), Progress::Stuck(3));

        // продвижение обнуляет счёт: красное стало зелёным — файлы те же, но мир
        // изменился, и это ровно то, ради чего вердикт входит в отпечаток
        let c = chain(Mode::Auto);
        c.note_progress("c1", &same, true);
        c.note_progress("c1", &same, true);
        let green = red("test result: ok. 40 passed", &["src/a.rs"]);
        assert_eq!(c.note_progress("c1", &green, true), Progress::Moved);
        assert_eq!(c.note_progress("c1", &green, true), Progress::Stale(1), "счёт начат заново");

        // тронули другой файл — тоже продвижение
        assert_ne!(same, red("1 failed в tests::a", &["src/a.rs", "src/b.rs"]));
        // и красных стало меньше — продвижение
        assert_ne!(same, red("2 failed в tests::a", &["src/a.rs"]));
        // а перефразированный ответ при том же счёте — НЕ продвижение: текст
        // модели меняется сам по себе и признаком служить не может
        assert_eq!(same, red("1 failed в tests::a — сейчас поправлю", &["src/a.rs"]));
        // ход без единого следа тоже считается топтанием
        assert_eq!(progress_mark(&build_outcome("s1", "j", "k", "думал", None)), "\u{1}-");
    }

    // ── исчерпание: «сделано, продолжать нечего» ────────────────────────────
    //
    // Это ДРУГОЙ вопрос, чем в тесте выше. Там — несколько заходов ПОДРЯД без
    // единого следа (карусель, сбой, счётчик `stale`). Здесь — ОДИН ход, о
    // котором сам агент сказал «готово», и в мире с прошлого раза ничего не
    // изменилось: это успех, а не сбой, и оба исхода обязаны остаться разными.

    /// Ровно тот заход из живого примера человека: файлов нет, команд нет,
    /// тесты зелёные и те же, что были, ответ — «готово». Исчерпано.
    #[test]
    fn nothing_new_and_a_plain_done_is_exhausted() {
        let facts = TurnFacts { final_reply: "test result: ok. 12 passed".into(), ..Default::default() };
        let mut o = build_outcome("s1", "jarvis", "k", "прогнал проверку", Some(&facts));
        o.reply = "Готово.".into();

        let c = chain(Mode::Auto);
        // Первый заход сравнивать не с чем — не хватает уверенности, что мир
        // не изменился, а значит не исчерпано.
        assert!(!c.exhausted("c1", &o), "первый ход исчерпанным не бывает — не с чем сравнить");
        c.note_progress("c1", &progress_mark(&o), false);
        // Второй такой же ход — вердикт не новый, файлов и команд нет, ответ
        // читается как «готово».
        assert!(c.exhausted("c1", &o), "живой пример человека обязан читаться как исчерпание");
    }

    /// Незакрытый вопрос или заявка на продолжение перевешивают любое «готово»
    /// в том же ответе — ложная остановка стоит дороже, чем лишний заход.
    #[test]
    fn an_open_question_or_a_promise_to_continue_is_not_exhausted() {
        let facts = TurnFacts { final_reply: "test result: ok. 12 passed".into(), ..Default::default() };
        let mut o = build_outcome("s1", "jarvis", "k", "прогнал проверку", Some(&facts));
        let c = chain(Mode::Auto);
        c.note_progress("c1", &progress_mark(&o), false);

        for reply in [
            "Готово. Продолжать дальше?",
            "Готово, но осталось поправить доку",
            "Дальше нужно посмотреть на кеш",
            "Не удалось починить последний тест",
            "Работу не закончил — переключился на другое",
        ] {
            o.reply = reply.into();
            assert!(!c.exhausted("c1", &o), "незакрытый вопрос принят за исчерпание: {reply}");
        }
    }

    /// Тронутые файлы — уже сама по себе работа, даже если слова звучат как
    /// «готово»: исчерпание не читает намерение агента раньше фактов хода.
    #[test]
    fn touched_files_are_never_exhausted_no_matter_the_words() {
        let f = TurnFacts {
            files: vec![FileTouch { path: "src/a.rs".into(), kind: "edited".into() }],
            final_reply: "test result: ok. 12 passed".into(),
            ..Default::default()
        };
        let mut o = build_outcome("s1", "jarvis", "k", "починил", Some(&f));
        o.reply = "Готово.".into();
        let c = chain(Mode::Auto);
        c.note_progress("c1", &progress_mark(&o), false);
        // Тот же список файлов во втором ходу — Stuck-детектор сказал бы
        // «топчемся», а не исчерпание: файлы делают ход НЕ исчерпанным сами
        // по себе, ещё до всякого сравнения с прошлым.
        assert!(!c.exhausted("c1", &o), "тронутые файлы прочитаны как пустая работа");
    }

    /// Красные тесты не бывают исчерпанием ни при каких словах — самое дорогое
    /// место для ложной остановки: работа не просто не закончена, она сломана.
    #[test]
    fn red_tests_are_never_exhausted_no_matter_the_words() {
        let f = TurnFacts {
            final_reply: "test result: FAILED. 9 passed; 1 failed".into(),
            ..Default::default()
        };
        let mut o = build_outcome("s1", "jarvis", "k", "чинил", Some(&f));
        o.reply = "Готово, всё сделано.".into();
        let c = chain(Mode::Auto);
        c.note_progress("c1", &progress_mark(&o), false);
        assert!(!c.exhausted("c1", &o), "красные тесты прочитаны как исчерпание");
    }

    /// Исчерпание и Stuck — разные исходы одного и того же «ничего не тронул»:
    /// повторный ход без единого следа и без слова «готово» обязан оставаться
    /// каруселью (сбоем со счётчиком `stale`), а не тихо переобуться в
    /// «сделано»: разводит их именно ответ агента — здесь нет ни файлов, ни
    /// команд, ни красных тестов, но и явного «готово» тоже нет, и Stuck ловит
    /// ровно такой, безмолвный застой, для которого exhausted не даёт добро.
    #[test]
    fn exhaustion_and_stuck_disagree_on_the_same_silent_repeat() {
        let f = TurnFacts { final_reply: "сейчас поправлю".into(), ..Default::default() };
        let o = build_outcome("s1", "jarvis", "k", "работаю", Some(&f));
        let c = chain(Mode::Auto);
        c.note_progress("c1", &progress_mark(&o), false);
        assert!(!c.exhausted("c1", &o), "молчаливый застой без «готово» — не «сделано»");
        assert_eq!(
            c.note_progress("c1", &progress_mark(&o), false),
            Progress::Stale(1),
            "тот же самый ход — это Stuck-счётчик, а не исчерпание"
        );
    }

    #[test]
    fn reply_reads_as_done_is_a_pattern_match_not_understanding() {
        assert!(reply_reads_as_done("готово"));
        assert!(reply_reads_as_done("Сделано, можно закрывать"));
        assert!(!reply_reads_as_done(""), "пустой ответ — не заявка на завершение");
        assert!(!reply_reads_as_done("Готово?"), "вопрос перевешивает готово");
        assert!(!reply_reads_as_done("Готово, осталось поправить README"));
        assert!(!reply_reads_as_done("работу не закончил"), "отрицание не должно читаться как завершение");
        assert!(!reply_reads_as_done("почитал код, разбираюсь"), "нет явного маркера — нет уверенности");
    }

    /// Тишина не съедает сигнал: ночью уведомления копятся, а утренняя сводка
    /// перечисляет их все и отвечает на четыре вопроса.
    #[test]
    fn night_notices_pile_up_and_the_morning_digest_answers_four_questions() {
        let log = NightLog::new();
        assert!(log.drain().is_none(), "ночи не было — сводке взяться неоткуда");

        log.hush("c1", "sent", "заход 1: почини красные тесты (сессия sess-123)");
        log.hush("c1", "sent", "заход 2: прогони проверку (сессия sess-123)");
        log.hush("c1", "failed", "Заход не ушёл: сессия не приняла заход");
        log.hush("c2", "stopped", "tmux: пана мертва. Ночью цепочку рвёт первая же неудача");
        log.hush("c1", "deferred", "Ночью необратимое не делаю: пуш в main");
        log.defer(Waiting {
            chat_id: "c1".into(),
            session_id: "sess-1234567890".into(),
            at: 1,
            kind: "пуш в main".into(),
            prompt: "прогони тесты и запушь в main".into(),
        });

        assert!(!log.is_empty(), "журнал знает, что копилось");
        let st = log.drain().expect("за ночь накопилось");
        assert!(log.is_empty(), "осушенный журнал пуст");
        assert_eq!(st.notices.len(), 5, "ни одно уведомление не потеряно");
        assert_eq!(st.waiting.len(), 1);
        assert!(log.drain().is_none(), "сводка одна: второй раз то же не выкатываем");
        assert_eq!(
            log.for_chat("c1").len(),
            1,
            "«ждёт тебя» переживает сводку — его снимает решение человека, а не рассказ о нём"
        );

        let d = morning_digest(&st, Some("за ночь 1.20$ из дневного бюджета"), &[]);
        for head in ["ЧТО СДЕЛАНО", "СКОЛЬКО ПОТРАЧЕНО", "ЧТО ВСТАЛО И ПОЧЕМУ", "ЧТО ЖДЁТ ТЕБЯ"] {
            assert!(d.contains(head), "в сводке нет ответа на «{head}»:\n{d}");
        }
        for n in &st.notices {
            assert!(d.contains(&n.text), "уведомление потеряно в сводке: {}\n{d}", n.text);
        }
        assert!(d.contains("заходов за ночь: 2"), "что сделано — числом:\n{d}");
        assert!(d.contains("1.20$"), "расход из бюджета:\n{d}");
        assert!(d.contains("пуш в main"), "отложенное обязано попасть в сводку:\n{d}");
        assert!(d.contains("запушь в main"), "заход виден целиком — утром его отправлять:\n{d}");

        // бюджет промолчал — сводка всё равно отвечает на все четыре вопроса
        let d = morning_digest(&NightState::default(), None, &[]);
        for head in ["ЧТО СДЕЛАНО", "СКОЛЬКО ПОТРАЧЕНО", "ЧТО ВСТАЛО И ПОЧЕМУ", "ЧТО ЖДЁТ ТЕБЯ"] {
            assert!(d.contains(head), "пустая ночь не повод молчать про «{head}»:\n{d}");
        }
        assert!(d.contains("бюджет расход за ночь не назвал"), "молчание бюджета — тоже ответ:\n{d}");

        // потолок журнала не теряет сигнал молча
        let log = NightLog::new();
        for i in 0..MAX_NOTICES + 3 {
            log.hush("c1", "sent", &format!("заход {i}"));
        }
        let st = log.drain().expect("накопилось");
        assert_eq!((st.notices.len(), st.dropped), (MAX_NOTICES, 3));
        assert!(morning_digest(&st, None, &[]).contains("вытеснено потолком журнала"));
    }

    /// Ночная тишина устроена ровно так: копим и метим `quiet`, но карточку в
    /// чат всё равно кладём — работа шла, и она обязана быть видна.
    #[test]
    fn night_is_asked_of_the_budget_and_not_invented_here() {
        // Два определения ночи в двух местах — это система, которая ночью ведёт
        // себя по-разному. Предикат один, и живёт он в бюджете.
        let src = include_str!("chain.rs");
        assert!(src.contains("crate::budget::is_night(&d)"),
            "цепочка снова считает ночь сама");
        let todo = format!("TO{}(budget)", "DO"); // не литералом: сторож ловил сам себя
        assert!(!src.contains(&todo), "переходник к бюджету так и не сведён");
        // Часов в коде цепочки быть не должно — границы ночи это настройка.
        // Литералы собираем из кусков: иначе сторож споткнётся о собственный
        // список (первая версия этого теста так и упала).
        let colon = ":";
        for h in ["23", "22", "07", "06"] {
            let lit = format!("{h}{colon}00");
            assert!(!src.contains(&lit), "в цепочке зашит час ночи: {lit}");
        }
    }

    /// Цепочка — фон, и перед каждым заходом она спрашивает бюджет свежими
    /// числами: на ступени `queue` фоновый заход не уходит вовсе. Отказ идёт
    /// через ту же `refuse` — значит доходит словами и считается неудачей
    /// (ночью первой же и рвёт цепочку).
    #[test]
    fn a_background_pass_asks_the_budget_before_it_goes() {
        let src = include_str!("chain.rs");
        let body = src
            .split("Decision::Send(step) => {")
            .nth(1)
            .and_then(|t| t.split("deliver(").next())
            .expect("ветка отправки на месте");
        assert!(body.contains("budget_gate("), "заход уходит, не спросив бюджет");
        assert!(body.contains("budget_gate(d, &agent, true,"), "заход не назвался фоном: {body}");
        assert!(
            body.find("budget_gate(").unwrap() < body.find("formulate(").unwrap(),
            "бюджет спрашивают после формулировки — служебный ход уже сожжён"
        );
        assert!(body.contains("refuse(&app, chat_id, \"budget\""), "отказ бюджета молчит");
    }

    #[test]
    fn the_night_is_quiet_but_not_blind() {
        let src = include_str!("chain.rs");
        let body = src
            .split("fn emit(app: &AppHandle")
            .nth(1)
            .and_then(|t| t.split("app.emit(").next())
            .expect("эмит на месте");
        assert!(body.contains("night_log().hush("), "ночью уведомление не копится");
        assert!(body.contains("\"quiet\": night"), "окну не сказано, что будить некого");
        assert!(body.contains("is_night(app)"), "ночь в эмите не спрашивается");

        // текст карточки, а не пересказ: утром восстановить его будет неоткуда
        let t = notice_text("sent", &json!({ "step": 3, "prompt": "почини\nтесты", "sessionId": "sess-1234567890" }));
        assert!(t.contains("заход 3") && t.contains("почини тесты"), "{t}");
        assert!(t.contains("sess-123"), "у уведомления должна быть сессия: {t}");
        assert_eq!(notice_text("stopped", &json!({ "text": "цепочка встала" })), "цепочка встала");
    }

    // ── точечная автономия: свой режим, свой расход, свои потолки ──────────

    /// Владелец включает «сам» ТОЧЕЧНО: сегодня один чат, завтра другой. Две
    /// цепочки на одной сессии — и режим у каждой свой; иначе переключатель в
    /// шапке заражал бы соседний разговор, который об этом не просил.
    #[test]
    fn the_switch_moves_only_its_own_chat() {
        let c = Chains::new();
        c.watch("c1", "s1", Mode::Auto);
        c.watch("c2", "s1", Mode::Ask);
        assert_eq!(
            c.on_done("s1", 100),
            vec![("c1".into(), Decision::Send(1)), ("c2".into(), Decision::Propose)],
            "один чат идёт сам, соседний ждёт кнопки"
        );
        // передумали по одному — второй не шелохнулся
        c.set_mode("c2", Mode::Auto);
        c.set_mode("c1", Mode::Ask);
        assert_eq!(
            c.on_done("s1", 101),
            vec![("c1".into(), Decision::Propose), ("c2".into(), Decision::Send(1))]
        );
    }

    fn visit(chat: &str, at: i64, night: bool, usd: Option<f64>) -> Visit {
        Visit {
            at,
            chat_id: chat.into(),
            session_id: "s1".into(),
            step: 1,
            kind: "sent".into(),
            decided: "почини красные тесты".into(),
            ran: vec!["cargo test".into()],
            changed: "тронул src/a.rs; тесты КРАСНЫЕ".into(),
            night,
            usd,
        }
    }

    /// Окно счёта прежних тестов: сутки назад и дефолтные потолки — ровно то,
    /// как считалось до того, как ночь стала окном.
    fn win(now: i64) -> Window {
        Window::rolling(now, Caps::default())
    }

    /// Расход по чату — не выдумка цепочки, а прирост стоимости сессии у
    /// `usage`. Первый замер сравнивать не с чем, и это ЧЕСТНОЕ «не знаю», а не
    /// ноль: сессия могла работать и до цепочки.
    #[test]
    fn chat_spend_is_a_delta_of_usage_or_an_honest_nothing() {
        let v = Visits::new();
        assert_eq!(v.delta("s1", Some(4.0)), None, "первый замер только ставит отметку");
        assert_eq!(v.delta("s1", Some(4.5)), Some(0.5), "заход стоил прирост, а не всю сессию");
        assert_eq!(v.delta("s1", None), None, "usage промолчал — числа нет");
        // счётчик сессии сбросили (пересборка агрегатов) — отрицательного расхода не бывает
        assert_eq!(v.delta("s1", Some(0.1)), Some(0.0));
        // у каждой сессии своя отметка
        assert_eq!(v.delta("s2", Some(9.0)), None);
        assert_eq!(v.delta("s2", Some(9.25)), Some(0.25));

        let now = 1_000_000_000;
        let v = Visits::new();
        v.note(visit("c1", now - 3_600_000, true, Some(0.4))); // ночью
        v.note(visit("c1", now - 600_000, false, Some(0.6))); // утром
        v.note(visit("c1", now - 2 * DAY_MS, true, Some(50.0))); // позавчера — не в счёт
        let s = v.spend("c1", &win(now));
        assert!(s.known, "числа есть, а счётчик молчит");
        assert!((s.night - 0.4).abs() < 1e-9, "за ночь: {}", s.night);
        assert!((s.day - 1.0).abs() < 1e-9, "за сутки: {}", s.day);
        assert_eq!(s.visits, 2, "заходы старше суток в счёт не идут");

        // расход не посчитан — так и говорим, а не рисуем 0.00$
        let v = Visits::new();
        v.note(visit("c1", now, true, None));
        let s = v.spend("c1", &win(now));
        assert!(!s.known, "нулём подменили отсутствие числа");
        assert_eq!(s.night, 0.0);
        assert!(v.cap_hit("c1", true, &win(now)).is_none(), "потолок без чисел запрещать не вправе");
    }

    /// Ровно то, чего боится владелец: три автономных чата, каждый в своих
    /// рамках, втроём съедают недельный бюджет. Свой потолок их не остановит —
    /// останавливает ОБЩИЙ, и он обязан сказать, что чат тут ни при чём.
    #[test]
    fn the_shared_ceiling_stops_the_third_while_each_stays_within_its_own() {
        let now = 1_000_000_000;
        let v = Visits::new();
        for chat in ["c1", "c2"] {
            v.note(visit(chat, now - 3_600_000, true, Some(2.5))); // по 2.5$ — меньше своих 3$
        }
        v.note(visit("c3", now - 60_000, true, Some(0.9)));
        for chat in ["c1", "c2", "c3"] {
            let s = v.spend(chat, &win(now));
            assert!(s.night < s.night_cap, "{chat} вышел за свой потолок: {}", s.night);
        }
        assert!((v.spend("c3", &win(now)).all_night - 5.9).abs() < 1e-9);

        // ещё доллар третьему — общий потолок исчерпан
        v.note(visit("c3", now, true, Some(0.2)));
        assert!(v.spend("c3", &win(now)).night < CHAT_NIGHT_USD, "третий всё ещё в своих рамках");
        let hit = v.cap_hit("c3", true, &win(now)).expect("общий потолок промолчал");
        let text = hit.text;
        assert_eq!(hit.action, CapAction::Stop, "ночью потолок обязан рвать цепочку");
        assert!(text.contains("ВСЕХ автономных"), "не сказано, чей потолок кончился: {text}");
        assert!(text.contains("в своих рамках"), "человек решит, что виноват этот чат: {text}");
        assert!(text.contains("6.00$"), "потолок без числа ничего не решает: {text}");
        // и своим двоим тоже стоп — бюджет общий
        assert!(v.cap_hit("c1", true, &win(now)).is_some());

        // свой потолок работает отдельно и срабатывает первым
        let v = Visits::new();
        v.note(visit("c1", now, true, Some(3.2)));
        let text = v.cap_hit("c1", true, &win(now)).expect("свой потолок промолчал").text;
        assert!(text.contains("этого чата"), "{text}");
        assert!(!text.contains("ВСЕХ автономных"), "свой потолок назвался общим: {text}");
        // днём ночной потолок не считается, а дневная норма шире
        assert!(v.cap_hit("c1", false, &win(now)).is_none(), "ночной потолок сработал днём");
    }

    /// Журнал заходов — структура, которую показывают, и утренняя сводка обязана
    /// отвечать по ней на «куда он ушёл, пока я спал» за десять секунд чтения:
    /// по строке на чат, а не по строке на заход.
    #[test]
    fn the_visit_journal_answers_where_it_went_at_night() {
        let now = 1_000_000_000;
        let v = Visits::new();
        v.note(visit("c1", now - 7_200_000, true, Some(0.4)));
        let mut last = visit("c1", now - 3_600_000, true, Some(0.8));
        last.step = 2;
        last.decided = "прогони проверку и почини оставшееся".into();
        last.changed = "тронул src/a.rs, src/b.rs; тесты зелёные".into();
        v.note(last);
        v.note(visit("c2", now - 1_800_000, true, None)); // расход не посчитан
        v.note(visit("c3", now - 60_000, false, Some(9.9))); // дневной — не ночь

        let night = v.night_visits(&win(now));
        assert_eq!(night.len(), 3, "ночной срез забрал дневное или потерял ночное");
        assert!(night.windows(2).all(|w| w[0].at <= w[1].at), "журнал не по времени");

        let st = NightState::default();
        let d = morning_digest(&st, Some("claude 4.0% из 15%"), &night);
        assert!(d.contains("КУДА ОН УШЁЛ"), "на главный ночной вопрос ответа нет:\n{d}");
        assert!(d.contains("• c1 — заходов 2, 1.20$"), "заходы и деньги по чату:\n{d}");
        assert!(d.contains("прогони проверку"), "что решил последним заходом:\n{d}");
        assert!(d.contains("тесты зелёные"), "что изменилось:\n{d}");
        assert!(d.contains("• c2 — заходов 1, расход не посчитан"), "молчание счётчика — тоже ответ:\n{d}");
        assert!(!d.contains("c3"), "дневной чат попал в ночную сводку:\n{d}");
        // десять секунд чтения: по строке на чат, а не по строке на заход
        let block = d.split("КУДА ОН УШЁЛ").nth(1).unwrap().split("\nЧТО ВСТАЛО").next().unwrap();
        assert_eq!(block.lines().filter(|l| l.starts_with('•')).count(), 2);

        // журнал чата отдаётся наружу целиком — шапке есть что показать
        assert_eq!(v.for_chat("c1").len(), 2);
        assert!(v.for_chat("c9").is_empty(), "чужой журнал не выдумываем");
    }

    /// Потолки автономного чата вшиты в путь отправки — иначе они украшение:
    /// заход спрашивает СВОЙ потолок и отдельно общий бюджет провайдера.
    #[test]
    fn a_pass_asks_its_own_ceiling_and_the_shared_budget_both() {
        let src = include_str!("chain.rs");
        let body = src
            .split("Decision::Send(step) => {")
            .nth(1)
            .and_then(|t| t.split("deliver(").next())
            .expect("ветка отправки на месте");
        assert!(body.contains("cap_hit("), "свой потолок чата не спрашивается");
        assert!(body.contains("budget_gate("), "общий бюджет провайдера подменили своим потолком");
        assert!(
            body.find("cap_hit(").unwrap() < body.find("budget_gate(").unwrap(),
            "свой потолок считается на месте — спрашивать его после сети незачем"
        );
        assert!(body.contains("note_visit("), "заход не оставляет следа в журнале");
        // Решение про дневной потолок должно ЧИТАТЬСЯ в ветке отправки, а не
        // угадываться: стоп — только там, где потолок сам сказал «стоп».
        assert!(
            body.contains("hit.action == CapAction::Stop"),
            "по коду не видно, какой потолок рвёт цепочку, а какой предупреждает"
        );
        assert!(body.contains("warn_cap("), "днём потолок молчит вовсе — это не предупреждение");
        assert!(
            src.contains("fn warn_cap") && src.contains("note_cap_warning("),
            "дневное предупреждение повторяется на каждом заходе — это шум, а не предупреждение"
        );
    }

    // ── память между запусками ────────────────────────────────────────────

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("jarvis-chain-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// «Куда он ушёл, пока я спал» не имеет права отвечать пустотой из-за того,
    /// что приложение ночью перезапустилось. Журнал ходит через диск целиком:
    /// заходы, их цена и отметка расхода сессии.
    #[test]
    fn the_visit_journal_survives_a_restart() {
        let dir = temp_dir("visits");
        let path = dir.join("agent-visits.json");
        let now = 1_000_000_000;

        let v = Visits::at(path.clone());
        assert_eq!(v.delta("s1", Some(4.0)), None, "первый замер только ставит отметку");
        v.note(visit("c1", now - 3_600_000, true, Some(0.4)));

        // перезапуск: тот же файл, другой экземпляр
        let again = Visits::at(path.clone());
        let back = again.for_chat("c1");
        assert_eq!(back.len(), 1, "журнал не пережил перезапуск");
        assert_eq!(back[0].decided, "почини красные тесты", "заход дошёл не целиком");
        assert_eq!(back[0].ran, vec!["cargo test".to_string()]);
        assert!(back[0].night, "ночная метка захода потерялась");
        let s = again.spend("c1", &win(now));
        assert!(s.known, "числа были, а после перезапуска счётчик молчит");
        assert!((s.night - 0.4).abs() < 1e-9, "ночной счёт обнулился перезапуском: {}", s.night);
        // отметка сессии тоже на диске: иначе цена первого захода после
        // перезапуска стала бы честным, но лишним «не знаю»
        assert_eq!(again.delta("s1", Some(4.5)), Some(0.5), "отметка сессии не пережила перезапуск");

        // журнал без файла живёт в памяти и на диск не лезет — этим и гоняются тесты
        let mem = Visits::new();
        mem.note(visit("c2", now, true, Some(1.0)));
        assert!(Visits::at(path).for_chat("c2").is_empty(), "журнал в памяти написал на диск");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Ночь — ОКНО, а не время жизни процесса: перезапуск в три часа ночи не
    /// обнуляет ночной счёт, а вчерашняя ночь в него не попадает.
    #[test]
    fn night_counters_are_tied_to_the_window_not_to_the_process() {
        let h = |x: f64| (x * 3_600_000.0) as i64;
        // «Плоские сутки»: полночь на нуле, ночь с одиннадцати вечера до восьми
        // утра. Синтетический предикат вместо местного времени — тест про окно,
        // а не про часовой пояс.
        let night_at = |t: i64| {
            let m = t.rem_euclid(DAY_MS) / 60_000;
            !(8 * 60..23 * 60).contains(&m)
        };
        let three_am = DAY_MS * 2 + h(3.0);
        let start = night_since(three_am, night_at).expect("ночи не нашлось");
        assert_eq!(start, DAY_MS + h(23.0), "начало ночи не там, где его ставит предикат");
        // днём окно указывает на ПРОШЕДШУЮ ночь — «сколько стоила ночь» человек
        // спрашивает как раз утром
        assert_eq!(night_since(DAY_MS * 2 + h(12.0), night_at), Some(DAY_MS + h(23.0)));
        // ночи не бывает вовсе (границы выключены или мусорные) — считать нечего
        assert_eq!(night_since(three_am, |_| false), None);

        let dir = temp_dir("window");
        let path = dir.join("agent-visits.json");
        let before = Visits::at(path.clone());
        before.note(visit("c1", DAY_MS + h(23.5), true, Some(2.0))); // эта ночь, до перезапуска
        before.note(visit("c1", DAY_MS + h(3.5), true, Some(2.5))); // ПРОШЛАЯ ночь, 23.5 часа назад
        drop(before);

        let w = Window::at(three_am, start, Caps::default());
        let after = Visits::at(path); // перезапуск в три часа ночи
        let s = after.spend("c1", &w);
        assert!((s.night - 2.0).abs() < 1e-9, "ночь взяла чужое или потеряла своё: {}", s.night);
        assert!((s.day - 4.5).abs() < 1e-9, "сутки остались скользящими: {}", s.day);
        // ровно та разница, ради которой заведено окно: «за последние сутки»
        // сложило бы две ночи в одну
        assert!((after.spend("c1", &win(three_am)).night - 4.5).abs() < 1e-9);
        assert!(after.cap_hit("c1", true, &w).is_none(), "чужая ночь съела потолок этой");
        assert_eq!(after.night_visits(&w).len(), 1, "в сводку про эту ночь попала прошлая");

        // ещё доллар — и потолок ЭТОЙ ночи исчерпан, хотя процесс живёт минуту
        after.note(visit("c1", three_am, true, Some(1.2)));
        let hit = after.cap_hit("c1", true, &w).expect("ночной потолок промолчал");
        assert_eq!(hit.action, CapAction::Stop, "ночью потолок обязан рвать цепочку");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Ночная копилка — та же память: сводка «что было ночью» не имеет права
    /// пропасть из-за перезапуска, а отложенное до утра — тем более.
    #[test]
    fn the_night_pile_survives_a_restart() {
        let dir = temp_dir("night");
        let path = dir.join("agent-night.json");
        let now = now_ms();
        let w = Waiting {
            chat_id: "c1".into(),
            session_id: "sess-1234567890".into(),
            at: now,
            kind: "пуш в main".into(),
            prompt: "прогони тесты и запушь в main".into(),
        };
        let log = NightLog::at(path.clone(), now);
        log.hush("c1", "sent", "заход 1: почини красные тесты");
        log.defer(w.clone());
        drop(log);

        let again = NightLog::at(path.clone(), now); // перезапуск среди ночи
        assert_eq!(again.for_chat("c1"), vec![w.clone()], "отложенное не пережило перезапуск");
        let st = again.drain().expect("копилка после перезапуска пуста");
        assert_eq!(st.notices.len(), 1, "уведомление потеряно");
        assert!(st.notices[0].text.contains("почини красные тесты"), "текст карточки пересказан");

        // осушённая копилка легла на диск сразу: сводка одна на ночь, и
        // перезапуск сразу после неё не имеет права выкатить её второй раз
        let third = NightLog::at(path, now);
        assert!(third.drain().is_none(), "сводка повторилась после перезапуска");
        assert_eq!(third.for_chat("c1").len(), 1, "«ждёт тебя» снимает человек, а не сводка");

        // забытая неделя: уведомления протухают, решение — нет
        let dir2 = temp_dir("stale");
        let path2 = dir2.join("agent-night.json");
        let old = NightLog::at(path2.clone(), now);
        old.hush("c1", "sent", "заход позапрошлой ночи");
        old.defer(w);
        drop(old);
        let much_later = NightLog::at(path2, now + 5 * DAY_MS);
        assert!(much_later.is_empty(), "позапрошлая ночь выдаётся за сегодняшнюю");
        assert_eq!(much_later.for_chat("c1").len(), 1, "отложенное решение протухло само");
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    // ── потолки: чьи они и что делают ─────────────────────────────────────

    /// Потолки — настройка человека, а не константа сборки. И НЕ настройка
    /// агента: тот, кто вправе поднять себе потолок, потолка не имеет.
    #[test]
    fn the_ceilings_are_read_from_settings_and_closed_to_the_agent() {
        assert_eq!(Caps::from_settings(&json!({})), Caps::default(), "нет блока — дефолты");
        let c = Caps::from_settings(&json!({ "autonomy": { "chatNightUsd": 5.0, "allNightUsd": 9.0 } }));
        assert_eq!(c.chat_night, 5.0, "своё число не доехало");
        assert_eq!(c.all_night, 9.0);
        assert_eq!(c.chat_day, CHAT_DAY_USD, "частичный блок снёс соседнее число");
        // ноль — законный потолок: «автономии денег не даю»
        assert_eq!(Caps::from_settings(&json!({ "autonomy": { "chatDayUsd": 0.0 } })).chat_day, 0.0);
        // мусор потолок не отменяет: минус прижимается к нулю, не-число — к дефолту
        let c = Caps::from_settings(&json!({ "autonomy": { "chatNightUsd": -3.0, "chatDayUsd": "много" } }));
        assert_eq!((c.chat_night, c.chat_day), (0.0, CHAT_DAY_USD));
        assert_eq!(Caps::from_settings(&json!({ "autonomy": { "allDayUsd": 1e9 } })).all_day, MAX_CAP_USD);

        // дефолт настроек и дефолт кода — одно число, а не два похожих
        let d = crate::settings::defaults();
        assert!(d.get("autonomy").is_some(), "ручки в настройках нет");
        assert_eq!(Caps::from_settings(&d), Caps::default(), "настройки и код разошлись в дефолтах");

        // и то же самое, что с бюджетом: агенту эти ключи на запись не отдаются
        let allow = crate::capability::grant::SETTINGS_ALLOWLIST;
        assert!(!allow.contains(&"autonomy"), "агент вправе поднять себе потолок автономии");
        assert!(!allow.contains(&"budget"), "сторож заодно: бюджет тоже не агентский");

        // потолки едут в шапку из окна, а не из констант
        let now = 1_000_000_000;
        let v = Visits::new();
        v.note(visit("c1", now, true, Some(0.1)));
        let s = v.spend("c1", &Window::rolling(now, c));
        assert_eq!((s.night_cap, s.day_cap), (0.0, CHAT_DAY_USD));
    }

    /// Развилка, оставленная прошлым заходом: дневной потолок ПРЕДУПРЕЖДАЕТ, а
    /// рвёт цепочку только ночью. Днём человек рядом, расход у него в шапке, и
    /// решение остаётся за ним; ночью решать некому.
    #[test]
    fn by_day_the_ceiling_warns_and_by_night_it_stops() {
        let now = 1_000_000_000;
        let v = Visits::new();
        v.note(visit("c1", now - 3_600_000, false, Some(10.5))); // дневной расход сверх нормы
        let w = win(now);

        let hit = v.cap_hit("c1", false, &w).expect("дневной потолок промолчал вовсе");
        assert_eq!(hit.action, CapAction::Warn, "днём потолок рвёт работу, которую человек видит");
        assert!(hit.text.contains("10.00$") && hit.text.contains("10.50$"), "числа: {}", hit.text);
        assert!(hit.text.contains("предупреждение"), "решение не сказано словами: {}", hit.text);
        assert!(!hit.text.contains("останавливаю"), "предупреждение выдаёт себя за стоп: {}", hit.text);

        // то же число ночью — стоп, и человек читает почему
        let hit = v.cap_hit("c1", true, &w).expect("ночью дневная норма перестала считаться");
        assert_eq!(hit.action, CapAction::Stop, "ночью посмотреть некому — цепочка обязана встать");
        assert!(hit.text.contains("останавливаю"), "{}", hit.text);

        // общий дневной потолок ведёт себя так же
        let v = Visits::new();
        for chat in ["c1", "c2", "c3"] {
            v.note(visit(chat, now, false, Some(7.0))); // каждый в своих рамках, вместе 21$
        }
        let hit = v.cap_hit("c1", false, &w).expect("общий дневной потолок промолчал");
        assert_eq!(hit.action, CapAction::Warn);
        assert!(hit.text.contains("ВСЕХ автономных"), "{}", hit.text);
        assert_eq!(v.cap_hit("c1", true, &w).unwrap().action, CapAction::Stop);

        // сказать про это цепочка обязана один раз: повтор на каждом заходе —
        // шум, а новая цепочка — новое решение человека продолжать
        let c = chain(Mode::Auto);
        assert!(c.note_cap_warning("c1"), "первое предупреждение не сказано");
        assert!(!c.note_cap_warning("c1"), "то же самое повторяется каждый заход");
        c.watch("c1", "s2", Mode::Auto);
        assert!(c.note_cap_warning("c1"), "новая цепочка начинается с чистого листа");
        assert!(!c.note_cap_warning("c9"), "цепочки нет — и предупреждать не о чем");
    }
}
