//! Вторая половина подъёма сессии: дождаться, когда CLI станет готов, отдать
//! ему задачу — и ГРОМКО провалиться, если он так и не встал.
//!
//! Почему это отдельный модуль, а не десять строк в `ipc::deliver_task`.
//!
//! У агентов ДВА разных порядка запуска, и прежний ожидатель знал только один.
//!
//! * Claude и Codex заводят сессию сами: `session-start` приходит через секунду
//!   после открытия терминала. Ждать сессию в реестре — правильно, и слать
//!   раньше нельзя: паста уехала бы в ещё не готовый TUI.
//! * Kimi Code сессию до первой реплики не заводит вовсе — он так и пишет на
//!   приветственном экране: «No session yet — one will be created on your first
//!   message». Получалась взаимная блокировка: мы ждём сессию, чтобы отдать
//!   реплику, а сессии не будет, пока реплики нет. Четыре подъёма подряд не
//!   встали ни разу, три задачи человека не выполнялись вообще.
//!
//! Разрыв блокировки — не «поспать N секунд» (это гонка, и она молча
//! проигрывается на медленной машине), а ПРИЗНАК готовности, снятый с экрана
//! паны: нарисована строка ввода и на экране нет модального вопроса. Вопрос
//! («Trust this folder?», логин, выбор темы) — отдельный исход: ждать его
//! истечения бессмысленно, потому что нажать клавишу может только человек.
//!
//! И третье, ради чего это писалось: подъём, не ставший сессией, обязан быть
//! слышен. Раньше он исчезал молча — агент считал, что работа пошла, человек
//! считал, что задача роздана, а не происходило ничего.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::daemon::Daemon;
use crate::util::{ellipsize, now_ms, one_line};

/// Сколько всего ждём подъёма. Столько же, сколько ждал прежний сторож: цифра
/// снята с живых запусков, а не выдумана.
const WAIT_SECS: u64 = 90;
/// Сколько ждём хук `session-start` ПОСЛЕ того, как задача уже отдана в пану.
/// Задача при этом доставлена — это ожидание только про имя и родителя.
const BIND_SECS: u64 = 60;
/// Сколько подряд опросов экран обязан выглядеть готовым. Один кадр — это
/// перерисовка TUI на полпути; два подряд с секундой между ними — состояние.
const READY_STREAK: u32 = 2;
/// Запас на рассинхрон часов и секундную гранулярность `#{session_created}`.
const CREATED_SLACK_MS: i64 = 3_000;
/// Сколько последних строк паны показываем человеку. Больше в тост и в отказ
/// не влезет, меньше — не видно самого вопроса.
const TAIL_LINES: usize = 12;

/* ================= что видно на экране паны ================= */

/// Состояние окна агента до первой реплики.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    /// Строка ввода нарисована, модальных вопросов нет — можно писать.
    Ready,
    /// На экране ИЗВЕСТНЫЙ вопрос старта, который ждёт КЛАВИШУ. Сами мы её
    /// нажать не вправе: «доверяешь ли ты этому каталогу» — не наш ответ.
    /// Строка — причина человеческими словами.
    Blocked(String),
    /// Похоже на модальный пикер, но чей он и о чём — мы не знаем.
    ///
    /// Отдельно от `Blocked` не из педантизма. Для агента, который сессию ещё не
    /// завёл, любой пикер на экране — это вопрос старта, и он окончателен. А у
    /// claude пикер бывает и в работающей сессии (`AskUserQuestion`): объяви мы
    /// его провалом подъёма — живая сессия получила бы отказ на ровном месте.
    Modal,
    /// Ещё грузится, пусто или незнакомое. Ждём дальше.
    Warming,
}

/// Известные вопросы, на которых CLI встаёт колом ещё до первого хука.
/// Пары «маркер на экране → что сказать человеку».
const BLOCKERS: &[(&str, &str)] = &[
    (
        "trust this folder",
        "окно ждёт ответа на вопрос о доверии к каталогу — выбери «Trust this folder»",
    ),
    (
        "is this a project you created or one you trust",
        "окно ждёт ответа на вопрос о доверии к каталогу — выбери «Yes, I trust this folder»",
    ),
    (
        "do you trust the files in this folder",
        "окно ждёт ответа на вопрос о доверии к каталогу — подтверди доверие",
    ),
    (
        "choose the text style",
        "первый запуск CLI: он спрашивает оформление — выбери тему в окне",
    ),
    (
        "select login method",
        "CLI не залогинен — пройди вход в окне (или `claude /login` / `kimi` заново)",
    ),
    (
        "please run /login",
        "CLI не залогинен — выполни /login в окне",
    ),
    (
        "invalid api key",
        "CLI не пускают по ключу — проверь авторизацию агента",
    ),
    (
        "command not found",
        "команда агента не запустилась — бинаря нет в PATH запуска",
    ),
    (
        "настоящий бинарь",
        "шим не нашёл настоящий бинарь агента — проверь установку CLI",
    ),
];

/// Признаки того, что на экране висит модальный пикер, даже если сам вопрос нам
/// незнаком. Все три сняты с живых экранов kimi 0.38 и claude 2.1: подсказка
/// навигации рисуется ровно под вопросом и больше нигде.
const MODAL_HINTS: &[&str] = &[
    "↑↓ navigate",
    "enter to confirm",
    "enter select",
    "esc to cancel",
    "esc exit",
];

/// Строка ввода. У kimi это `│ >` внутри рамки, у claude — голый `❯`.
/// Регистронезависимость тут не нужна, а вот пробелы и рамка — да.
fn has_input_line(tail: &str) -> bool {
    tail.lines().any(|l| {
        let t = l.trim_start();
        let t = t.strip_prefix('│').or_else(|| t.strip_prefix('┃')).or_else(|| t.strip_prefix('|')).unwrap_or(t);
        let t = t.trim_start();
        t.starts_with("> ") || t == ">" || t.starts_with("❯ ") || t == "❯"
    })
}

