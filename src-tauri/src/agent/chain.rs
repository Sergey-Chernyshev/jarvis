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

/// Отложенное до утра: необратимое, которое ночью не делается вовсе.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
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
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub at: i64,
    pub chat_id: String,
    pub kind: String,
    pub text: String,
}

/// Ночь целиком: что не показали, что ждёт человека, сколько потеряли по потолку.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NightState {
    pub notices: Vec<Notice>,
    pub waiting: Vec<Waiting>,
    pub dropped: u32,
}

/// Ночной журнал. Отдельный тип (как `Chains`) — чтобы тесты гоняли его без
/// живого приложения и не дрались за один процессный экземпляр.
#[derive(Default)]
pub struct NightLog {
    inner: Mutex<NightState>,
}

impl NightLog {
    pub fn new() -> Self {
        NightLog::default()
    }

    /// Не показать сейчас — показать утром. Ровно та строка, которую человек
    /// прочитал бы на карточке: пересказ по памяти утром уже не восстановить.
    pub fn hush(&self, chat_id: &str, kind: &str, text: &str) {
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
    }

    /// Отложить необратимое. Повтор того же захода по тому же чату не плодит
    /// вторую строчку: человеку решать один раз.
    pub fn defer(&self, w: Waiting) {
        let mut st = self.inner.lock().unwrap();
        if st
            .waiting
            .iter()
            .any(|x| x.chat_id == w.chat_id && x.prompt == w.prompt)
        {
            return;
        }
        st.waiting.push(w);
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
        let mut st = self.inner.lock().unwrap();
        let before = st.waiting.len();
        st.waiting.retain(|w| w.chat_id != chat_id);
        before - st.waiting.len()
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
        let mut st = self.inner.lock().unwrap();
        if st.notices.is_empty() {
            return None;
        }
        Some(NightState {
            notices: std::mem::take(&mut st.notices),
            waiting: st.waiting.clone(),
            dropped: std::mem::take(&mut st.dropped),
        })
    }
}

/// Процессный ночной журнал — один на приложение.
pub fn night_log() -> &'static NightLog {
    static N: std::sync::OnceLock<NightLog> = std::sync::OnceLock::new();
    N.get_or_init(NightLog::new)
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

/// Ночной потолок ОДНОГО автономного чата, доллары. Заход стоит порядка
/// четверти доллара, потолок глубины — десять заходов: три доллара это полная
/// цепочка с запасом, то есть ровно та работа, которую отдают на ночь.
pub const CHAT_NIGHT_USD: f64 = 3.0;

/// Дневная норма одного автономного чата. Днём человек рядом и видит, куда
/// уходит время, — норма втрое шире ночной и служит стопом от карусели, а не
/// рамкой работы.
pub const CHAT_DAY_USD: f64 = 10.0;

/// Общий ночной потолок ВСЕХ автономных чатов. Свой потолок держит один чат в
/// рамках, но трое таких, каждый «в своих рамках», съедают втрое больше — и
/// именно это владелец назвал недельным бюджетом. Две полные цепочки за ночь на
/// всех: третья встаёт и говорит об этом словами.
pub const ALL_NIGHT_USD: f64 = 6.0;

/// То же на сутки.
pub const ALL_DAY_USD: f64 = 20.0;

/// Один заход в журнале: не строка в логе, а структура — её показывают.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
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
fn sum_usd(list: &[Visit], now: i64, night_only: bool) -> (f64, bool) {
    let mut usd = 0.0;
    let mut known = false;
    for v in list
        .iter()
        .filter(|v| now - v.at <= DAY_MS && (!night_only || v.night))
    {
        if let Some(x) = v.usd {
            usd += x;
            known = true;
        }
    }
    (usd, known)
}

/// Журнал заходов. Отдельный тип (как `Chains` и `NightLog`) — чтобы тесты
/// гоняли его без живого приложения и не дрались за процессный экземпляр.
#[derive(Default)]
pub struct Visits {
    log: Mutex<HashMap<String, Vec<Visit>>>,
    /// Сессия → её стоимость на прошлом замере.
    marks: Mutex<HashMap<String, f64>>,
}

impl Visits {
    pub fn new() -> Self {
        Visits::default()
    }

    /// Во сколько обошёлся заход. Своих чисел у цепочки нет и быть не должно:
    /// токены считает `usage`, и берём мы у него ПРИРОСТ стоимости сессии с
    /// прошлого замера. Первый замер сравнивать не с чем — он только ставит
    /// отметку (тот же приём, что у `note_progress`): сессия могла работать и до
    /// цепочки, и записать её прошлое на эту ночь значит соврать числом.
    pub fn delta(&self, session_id: &str, total: Option<f64>) -> Option<f64> {
        let total = total?;
        let prev = self
            .marks
            .lock()
            .unwrap()
            .insert(session_id.to_string(), total);
        prev.map(|p| (total - p).max(0.0))
    }

    /// Записать заход. Хвост старше суток в счёт не идёт, но из журнала не
    /// выпадает: утром человек читает ночь целиком.
    pub fn note(&self, v: Visit) {
        let mut log = self.log.lock().unwrap();
        let list = log.entry(v.chat_id.clone()).or_default();
        list.push(v);
        if list.len() > MAX_VISITS {
            list.remove(0);
        }
    }

    /// Журнал чата, свежие в конце.
    pub fn for_chat(&self, chat_id: &str) -> Vec<Visit> {
        self.log.lock().unwrap().get(chat_id).cloned().unwrap_or_default()
    }

    /// Ночные заходы всех чатов за последние сутки, в порядке времени, — из них
    /// и складывается утренний ответ «куда он ушёл, пока я спал».
    pub fn night_visits(&self, now: i64) -> Vec<Visit> {
        let log = self.log.lock().unwrap();
        let mut out: Vec<Visit> = log
            .values()
            .flatten()
            .filter(|v| v.night && now - v.at <= DAY_MS)
            .cloned()
            .collect();
        out.sort_by_key(|v| v.at);
        out
    }

    /// Расход чата — свой и общий разом: одно без другого не решает ничего.
    pub fn spend(&self, chat_id: &str, now: i64) -> Spend {
        let log = self.log.lock().unwrap();
        let mine: &[Visit] = log.get(chat_id).map(Vec::as_slice).unwrap_or_default();
        let (night, kn) = sum_usd(mine, now, true);
        let (day, kd) = sum_usd(mine, now, false);
        let mut all_night = 0.0;
        let mut all_day = 0.0;
        for l in log.values() {
            all_night += sum_usd(l, now, true).0;
            all_day += sum_usd(l, now, false).0;
        }
        Spend {
            night,
            day,
            visits: mine.iter().filter(|v| now - v.at <= DAY_MS).count() as u32,
            known: kn || kd,
            night_cap: CHAT_NIGHT_USD,
            day_cap: CHAT_DAY_USD,
            all_night,
            all_night_cap: ALL_NIGHT_USD,
            all_day,
            all_day_cap: ALL_DAY_USD,
        }
    }

    /// Упёрлись ли в потолок — и в какой. СВОЙ и ОБЩИЙ проверяются оба: свой
    /// держит один чат в рамках, общий — всех разом, и три автономных чата, у
    /// каждого из которых всё в порядке, вместе съедают втрое больше.
    ///
    /// Это не замена ступеням бюджета (`budget.rs`): там недельная шкала
    /// провайдера и ночной потолок в процентах на всё приложение, здесь — деньги
    /// конкретных чатов. Оба спрашиваются, и любой из них может сказать «стоп».
    ///
    /// Чисел нет — запрета нет: врать потолком, которого не посчитали, хуже, чем
    /// пропустить заход; про молчание счётчика человек узнаёт из шапки.
    pub fn cap_refusal(&self, chat_id: &str, night: bool, now: i64) -> Option<String> {
        let s = self.spend(chat_id, now);
        if !s.known {
            return None;
        }
        let own = |what: &str, spent: f64, cap: f64| {
            format!("{what} потолок этого чата — {cap:.2}$, потрачено {spent:.2}$. Цепочку останавливаю")
        };
        let all = |what: &str, spent: f64, cap: f64, mine: f64| {
            format!(
                "{what} потолок ВСЕХ автономных чатов — {cap:.2}$, вместе они потратили {spent:.2}$. \
                 Этот чат в своих рамках ({mine:.2}$), но общий бюджет кончился — цепочку останавливаю"
            )
        };
        if night && s.night >= s.night_cap {
            return Some(own("Ночной", s.night, s.night_cap));
        }
        if night && s.all_night >= s.all_night_cap {
            return Some(all("Общий ночной", s.all_night, s.all_night_cap, s.night));
        }
        if s.day >= s.day_cap {
            return Some(own("Дневной", s.day, s.day_cap));
        }
        if s.all_day >= s.all_day_cap {
            return Some(all("Общий дневной", s.all_day, s.all_day_cap, s.day));
        }
        None
    }
}