/// Последние `n` содержательных строк экрана.
pub fn tail(screen: &str, n: usize) -> String {
    let lines: Vec<&str> = screen.lines().map(|l| l.trim_end()).collect();
    let end = lines.iter().rposition(|l| !l.is_empty()).map(|i| i + 1).unwrap_or(0);
    let start = end.saturating_sub(n);
    lines[start..end].join("\n")
}

/// Что происходит в окне. Чистая: ни tmux, ни демона — поэтому проверяется
/// тестами на настоящих снимках экранов.
///
/// Порядок веток — не вкусовщина. Вопрос проверяется ПЕРВЫМ, потому что на
/// экране доверия у claude есть строка `❯ 1. Yes, I trust this folder`, и
/// «есть строка ввода» на нём сработало бы: мы вставили бы задачу в пикер.
pub fn classify(screen: &str) -> Screen {
    let t = tail(screen, 24);
    if t.trim().is_empty() {
        return Screen::Warming;
    }
    let low = t.to_lowercase();
    for (marker, why) in BLOCKERS {
        if low.contains(marker) {
            return Screen::Blocked((*why).to_string());
        }
    }
    if MODAL_HINTS.iter().any(|h| low.contains(h)) {
        return Screen::Modal;
    }
    if has_input_line(&t) {
        return Screen::Ready;
    }
    Screen::Warming
}

/// Накопитель признака готовности: один кадр — это перерисовка TUI на полпути,
/// два подряд с секундой между ними — состояние. Отдельным типом, чтобы живая
/// проверка и боевой ожидатель считали готовность ОДНИМ кодом, а не двумя
/// похожими.
#[derive(Default)]
pub struct ReadyGate {
    streak: u32,
}

impl ReadyGate {
    /// Скормить очередной снимок экрана. `Some(Screen::Ready)` — готов
    /// по-настоящему; `Some(Screen::Blocked)` — сразу, без накопления (вопрос
    /// сам не рассосётся); `None` — ждём дальше.
    pub fn feed(&mut self, screen: &str) -> Option<Screen> {
        match classify(screen) {
            Screen::Blocked(why) => Some(Screen::Blocked(why)),
            Screen::Modal => Some(Screen::Modal),
            Screen::Ready => {
                self.streak += 1;
                (self.streak >= READY_STREAK).then_some(Screen::Ready)
            }
            Screen::Warming => {
                self.streak = 0;
                None
            }
        }
    }
}

/// Дождаться, когда пана станет готова принять реплику. Возвращает последнее
/// увиденное состояние и снимок экрана — по нему объясняют человеку, что не так.
///
/// Только для проб: боевой путь крутит `ReadyGate` сам, внутри `run`, потому что
/// там та же секунда занята ещё и поиском сессии. Оставлять эту обёртку
/// доступной боевому коду — обещать второй путь готовности, которого нет.
#[cfg(test)]
pub async fn await_ready(pane: &str, secs: u64) -> (Screen, Option<String>) {
    let mut gate = ReadyGate::default();
    let mut last: Option<String> = None;
    for _ in 0..secs {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let Some(screen) = crate::tmux::capture_pane(pane).await else { continue };
        let verdict = gate.feed(&screen);
        last = Some(screen);
        if let Some(v) = verdict {
            return (v, last);
        }
    }
    (Screen::Warming, last)
}

/* ================= подъём, не ставший сессией ================= */

/// Всё, что известно про несостоявшийся подъём. Одна структура на три адресата:
/// лог, тост человеку и отказ тому джарвису, который поднимал.
#[derive(Debug, Clone)]
pub struct Stall {
    pub agent: String,
    pub cwd: String,
    /// Сколько секунд ждали.
    pub secs: u64,
    /// Имя tmux-сессии брошенного окна — по нему его можно и найти, и погасить.
    pub tmux_session: Option<String>,
    pub pane: Option<String>,
    /// Последние строки экрана: там прямым текстом написано, что случилось.
    pub tail: Option<String>,
    /// Распознанная причина, если распознали.
    pub why: Option<String>,
    /// Текст задачи, который не доехал. Она не теряется — её можно повторить.
    pub task: Option<String>,
}

impl Stall {
    /// Отказ для того, кто поднимал: что не так, что видно в окне, что делать и
    /// куда девать задачу. Он же ложится в `sessions.get(<талон>)`.
    pub fn report(&self) -> String {
        let mut s = format!(
            "сессия {} в {} не встала за {} с — задача НЕ доставлена.",
            self.agent, self.cwd, self.secs
        );
        match &self.why {
            Some(w) => s.push_str(&format!("\nПричина: {w}.")),
            None => s.push_str("\nПричина неизвестна — смотри последние строки окна."),
        }
        if let Some(name) = &self.tmux_session {
            s.push_str(&format!(
                "\nОкно живо и НЕ погашено: tmux-сессия «{name}» \
                 (посмотреть — `tmux -L jarvis attach -t {name}`). \
                 Гасить её решает человек; агент может закрыть её своим sessions.close по талону."
            ));
        }
        if let Some(t) = &self.tail {
            s.push_str(&format!("\nПоследнее, что в окне:\n{t}"));
        }
        match &self.task {
            Some(task) => s.push_str(&format!(
                "\nЗадача цела: «{}». Не бросай её: либо повтори sessions.spawn (после того как \
                 вопрос в окне снят), либо отдай живой сессии через sessions.reply.",
                ellipsize(&one_line(task), 160)
            )),
            None => s.push_str("\nПервого промпта не было — терять нечего."),
        }
        s
    }

    /// Заголовок и тело карточки человеку. Тост — единственный канал, который
    /// доходит до него сам: лог он не читает, а список сессий не покажет то,
    /// чего в нём нет.
    pub fn toast(&self) -> (String, String) {
        let title = format!("⚠︎ {} не поднялся", self.agent);
        let mut body = match &self.why {
            Some(w) => format!("{w}. "),
            None => String::new(),
        };
        body.push_str(&format!("Каталог {}. ", self.cwd));
        if let Some(name) = &self.tmux_session {
            body.push_str(&format!("Окно осталось: tmux -L jarvis attach -t {name}. "));
        }
        if self.task.is_some() {
            body.push_str("Задача не доставлена и не потеряна — повтори запуск или отдай её живой сессии.");
        }
        (title, one_line(&body))
    }
}

/// Журнал несостоявшихся подъёмов: талон → что случилось.
///
/// Живёт здесь, а не в реестре запусков, ровно по одной причине: реестр —
/// капабилити-слой, и ему для громкого отказа достаточно спросить это одной
/// строкой (`ready::stall_of(&ticket)`), не заводя своего учёта.
static STALLS: OnceLock<Mutex<Vec<(String, Stall)>>> = OnceLock::new();

fn stalls() -> &'static Mutex<Vec<(String, Stall)>> {
    STALLS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Чем кончился подъём по этому талону. `None` — либо всё хорошо, либо талона
/// такого не было.
pub fn stall_of(ticket: &str) -> Option<Stall> {
    stalls()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .rev()
        .find(|(t, _)| t == ticket)
        .map(|(_, s)| s.clone())
}

/// Талон → имя tmux-окна, которое он открыл. Заполняется в тот момент, когда
/// пана НАЙДЕНА, а не когда подъём провалился: талон могут снять и раньше
/// («передумал»), и тогда окно тоже нельзя оставлять безымянным.
static WINDOWS: OnceLock<Mutex<Vec<(String, String)>>> = OnceLock::new();

fn windows() -> &'static Mutex<Vec<(String, String)>> {
    WINDOWS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Как называется tmux-окно этого подъёма. Единственный способ и найти его
/// глазами, и погасить: сессии Jarvis у него нет — гасить нечего по её id.
pub fn window_of(ticket: &str) -> Option<String> {
    if let Some(name) = windows()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .rev()
        .find(|(t, _)| t == ticket)
        .map(|(_, n)| n.clone())
    {
        return Some(name);
    }
    stall_of(ticket).and_then(|s| s.tmux_session)
}

fn note_window(ticket: &str, name: &str) {
    let mut list = windows().lock().unwrap_or_else(|e| e.into_inner());
    list.push((ticket.to_string(), name.to_string()));
    let len = list.len();
    if len > 64 {
        list.drain(..len - 64);
    }
}

fn remember_stall(ticket: &str, s: &Stall) {
    let mut list = stalls().lock().unwrap_or_else(|e| e.into_inner());
    list.push((ticket.to_string(), s.clone()));
    // Журнал вечно расти не должен: провалов на порядки меньше, чем запусков,
    // но демон живёт неделями.
    let len = list.len();
    if len > 64 {
        list.drain(..len - 64);
    }
}

/* ================= поиск своей паны ================= */

/// Паны, которые уже сторожит другой ожидатель. Без этого два одновременных
/// подъёма в ОДНОМ каталоге (ровно боевой случай: четыре сессии подряд в
/// `FastWorkBot/server`) отдали бы обе задачи в одно окно, а второе бросили.
static CLAIMED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn claimed() -> &'static Mutex<HashSet<String>> {
    CLAIMED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Занятая пана. Отпускается сама — в том числе если ожидатель упал по пути.
pub(crate) struct Claim {
    pub(crate) pane: String,
    pub(crate) name: String,
}

impl Drop for Claim {
    fn drop(&mut self) {
        claimed()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.pane);
    }
}

/// Путь в том написании, в каком его увидит tmux: `pane_current_path` приходит
/// уже разыменованным (`/tmp/x` → `/private/tmp/x` на macOS), а нам его сверять
/// с тем, что дали на вход.
fn canon(p: &str) -> String {
    let p = p.trim().trim_end_matches('/');
    std::fs::canonicalize(p)
        .map(|c| c.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string())
}

/// Чистое ядро выбора паны — тестируется без tmux.
///
/// Своей считается пана, которая: (1) стоит в нашем каталоге, (2) поднялась не
/// раньше нашего запуска, (3) не привязана к уже известной сессии, (4) не занята
/// соседним ожидателем. Из подходящих берём САМУЮ СТАРУЮ: ожидатели стартуют в
/// том же порядке, что и терминалы, и старший обязан забрать своё окно, иначе он
/// уведёт окно младшего, а младший останется ни с чем.
fn pick_pane<'a>(
    panes: &'a [crate::tmux::PaneInfo],
    cwd: &str,
    since: i64,
    busy: &HashSet<String>,
    taken: &HashSet<String>,
) -> Option<&'a crate::tmux::PaneInfo> {
    let want = canon(cwd);
    panes
        .iter()
        .filter(|p| canon(&p.cwd) == want)
        .filter(|p| p.created * 1000 + CREATED_SLACK_MS >= since)
        .filter(|p| !busy.contains(&p.pane_id) && !taken.contains(&p.pane_id))
        .min_by_key(|p| (p.created, p.pane_id.clone()))
}