/// Процессный журнал заходов — один на приложение.
pub fn visits() -> &'static Visits {
    static V: std::sync::OnceLock<Visits> = std::sync::OnceLock::new();
    V.get_or_init(Visits::new)
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

    let done = pick(&["sent"]);
    let stuck = pick(&["failed", "stopped"]);
    // Всё, что не легло в разделы, идёт хвостом. Хвост нужен именно затем, чтобы
    // тишина не съедала сигнал: новый вид карточки не должен пропасть молча.
    let known = ["sent", "failed", "stopped"];
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
    let text = morning_digest(
        &st,
        night_spent(app).as_deref(),
        &visits().night_visits(now_ms()),
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
    let now = now_ms();
    chains()
        .state(chat_id, mode_of(app, chat_id))
        .with_waiting(night_log().for_chat(chat_id))
        .with_log(visits().for_chat(chat_id), visits().spend(chat_id, now))
}

/// Событие цепочки наружу. Канал свой (`agent:chain`), но правило то же, что у
/// `agent:event`: метка чата обязательна — без неё карточка легла бы в чужой
/// разговор, стоит человеку уйти в соседний проект.
fn emit(app: &AppHandle, chat_id: &str, kind: &str, extra: Value) {
    // Тишина. Ночью карточка в чат ложится (работа идёт и должна быть видна), но
    // будить звуком и всплывашкой некого: уведомление копится до утра. Копим
    // только то, что человеку адресовано, — служебные срезы шапки в сводке лишние.
    let night = is_night(app);
    if night && ["sent", "proposed", "failed", "stopped", "deferred"].contains(&kind) {
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
            if let Some(text) = visits().cap_refusal(chat_id, night, now_ms()) {
                note_visit(d, chat_id, sid, step, "stopped", &text, &outcome, night);
                chains().stop(chat_id);
                emit(&app, chat_id, "stopped", json!({ "reason": "cap", "text": text }));
                return;
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
    let out = crate::ipc::via_gate_panel(
        d,
        "sessions.reply",
        json!({ "session_id": sid, "text": prompt }),
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
        let s = v.spend("c1", now);
        assert!(s.known, "числа есть, а счётчик молчит");
        assert!((s.night - 0.4).abs() < 1e-9, "за ночь: {}", s.night);
        assert!((s.day - 1.0).abs() < 1e-9, "за сутки: {}", s.day);
        assert_eq!(s.visits, 2, "заходы старше суток в счёт не идут");

        // расход не посчитан — так и говорим, а не рисуем 0.00$
        let v = Visits::new();
        v.note(visit("c1", now, true, None));
        let s = v.spend("c1", now);
        assert!(!s.known, "нулём подменили отсутствие числа");
        assert_eq!(s.night, 0.0);
        assert!(v.cap_refusal("c1", true, now).is_none(), "потолок без чисел запрещать не вправе");
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
            let s = v.spend(chat, now);
            assert!(s.night < s.night_cap, "{chat} вышел за свой потолок: {}", s.night);
        }
        assert!((v.spend("c3", now).all_night - 5.9).abs() < 1e-9);

        // ещё доллар третьему — общий потолок исчерпан
        v.note(visit("c3", now, true, Some(0.2)));
        assert!(v.spend("c3", now).night < CHAT_NIGHT_USD, "третий всё ещё в своих рамках");
        let text = v.cap_refusal("c3", true, now).expect("общий потолок промолчал");
        assert!(text.contains("ВСЕХ автономных"), "не сказано, чей потолок кончился: {text}");
        assert!(text.contains("в своих рамках"), "человек решит, что виноват этот чат: {text}");
        assert!(text.contains("6.00$"), "потолок без числа ничего не решает: {text}");
        // и своим двоим тоже стоп — бюджет общий
        assert!(v.cap_refusal("c1", true, now).is_some());

        // свой потолок работает отдельно и срабатывает первым
        let v = Visits::new();
        v.note(visit("c1", now, true, Some(3.2)));
        let text = v.cap_refusal("c1", true, now).expect("свой потолок промолчал");
        assert!(text.contains("этого чата"), "{text}");
        assert!(!text.contains("ВСЕХ автономных"), "свой потолок назвался общим: {text}");
        // днём ночной потолок не считается, а дневная норма шире
        assert!(v.cap_refusal("c1", false, now).is_none(), "ночной потолок сработал днём");
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

        let night = v.night_visits(now);
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
        assert!(body.contains("cap_refusal("), "свой потолок чата не спрашивается");
        assert!(body.contains("budget_gate("), "общий бюджет провайдера подменили своим потолком");
        assert!(
            body.find("cap_refusal(").unwrap() < body.find("budget_gate(").unwrap(),
            "свой потолок считается на месте — спрашивать его после сети незачем"
        );
        assert!(body.contains("note_visit("), "заход не оставляет следа в журнале");
    }
}