/// Найти и занять свою пану. `None` — ещё не появилась (терминал открывается не
/// мгновенно) или tmux не отвечает.
pub(crate) async fn claim_pane(d: &Arc<Daemon>, cwd: &str, since: i64) -> Option<Claim> {
    let panes = crate::tmux::list_panes_meta().await.ok().flatten()?;
    let busy: HashSet<String> = {
        let sessions = d.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.values().filter(|s| s.remote.is_none()).filter_map(|s| s.tmux_pane.clone()).collect()
    };
    let mut taken = claimed().lock().unwrap_or_else(|e| e.into_inner());
    let p = pick_pane(&panes, cwd, since, &busy, &taken)?;
    taken.insert(p.pane_id.clone());
    Some(Claim {
        pane: p.pane_id.clone(),
        name: p.session_name.clone(),
    })
}

/* ================= сам ожидатель ================= */

/// Сессия ЭТОГО запуска в реестре демона: тот же хост, тот же каталог, появилась
/// после запуска. Либо — точное совпадение по нашей пане, если мы её уже нашли:
/// это надёжнее любых эвристик по каталогу.
fn find_session(
    d: &Arc<Daemon>,
    machine: &str,
    cwd: &str,
    since: i64,
    pane: Option<&str>,
) -> Option<(String, String)> {
    let sessions = d.sessions.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pane) = pane {
        if let Some(s) = sessions.values().find(|s| s.tmux_pane.as_deref() == Some(pane) && s.remote.as_deref().unwrap_or("") == (if machine == "local" { "" } else { machine })) {
            return Some((s.id.clone(), pane.to_string()));
        }
        return None;
    }
    let cwd = cwd.trim_end_matches('/');
    sessions
        .values()
        .filter(|s| {
            let same_host = if machine.is_empty() || machine == "local" {
                s.remote.is_none()
            } else {
                s.remote.as_deref() == Some(machine)
            };
            same_host
                && s.cwd.as_deref().map(|c| c.trim_end_matches('/')) == Some(cwd)
                && s.created_at >= since
        })
        .max_by_key(|s| s.created_at)
        .and_then(|s| s.tmux_pane.clone().map(|p| (s.id.clone(), p)))
}

/// Отдать задачу в пану — локально или через узел.
async fn send(d: &Arc<Daemon>, machine: &str, pane: &str, text: &str) -> Result<(), String> {
    if machine.is_empty() || machine == "local" {
        crate::tmux::reply(pane, text).await
    } else {
        match d.remotes.node(machine).and_then(|n| n.client().ok()) {
            Some(c) => c.reply(pane, text).await,
            None => Err("узел пропал из настроек".into()),
        }
    }
}

/// Отдать задачу агенту, как только он встанет, — и не молчать, если не встал.
///
/// Возвращает управление сразу: терминал уже открыт, а всё остальное едет фоном.
/// Тот же контракт, что был у прежнего `ipc::deliver_task`, и та же форма
/// вызова плюс один аргумент — `agent`. Он обязателен: ПОРЯДОК ЗАПУСКА (заводит
/// ли CLI сессию сам или только после первой реплики) — свойство агента, и без
/// него ожидатель снова начнёт ждать хук у того, кто его не пришлёт.
pub fn deliver(
    d: &Arc<Daemon>,
    machine: &str,
    cwd: &str,
    agent: &str,
    task: Option<String>,
    bind: Option<crate::capability::native::spawn::Bind>,
) {
    let text = task.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    if text.is_none() && bind.is_none() {
        return;
    }
    let (d, machine, agent) = (d.clone(), machine.to_string(), agent.to_string());
    let cwd = cwd.trim_end_matches('/').to_string();
    let since = now_ms();
    tauri::async_runtime::spawn(async move {
        run(d, machine, cwd, agent, text, bind, since).await;
    });
}

async fn run(
    d: Arc<Daemon>,
    machine: String,
    cwd: String,
    agent: String,
    text: Option<String>,
    bind: Option<crate::capability::native::spawn::Bind>,
    since: i64,
) {
    let local = machine.is_empty() || machine == "local";
    let be = crate::backend::backend(crate::backend::Agent::from_label(&agent));
    // Порядок запуска — свойство агента, а не наша догадка. Claude и Codex
    // заводят сессию сами: слать им раньше хука нельзя, и мы не шлём.
    let hook_first = be.session_before_prompt();
    // Пану ищем даже там, где не собираемся в неё писать: без неё провал был бы
    // безымянным («не встал»), а с ней у него есть имя окна и последние строки.
    let hunt_pane = local;

    let mut claim: Option<Claim> = None;
    let mut gate = ReadyGate::default();
    let mut last_screen: Option<String> = None;

    for _ in 0..WAIT_SECS {
        tokio::time::sleep(Duration::from_secs(1)).await;

        // 1) Сессия появилась сама — прежний, ничем не тронутый путь.
        if let Some((id, pane)) = find_session(&d, &machine, &cwd, since, claim.as_ref().map(|c| c.pane.as_str())) {
            if let Some(b) = &bind {
                crate::capability::native::spawn::on_bound(&d, b, &id).await;
            }
            let Some(text) = &text else { return };
            match send(&d, &machine, &pane, text).await {
                Ok(()) => crate::log::line(&format!("launch: задача уехала в {id}")),
                Err(e) => crate::log::line(&format!("launch: задача не доехала: {e}")),
            }
            return;
        }

        if !hunt_pane {
            continue;
        }
        if claim.is_none() {
            claim = claim_pane(&d, &cwd, since).await;
            // Окно нашлось — значит у талона появилось имя. Записываем СРАЗУ:
            // талон могут снять и до таймаута, и тогда `sessions.close` обязан
            // знать, что именно гасить, иначе окно останется брошенным.
            if let (Some(c), Some(b)) = (&claim, &bind) {
                note_window(&b.ticket, &c.name);
            }
        }
        let Some(c) = &claim else { continue };
        let Some(screen) = crate::tmux::capture_pane(&c.pane).await else { continue };
        let verdict = gate.feed(&screen);
        last_screen = Some(screen);

        // 2) На экране вопрос, который ждёт человека. Досиживать до таймаута
        //    незачем: сами мы клавишу не нажмём, и через 89 секунд ответ будет
        //    тот же самый — только человек узнает о нём на полторы минуты позже.
        //
        //    Незнакомый пикер (`Modal`) окончателен только для того, кто сессию
        //    ещё не завёл: у claude пикер бывает и в работающей сессии, и объяви
        //    мы его провалом — живая работа получила бы отказ на ровном месте.
        let stopper = match &verdict {
            Some(Screen::Blocked(why)) => Some(why.clone()),
            Some(Screen::Modal) if !hook_first => Some(
                "окно ждёт ответа на вопрос — что за вопрос, видно в последних строках".to_string(),
            ),
            _ => None,
        };
        if let Some(why) = stopper {
            give_up(&d, &agent, &cwd, since, &bind, &text, Some(c), last_screen.as_deref(), Some(why)).await;
            return;
        }

        // 3) Агент, заводящий сессию сам, ждёт свой хук — в пану не пишем.
        //    Экран мы всё равно читали: он пригодится в отказе.
        if hook_first {
            continue;
        }

        // 4) CLI готов принять реплику — признак продержался положенное.
        if verdict != Some(Screen::Ready) {
            continue;
        }

        // Модель — ДО задачи: после неё агент уже думает, и `/model` уехал бы в
        // работающий ход. Сессии ещё нет, поэтому идём в пану напрямую.
        let mut bind = bind.clone();
        if let Some(b) = &mut bind {
            if let Some(m) = b.model.clone() {
                if be.validate_model(&m).is_ok() {
                    let _ = crate::tmux::paste_slash(&c.pane, &format!("/model {m}")).await;
                    tokio::time::sleep(Duration::from_millis(700)).await;
                }
                // Дальше модель уже стоит: повторно её ставить в `on_bound`
                // нельзя — там это слэш-команда в уже РАБОТАЮЩУЮ сессию.
                b.model = None;
            }
        }
        let Some(text) = &text else { return };
        if let Err(e) = send(&d, &machine, &c.pane, text).await {
            crate::log::line(&format!("launch: задача не доехала в пану {}: {e}", c.pane));
            give_up(&d, &agent, &cwd, since, &bind, &Some(text.clone()), Some(c), last_screen.as_deref(), Some(format!("вставка в окно не удалась: {e}"))).await;
            return;
        }
        crate::log::line(&format!(
            "launch: {agent} не заводит сессию до реплики — задача отдана в пану {} ({})",
            c.pane, c.name
        ));
        // Сессия заведётся как СЛЕДСТВИЕ реплики — теперь ждём её ради имени и
        // родителя. Задача при этом уже доставлена, и провал этого ожидания —
        // не потеря работы, а всего лишь безымянный чат.
        await_bind(&d, &machine, &cwd, since, c, bind).await;
        return;
    }

    give_up(&d, &agent, &cwd, since, &bind, &text, claim.as_ref(), last_screen.as_deref(), None).await;
}

/// Дождаться хука уже после того, как задача отдана: имя, родитель, модель.
async fn await_bind(
    d: &Arc<Daemon>,
    machine: &str,
    cwd: &str,
    since: i64,
    c: &Claim,
    bind: Option<crate::capability::native::spawn::Bind>,
) {
    let Some(b) = bind else { return };
    for _ in 0..BIND_SECS {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some((id, _)) = find_session(d, machine, cwd, since, Some(&c.pane)) {
            crate::capability::native::spawn::on_bound(d, &b, &id).await;
            return;
        }
    }
    crate::log::line(&format!(
        "launch: задача отдана в пану {}, но хук сессии не пришёл за {BIND_SECS} с — чат остался безымянным",
        c.pane
    ));
}

/// Провал. Раньше он был строкой в логе и снятым талоном — то есть тишиной.
/// Теперь у него три адресата, и ни один не узнаёт о нём последним.
#[allow(clippy::too_many_arguments)]
async fn give_up(
    d: &Arc<Daemon>,
    agent: &str,
    cwd: &str,
    since: i64,
    bind: &Option<crate::capability::native::spawn::Bind>,
    text: &Option<String>,
    claim: Option<&Claim>,
    screen: Option<&str>,
    why: Option<String>,
) {
    // Причина, если её видно с нашей стороны, а не с экрана: незакрытый вопрос
    // о доверии виден по отсутствию отметки, даже когда пану мы не нашли.
    let why = why.or_else(|| crate::launch::stall_hint(agent, cwd));
    let stall = Stall {
        agent: agent.to_string(),
        cwd: cwd.to_string(),
        secs: ((now_ms() - since) / 1000).max(0) as u64,
        tmux_session: claim.map(|c| c.name.clone()),
        pane: claim.map(|c| c.pane.clone()),
        tail: screen.map(|s| tail(s, TAIL_LINES)).filter(|s| !s.trim().is_empty()),
        why,
        task: text.clone(),
    };

    crate::log::line(&format!("[launch] ПОДЪЁМ НЕ СОСТОЯЛСЯ\n{}", stall.report()));

    // Человеку — карточка. Клик открывает чат родителя: оттуда задачу и
    // поднимали, туда же её и возвращать.
    let (title, body) = stall.toast();
    d.notify(
        &title,
        &body,
        bind.as_ref().and_then(|b| b.parent.as_deref()),
        "error",
    );

    // Тому джарвису, который поднимал, — через его же талон: `sessions.get`
    // отдаст этот самый текст вместо бодрого «поднимается».
    if let Some(b) = bind {
        remember_stall(&b.ticket, &stall);
        d.spawns.give_up(&b.ticket);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Снято с живого kimi 0.38 в НЕдоверенном каталоге. Это ровно тот экран,
    /// на котором ночью висели брошенные процессы.
    const KIMI_TRUST: &str = "\
 ──────────────────────────────────────────────
  Trust this folder?
  ↑↓ navigate · Enter select · Esc exit

  /private/tmp/kimi-trust-probe

  Project-level MCP servers are disabled until you explicitly choose Trust.

     Trust this folder
     Enable project MCP servers. Remembered for this folder.

   ❯ Don't trust
     Exit Kimi Code. Asked again next launch.
 ──────────────────────────────────────────────";

    /// Снято с живого claude 2.1 в НЕдоверенном каталоге.
    const CLAUDE_TRUST: &str = "\
 Accessing workspace:

 /private/tmp/claude-trust-probe

 Quick safety check: Is this a project you created or one you trust?

 ❯ 1. Yes, I trust this folder
   2. No, exit

 Enter to confirm · Esc to cancel";

    /// Снято с живого kimi 0.38 в ДОВЕРЕННОМ каталоге: он сам пишет, что сессии
    /// ещё нет и она появится с первой репликой. Ровно этот экран прежний
    /// ожидатель считал «агент не встал».
    const KIMI_READY: &str = "\
 │  ▐█▛█▛█▌  Welcome to Kimi Code!                    │
 │  Directory: /Users/u/PycharmProjects/FastWorkBot/server │
 │  Session:                                          │
 │  Model:     K3                                     │

   No session yet — one will be created on your first message.

 ╭────────────────────────────────────────────────────╮
 │ >                                                  │
 ╰────────────────────────────────────────────────────╯
 yolo  K3 thinking: high  …/FastWorkBot/server  main
                                    context: 0% (0/1M)";

    const CLAUDE_READY: &str = "\
✻ Cogitated for 16s

────────────────────────────────────────────────
❯
────────────────────────────────────────────────
  ⏵⏵ don't ask on (shift+tab to cycle) · ← 1 agent";

    /// Главное различение всего модуля: «ждёт реплику» против «ждёт клавишу».
    /// Спутай их — и задача уедет в пикер доверия, выбрав в нём случайный пункт.
    #[test]
    fn a_trust_question_is_never_mistaken_for_a_prompt() {
        assert!(matches!(classify(KIMI_TRUST), Screen::Blocked(_)));
        assert!(matches!(classify(CLAUDE_TRUST), Screen::Blocked(_)));
        // и причина названа словами, а не «не встал»
        let Screen::Blocked(why) = classify(KIMI_TRUST) else { unreachable!() };
        assert!(why.contains("довери"), "{why}");
        let Screen::Blocked(why) = classify(CLAUDE_TRUST) else { unreachable!() };
        assert!(why.contains("довери"), "{why}");
    }

    /// У claude на экране доверия есть строка `❯ 1. Yes, I trust this folder` —
    /// то есть признак «строка ввода» на нём срабатывает. Порядок проверок и
    /// есть защита; тест сторожит именно порядок.
    #[test]
    fn the_question_is_checked_before_the_input_line() {
        assert!(has_input_line(CLAUDE_TRUST), "предпосылка теста: признак ввода тут есть");
        assert!(matches!(classify(CLAUDE_TRUST), Screen::Blocked(_)));
    }

    #[test]
    fn a_ready_cli_is_recognised_for_both_agents() {
        assert_eq!(classify(KIMI_READY), Screen::Ready);
        assert_eq!(classify(CLAUDE_READY), Screen::Ready);
    }

    /// Пустой и наполовину отрисованный экран — это «ждём дальше», а не
    /// «готов»: вставка в недорисованный TUI теряется.
    #[test]
    fn a_blank_or_half_drawn_screen_is_not_ready() {
        assert_eq!(classify(""), Screen::Warming);
        assert_eq!(classify("   \n\n  "), Screen::Warming);
        assert_eq!(classify("Loading…"), Screen::Warming);
        assert_eq!(classify(" ▐█▛█▛█▌  Welcome to Kimi Code!"), Screen::Warming);
    }

    /// Незнакомый вопрос — тоже вопрос: подсказка навигации рисуется только под
    /// модальным пикером. Вставить задачу в чужой список нельзя, но и назвать
    /// его провалом подъёма для КАЖДОГО агента нельзя тоже — отсюда `Modal`
    /// отдельно от `Blocked` (у claude пикер бывает и в работающей сессии).
    #[test]
    fn an_unknown_modal_is_a_modal_not_a_named_blocker() {
        let s = "Что-то новое спрашивают\n  ❯ 1. Да\n    2. Нет\n Enter to confirm · Esc to cancel";
        assert_eq!(classify(s), Screen::Modal);
        assert_ne!(classify(s), Screen::Ready, "в незнакомый пикер не пишем");
        // а известный вопрос старта остаётся именно им — с причиной словами
        assert!(matches!(classify(KIMI_TRUST), Screen::Blocked(_)));
    }

    /// Шим не нашёл бинарь — это не «агент задумался», это конец.
    #[test]
    fn a_dead_launch_is_blocked_not_warming() {
        let s = "jarvis-shim: настоящий бинарь claude не найден в PATH";
        assert!(matches!(classify(s), Screen::Blocked(_)));
        assert!(matches!(classify("zsh: command not found: kimi"), Screen::Blocked(_)));
    }

    #[test]
    fn tail_keeps_the_last_meaningful_lines() {
        let s = "a\nb\nc\nd\n\n\n";
        assert_eq!(tail(s, 2), "c\nd");
        assert_eq!(tail(s, 99), "a\nb\nc\nd");
        assert_eq!(tail("", 5), "");
    }

    fn pane(id: &str, name: &str, cwd: &str, created: i64) -> crate::tmux::PaneInfo {
        crate::tmux::PaneInfo {
            pane_id: id.into(),
            session_name: name.into(),
            cwd: cwd.into(),
            pid: 1,
            created,
        }
    }

    /// Своей пана считается по каталогу И времени: соседнее окно того же
    /// проекта, открытое вчера, задачу получить не должно.
    #[test]
    fn an_older_pane_in_the_same_project_is_not_ours() {
        let dir = std::env::temp_dir().join("jarvis-ready-pick");
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        let since = 1_700_000_000_000i64;
        let panes = vec![
            pane("%1", "old", &cwd, 1_600_000_000),
            pane("%2", "mine", &cwd, 1_700_000_000),
        ];
        let none = HashSet::new();
        let got = pick_pane(&panes, &cwd, since, &none, &none).unwrap();
        assert_eq!(got.pane_id, "%2");
        // чужой каталог не наш ни при каком времени
        assert!(pick_pane(&panes, "/nowhere-at-all", since, &none, &none).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Два подъёма подряд в ОДНОМ каталоге — боевой случай (четыре сессии в
    /// `FastWorkBot/server`). Занятая пана второму ожидателю не достаётся,
    /// иначе одно окно получило бы обе задачи, а второе — ни одной.
    #[test]
    fn two_waiters_in_one_directory_do_not_fight_over_a_pane() {
        let dir = std::env::temp_dir().join("jarvis-ready-pick2");
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        let panes = vec![
            pane("%1", "first", &cwd, 1_700_000_000),
            pane("%2", "second", &cwd, 1_700_000_005),
        ];
        let none = HashSet::new();
        // старший ожидатель видит оба окна и обязан взять СТАРШЕЕ — своё
        let a = pick_pane(&panes, &cwd, 1_700_000_000_000, &none, &none).unwrap();
        assert_eq!(a.pane_id, "%1");
        let mut taken = HashSet::new();
        taken.insert(a.pane_id.clone());
        let b = pick_pane(&panes, &cwd, 1_700_000_003_000, &none, &taken).unwrap();
        assert_eq!(b.pane_id, "%2");
        // пана уже привязанной сессии не свободна вовсе
        let mut busy = HashSet::new();
        busy.insert("%2".to_string());
        assert!(pick_pane(&panes, &cwd, 1_700_000_003_000, &busy, &taken).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn stall() -> Stall {
        Stall {
            agent: "kimi".into(),
            cwd: "/Users/u/PycharmProjects/FastWorkBot/wt-server-19478".into(),
            secs: 90,
            tmux_session: Some("wt-server-19478-12128".into()),
            pane: Some("%42".into()),
            tail: Some("Trust this folder?\n  ❯ Don't trust".into()),
            why: Some("окно ждёт ответа на вопрос о доверии к каталогу".into()),
            task: Some("проверь свежесть данных".into()),
        }
    }

    /// Отказ обязан отвечать на три вопроса разом: что случилось, что видно в
    /// окне и куда девать задачу. Без последнего работа теряется молча — ровно
    /// то, из-за чего три задачи человека не были сделаны вообще.
    #[test]
    fn the_refusal_says_what_happened_what_is_on_screen_and_what_to_do() {
        let r = stall().report();
        assert!(r.contains("не встала за 90 с"), "{r}");
        assert!(r.contains("НЕ доставлена"), "{r}");
        assert!(r.contains("довери"), "причина не названа: {r}");
        assert!(r.contains("Trust this folder?"), "экран не показан: {r}");
        assert!(r.contains("wt-server-19478-12128"), "имя окна не названо: {r}");
        assert!(r.contains("проверь свежесть данных"), "задача не названа: {r}");
        assert!(r.contains("sessions.spawn") && r.contains("sessions.reply"),
            "не сказано, куда девать задачу: {r}");
    }

    /// Осиротевшее окно называется по имени и НЕ гасится молча: гасить чужую
    /// работу по таймеру — решение, которое принимает человек, а не сторож.
    #[test]
    fn the_orphan_window_is_named_and_not_killed_behind_the_back() {
        let r = stall().report();
        assert!(r.contains("НЕ погашено"), "{r}");
        assert!(r.contains("tmux -L jarvis attach -t wt-server-19478-12128"), "{r}");
    }

    /// Карточка человеку — не «что-то пошло не так», а причина + куда смотреть.
    #[test]
    fn the_human_card_is_actionable_and_one_line() {
        let (title, body) = stall().toast();
        assert!(title.contains("kimi"), "{title}");
        assert!(body.contains("довери"), "{body}");
        assert!(body.contains("wt-server-19478-12128"), "{body}");
        assert!(body.contains("не потеряна"), "{body}");
        assert!(!body.contains('\n'), "тост однострочный: {body}");
    }

    /// Талон, не ставший сессией, обязан ОСТАВИТЬ СЛЕД: иначе `sessions.get`
    /// отвечает «не найдено», и агент до конца считает, что работа идёт.
    #[test]
    fn a_failed_ticket_leaves_a_readable_trace() {
        assert!(stall_of("spawn-никогда-не-было").is_none());
        remember_stall("spawn-test-1", &stall());
        let got = stall_of("spawn-test-1").expect("след остался");
        assert_eq!(got.tmux_session.as_deref(), Some("wt-server-19478-12128"));
        assert!(got.report().contains("НЕ доставлена"));
    }

    /// Имя окна известно ДВУМЯ путями: как только пану нашли (талон могут снять
    /// раньше таймаута) и по итогам провала. Иначе брошенное окно осталось бы
    /// безымянным — ровно так и накопились шесть чужих процессов.
    #[test]
    fn the_window_name_is_known_before_the_timeout_too() {
        assert!(window_of("spawn-никогда-не-было").is_none());
        note_window("spawn-test-2", "wt-server-77-4242");
        assert_eq!(window_of("spawn-test-2").as_deref(), Some("wt-server-77-4242"));
        // и через журнал провалов — для талона, чью пану записать не успели
        remember_stall("spawn-test-3", &stall());
        assert_eq!(window_of("spawn-test-3").as_deref(), Some("wt-server-19478-12128"));
    }

    /// Сторож формы вызова: ровно так `ipc::deliver_task` зовёт этот модуль.
    /// Разъедься сигнатуры — правка из отчёта перестанет компилироваться молча,
    /// а узнают об этом на живом запуске.
    #[allow(dead_code)]
    fn the_documented_call_from_ipc_still_type_checks(
        d: &Arc<Daemon>,
        machine: &str,
        cwd: &str,
        agent: &str,
        task: Option<String>,
        bind: Option<crate::capability::native::spawn::Bind>,
    ) {
        deliver(d, machine, cwd, agent, task, bind);
    }

    /// Порядок запуска — свойство агента, и оно не должно тихо перевернуться:
    /// отправь claude реплику до его хука — она уедет в недорисованный TUI.
    #[test]
    fn only_kimi_gets_the_task_before_its_hook() {
        use crate::backend::{backend, Agent};
        assert!(backend(Agent::Claude).session_before_prompt());
        assert!(backend(Agent::Codex).session_before_prompt());
        assert!(!backend(Agent::Kimi).session_before_prompt());
    }

    /// ЖИВАЯ проверка: настоящий kimi, настоящий tmux, ЧУЖОЙ проект.
    ///
    /// В общий прогон не входит (`#[ignore]`): поднимает процесс, тратит время и
    /// требует установленного CLI. Но без неё вся эта работа — рассуждение:
    /// прежний ожидатель тоже выглядел правильным, а не встал ни разу.
    ///
    ///   JARVIS_LIVE_DIR=/путь/к/чужому/проекту \
    ///   JARVIS_LIVE_TRUST=0|1 \
    ///   cargo test --no-default-features launch::ready::tests::live -- --ignored --nocapture
    ///
    /// `JARVIS_LIVE_TRUST=0` — каталог НЕ помечаем доверенным: так проверяется
    /// второй исход, «окно ждёт клавишу», ради которого ночью и висели брошенные
    /// процессы.
    #[tokio::test]
    #[ignore]
    async fn live_prompt_first_agent_gets_its_task_without_a_hook() {
        let dir = std::env::var("JARVIS_LIVE_DIR").expect("нужен JARVIS_LIVE_DIR=<каталог>");
        let trust = std::env::var("JARVIS_LIVE_TRUST").unwrap_or_else(|_| "1".into()) != "0";
        assert!(std::path::Path::new(&dir).is_dir(), "нет каталога {dir}");
        // Ровно то, что делает боевой путь перед открытием терминала.
        if trust {
            crate::launch::prepare_workspace("kimi", &dir);
        }
        println!("== каталог {dir} (доверие проставляем: {trust})");

        let since = now_ms();
        let name = format!("jarvis-live-{}", std::process::id());
        let shims = crate::util::jarvis_dir().join("shims");
        let cmd = format!(
            "PATH={}:$PATH exec kimi",
            crate::util::shell_quote(&shims.to_string_lossy())
        );
        crate::tmux::tmux_j(&["new-session", "-d", "-s", &name, "-c", &dir, "sh", "-lc", &cmd])
            .await
            .expect("tmux поднял окно");

        // 1) Свою пану находим САМИ — без единого хука агента. В этом и суть.
        let mut pane = None;
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let panes = crate::tmux::list_panes_meta().await.ok().flatten().unwrap_or_default();
            let none = HashSet::new();
            if let Some(p) = pick_pane(&panes, &dir, since, &none, &none) {
                println!("== пана найдена без хука: {} ({})", p.pane_id, p.session_name);
                pane = Some(p.pane_id.clone());
                break;
            }
        }
        let pane = pane.expect("пана своего запуска не нашлась");

        // 2) Готовность — по признаку, а не по «поспать N секунд».
        let (state, screen) = await_ready(&pane, 90).await;
        println!("== вердикт: {state:?}\n{}", tail(screen.as_deref().unwrap_or(""), 8));

        if !trust {
            // Недоверенный каталог: единственный правильный исход — назвать
            // вопрос, а не молчать полторы минуты и не жать клавиши вслепую.
            assert!(matches!(state, Screen::Blocked(_)), "вопрос о доверии не распознан");
            let _ = crate::tmux::kill_session(&name).await;
            return;
        }
        assert_eq!(state, Screen::Ready, "CLI не дошёл до строки ввода");

        // 3) Задача уходит в пану — и сессия у kimi заводится КАК СЛЕДСТВИЕ.
        let key = crate::backend::kimi::workdir_key(
            &std::fs::canonicalize(&dir).unwrap().to_string_lossy(),
        );
        let sessions_dir = crate::backend::kimi::kimi_home().join("sessions").join(&key);
        let names = || -> HashSet<String> {
            std::fs::read_dir(&sessions_dir)
                .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
                .unwrap_or_default()
        };
        let before = names();
        crate::tmux::reply(&pane, "ответь одним словом: готово").await.expect("вставка удалась");

        let mut born: Option<String> = None;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if let Some(s) = names().difference(&before).next() {
                born = Some(s.clone());
                break;
            }
        }
        println!("== сессия kimi завелась после реплики: {born:?}");
        let _ = crate::tmux::kill_session(&name).await;
        assert!(born.is_some(), "реплика ушла, а сессия так и не завелась — блокировка не разорвана");
    }
}
