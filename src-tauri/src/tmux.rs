//! tmux-транспорт: отдельный сервер `-L jarvis` (его поднимает claude-шим).
//!
//! Это канал ВВОДА демона: вставка ответов в пану, слэш-команды пульта,
//! ответы на вопросы клавишами. Текст всегда уходит элементом argv —
//! никакой интерполяции в shell-строку.

use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

const BRACKETED_PASTE_SETTLE: Duration = Duration::from_millis(90);
static BUFFER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
enum BufferKind {
    Reply,
    Command,
    Answer,
}

fn unique_buffer_name(kind: BufferKind) -> String {
    let kind = match kind {
        BufferKind::Reply => "reply",
        BufferKind::Command => "cmd",
        BufferKind::Answer => "answer",
    };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = BUFFER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "jarvis-{kind}-{}-{timestamp}-{sequence}",
        std::process::id()
    )
}

fn set_buffer_args<'a>(buffer: &'a str, text: &'a str) -> [&'a str; 5] {
    ["set-buffer", "-b", buffer, "--", text]
}

fn paste_buffer_args<'a>(buffer: &'a str, pane: &'a str) -> [&'a str; 7] {
    ["paste-buffer", "-p", "-d", "-b", buffer, "-t", pane]
}

fn focus_args<'a>(command: &'a str, pane: &'a str) -> [&'a str; 5] {
    ["-L", "jarvis", command, "-t", pane]
}

/// `tmux -L jarvis <args>`: stdout при успехе, текст ошибки при провале.
pub async fn tmux_j(args: &[&str]) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("tmux");
    cmd.arg("-L")
        .arg("jarvis")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(5), cmd.output())
        .await
        .map_err(|_| "tmux: таймаут".to_string())?
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            "tmux: ошибка".into()
        } else {
            err
        })
    }
}

pub async fn pane_alive(pane: &str) -> bool {
    tmux_j(&["display-message", "-p", "-t", pane, "ok"])
        .await
        .is_ok()
}

pub async fn capture_pane(pane: &str) -> Option<String> {
    tmux_j(&["capture-pane", "-t", pane, "-p"]).await.ok()
}

/// Человекочитаемое имя tmux-сессии паны — для бейджа в панели.
pub async fn session_name(pane: &str) -> Option<String> {
    tmux_j(&["display-message", "-p", "-t", pane, "#{session_name}"])
        .await
        .ok()
        .map(|s| crate::util::one_line(&s))
        .filter(|s| !s.is_empty())
}

/// Вставка промпта в пану. C-u срезает недописанный черновик в строке ввода —
/// иначе вставка доклеится к нему и Enter отправит склейку.
/// set-buffer → paste-buffer (bracketed, ради многострочных) → отдельный Enter.
pub async fn reply(pane: &str, prompt: &str) -> Result<(), String> {
    tmux_j(&["send-keys", "-t", pane, "C-u"]).await?;
    let buffer = unique_buffer_name(BufferKind::Reply);
    tmux_j(&set_buffer_args(&buffer, prompt)).await?;
    tmux_j(&paste_buffer_args(&buffer, pane)).await?;
    // даём TUI дожевать bracketed-paste, иначе Enter иногда обгоняет вставку
    // и текст остаётся в строке ввода неотправленным
    sleep(BRACKETED_PASTE_SETTLE).await;
    tmux_j(&["send-keys", "-t", pane, "Enter"]).await?;
    Ok(())
}

/// Пульт: слэш-команда с аргументом (`/model sonnet`, `/effort high`).
/// На длинной сессии /model показывает «Switch model?» — подтверждаем
/// выделенный по умолчанию вариант (Yes) ещё одним Enter, если он есть.
pub async fn paste_slash(pane: &str, text: &str) -> Result<(), String> {
    tmux_j(&["send-keys", "-t", pane, "C-u"]).await?; // не клеимся к черновику
    let buffer = unique_buffer_name(BufferKind::Command);
    tmux_j(&set_buffer_args(&buffer, text)).await?;
    tmux_j(&paste_buffer_args(&buffer, pane)).await?;
    sleep(BRACKETED_PASTE_SETTLE).await;
    tmux_j(&["send-keys", "-t", pane, "Enter"]).await?;
    sleep(Duration::from_millis(700)).await;
    if let Some(screen) = capture_pane(pane).await {
        // 11, не 12: у JS slice(-12) последний элемент — пустой хвост от trailing \n
        let tail: Vec<&str> = screen.lines().rev().take(11).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        let confirm = regex::RegexBuilder::new(r"Switch model\?|Enter to select|to confirm")
            .case_insensitive(true)
            .build()
            .unwrap();
        if confirm.is_match(&tail) {
            tmux_j(&["send-keys", "-t", pane, "Enter"]).await?;
        }
    }
    Ok(())
}

/// Метаданные живой паны для адопта осиротевших сессий при рестарте демона.
#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub pane_id: String,
    pub session_name: String,
    pub cwd: String,
    pub pid: i64,
}

/// Живые паны сервера jarvis с метаданными (id, имя сессии, cwd, pid процесса
/// паны). Семантика арм: `Ok(Some)` — успех, `Ok(None)` — tmux не установлен
/// (реестр не трогаем), `Err` — ошибка/пустой сервер.
/// Разделитель полей — таб: ни id, ни имя сессии, ни pid его не содержат, а путь
/// идёт последним полем.
pub async fn list_panes_meta() -> Result<Option<Vec<PaneInfo>>, ()> {
    let mut cmd = tokio::process::Command::new("tmux");
    cmd.args([
        "-L",
        "jarvis",
        "list-panes",
        "-a",
        "-F",
        "#{pane_id}\t#{session_name}\t#{pane_pid}\t#{pane_current_path}",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(4), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => Ok(Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| {
                    let mut it = line.splitn(4, '\t');
                    let pane_id = it.next()?.trim();
                    if pane_id.is_empty() {
                        return None;
                    }
                    let session_name = it.next().unwrap_or("").trim().to_string();
                    let pid = it.next().unwrap_or("").trim().parse::<i64>().unwrap_or(0);
                    let cwd = it.next().unwrap_or("").trim().to_string();
                    Some(PaneInfo {
                        pane_id: pane_id.to_string(),
                        session_name,
                        cwd,
                        pid,
                    })
                })
                .collect(),
        )),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => Err(()),
    }
}

/// Подписать tmux-окно заголовком сессии (терминал подписывает сам себя).
pub async fn rename_window(pane: &str, name: &str) -> Result<(), String> {
    tmux_j(&["rename-window", "-t", pane, name])
        .await
        .map(|_| ())
}

/// Ответ на вопрос(ы) клавишами в пану. Раскладку строит `answer_keys`
/// (чистая, протестирована); здесь — только проигрывание с задержками.
/// Именованные клавиши — send-keys; свой текст — через буфер, как `reply()`
/// (bracketed paste надёжен для юникода/пробелов, settle — чтобы следующий
/// Enter не обогнал вставку).
pub async fn answer_question(
    pane: &str,
    agent: crate::backend::Agent,
    q: &crate::model::Question,
    answers: &[Vec<u32>],
    texts: &[Option<String>],
) -> Result<(), String> {
    let keys = answer_keys(agent, q, answers, texts);
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            sleep(Duration::from_millis(140)).await; // дать пикеру перерисоваться
        }
        match k {
            Key::Named(name) => {
                tmux_j(&["send-keys", "-t", pane, name]).await?;
            }
            Key::Text(text) => {
                let buffer = unique_buffer_name(BufferKind::Answer);
                tmux_j(&set_buffer_args(&buffer, text)).await?;
                tmux_j(&paste_buffer_args(&buffer, pane)).await?;
                sleep(BRACKETED_PASTE_SETTLE).await;
            }
        }
    }
    Ok(())
}

/// «Где это?» — секундный оверлей прямо в терминале сессии.
/// popup рисуется в подключённом клиенте — у detached-сессии его нет.
pub async fn ping(pane: &str) -> Result<(), String> {
    let clients = tmux_j(&["list-clients", "-t", pane, "-F", "#{client_name}"])
        .await
        .unwrap_or_default();
    if crate::util::one_line(&clients).is_empty() {
        return Err("Окно терминала не подключено (detached) — показать негде".into());
    }
    tmux_j(&[
        "display-popup",
        "-t",
        pane,
        "-w",
        "34",
        "-h",
        "3",
        "-E",
        "printf \"\\n   ◇ Jarvis: вот эта сессия\"; sleep 1",
    ])
    .await
    .map(|_| ())
    .map_err(|e| {
        format!(
            "Поповер не показался: {}",
            crate::util::ellipsize(&crate::util::one_line(&e), 80)
        )
    })
}

// Клавиши пикеров Claude (выверено вживую на v2.1.172): несколько вопросов =
// табы [Q1][Q2]…[Submit]. single-select: цифра сама перескакивает на следующий
// таб; multiSelect: после тогглов нужен Tab/→, чтобы уйти с таба. После
// последнего вопроса — Review-экран, где «1» = «Submit answers».
const CLAUDE_ADVANCE: &str = "Tab"; // уйти с multiSelect-таба к следующему
const CLAUDE_SUBMIT_RIGHT: &str = "Right"; // Submit-таб одиночного multiSelect-вопроса
const CLAUDE_SUBMIT_CONFIRM: &str = "1"; // на Review-экране «1. Submit answers»

/// Клавиша раскладки ответа: именованная (send-keys как есть) либо свой текст
/// для строки «Other» — его вставляет транспорт через tmux-буфер, как `reply()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Named(String),
    Text(String),
}

impl Key {
    fn named(s: &str) -> Key {
        Key::Named(s.to_string())
    }
}

/// Спуск курсора к строке «Other» пикера Claude (стоит сразу после последней
/// опции) со Space-тогглами выбранных по пути (мультивыбор), затем текст и
/// Enter — Enter на «Other» подтверждает весь вопрос.
fn claude_other_keys(keys: &mut Vec<Key>, n_opts: u32, picks: &[u32], text: &str) {
    let mut picks: Vec<u32> = picks.to_vec();
    picks.sort_unstable();
    let mut cursor: u32 = 1; // подсветка стартует на опции 1
    for t in picks {
        for _ in cursor..t {
            keys.push(Key::named("Down"));
        }
        cursor = t;
        keys.push(Key::named("Space"));
    }
    for _ in cursor..=n_opts {
        keys.push(Key::named("Down"));
    }
    keys.push(Key::Text(text.to_string()));
    keys.push(Key::named("Enter"));
}

/// Плоская последовательность tmux send-keys для ответа на вопрос(ы).
/// Чистая и детерминированная — тестируется без tmux. `answers[i]` — выбранные
/// опции (1-based) вопроса `i`; `texts[i]` — свой ответ строкой «Other» (только
/// Claude; в single-select он приоритетнее опций). Без текстов —
/// последовательности байт-в-байт прежние. Ветвится по агенту и позиции вопроса.
pub fn answer_keys(
    agent: crate::backend::Agent,
    q: &crate::model::Question,
    answers: &[Vec<u32>],
    texts: &[Option<String>],
) -> Vec<Key> {
    use crate::backend::Agent;
    let mut keys = Vec::new();
    let n_q = q.questions.len();
    // свой текст вопроса `i`; пустой = нет
    let text_of = |i: usize| {
        texts
            .get(i)
            .and_then(|t| t.as_deref())
            .filter(|t| !t.is_empty())
    };

    match agent {
        Agent::Claude => {
            // Быстрый путь: один вопрос, single-select — цифра авто-подтверждает;
            // свой текст — это выбор «Other», опции игнорируем.
            if n_q == 1 && !q.questions[0].multi_select {
                if let Some(text) = text_of(0) {
                    claude_other_keys(&mut keys, q.questions[0].options.len() as u32, &[], text);
                } else if let Some(i) = answers.first().and_then(|a| a.first()) {
                    keys.push(Key::named(&i.to_string()));
                }
                return keys;
            }
            // Один вопрос, multiSelect — тоггл цифр, затем Submit-таб и «1».
            // Со своим текстом — Space-тогглы по пути вниз к «Other»; Enter на
            // нём подтверждает вопрос целиком, Submit-таб не нужен.
            if n_q == 1 {
                let picks = answers.first().map(Vec::as_slice).unwrap_or(&[]);
                if let Some(text) = text_of(0) {
                    claude_other_keys(&mut keys, q.questions[0].options.len() as u32, picks, text);
                } else {
                    for i in picks {
                        keys.push(Key::named(&i.to_string()));
                    }
                    keys.push(Key::named(CLAUDE_SUBMIT_RIGHT));
                    keys.push(Key::named(CLAUDE_SUBMIT_CONFIRM));
                }
                return keys;
            }
            // Несколько вопросов (табы). На каждый — цифры выбора. single-select
            // авто-перескакивает на следующий таб; multiSelect требует Tab после
            // тогглов; Enter на «Other» уводит с таба сам. После последнего
            // вопроса попадаем на Review — там «1».
            for (idx, item) in q.questions.iter().enumerate() {
                let picks = answers.get(idx).map(Vec::as_slice).unwrap_or(&[]);
                if let Some(text) = text_of(idx) {
                    let picks = if item.multi_select { picks } else { &[] };
                    claude_other_keys(&mut keys, item.options.len() as u32, picks, text);
                    continue;
                }
                for i in picks {
                    keys.push(Key::named(&i.to_string()));
                }
                if item.multi_select {
                    keys.push(Key::named(CLAUDE_ADVANCE));
                }
            }
            keys.push(Key::named(CLAUDE_SUBMIT_CONFIRM));
        }
        Agent::Codex => {
            // Codex всегда один вопрос (скрин-скрейп) и без строки «Other» —
            // свой текст сюда не доставить, texts игнорируем (ipc отфильтрует).
            // Навигация стрелками от подсветки на опции 1; Space тогглит в
            // мультивыборе; Enter подтверждает.
            let item_multi = q.questions.first().map(|x| x.multi_select).unwrap_or(false);
            let mut targets: Vec<u32> = answers.first().cloned().unwrap_or_default();
            targets.sort_unstable();
            let mut cursor: u32 = 1; // подсветка стартует на опции 1
            for t in targets {
                for _ in cursor..t {
                    keys.push(Key::named("Down"));
                }
                cursor = t;
                if item_multi {
                    keys.push(Key::named("Space"));
                }
            }
            keys.push(Key::named("Enter"));
        }
    }
    keys
}

/// Фокус-лесенка, ступень tmux: switch-client, не вышло — select-window.
pub async fn focus(pane: &str) -> bool {
    let direct_args = focus_args("switch-client", pane);
    let direct = tokio::process::Command::new("tmux")
        .args(direct_args)
        .output()
        .await;
    if matches!(&direct, Ok(o) if o.status.success()) {
        return true;
    }
    let select_args = focus_args("select-window", pane);
    let select = tokio::process::Command::new("tmux")
        .args(select_args)
        .output()
        .await;
    matches!(&select, Ok(o) if o.status.success())
}

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
                .map(|i| QuestionOption {
                    label: format!("o{i}"),
                    description: String::new(),
                })
                .collect(),
        }
    }
    fn q(items: Vec<QuestionItem>) -> Question {
        Question {
            at: 0,
            from_screen: false,
            questions: items,
        }
    }
    // ожидаемая раскладка: имена клавиш как есть, `~текст` — вставка текста
    fn seq(keys: &[&str]) -> Vec<Key> {
        keys.iter()
            .map(|s| match s.strip_prefix('~') {
                Some(text) => Key::Text(text.to_string()),
                None => Key::named(s),
            })
            .collect()
    }
    // texts-заглушка «без кастома» на n вопросов
    fn no_texts(n: usize) -> Vec<Option<String>> {
        vec![None; n]
    }

    // Claude, один вопрос, single-select: цифра авто-подтверждает.
    #[test]
    fn claude_single_question_single_select_just_digit() {
        let keys = answer_keys(Agent::Claude, &q(vec![item(false, 3)]), &[vec![2]], &[]);
        assert_eq!(keys, seq(&["2"]));
    }

    // Claude, один вопрос, multiSelect: тогглы → Right (Submit-таб) → «1».
    #[test]
    fn claude_single_question_multi_select_toggles_then_submit() {
        let keys = answer_keys(Agent::Claude, &q(vec![item(true, 3)]), &[vec![1, 3]], &[]);
        assert_eq!(keys, seq(&["1", "3", "Right", "1"]));
    }

    // Claude, два single-select вопроса (выверено вживую): каждая цифра сама
    // перескакивает на следующий таб; в конце «1» на Review-экране.
    #[test]
    fn claude_two_single_select_questions_autoadvance_then_submit() {
        let keys = answer_keys(
            Agent::Claude,
            &q(vec![item(false, 3), item(false, 2)]),
            &[vec![2], vec![1]],
            &no_texts(2),
        );
        assert_eq!(keys, seq(&["2", "1", "1"]));
    }

    // Claude, multiSelect-вопрос + single-select (выверено вживую): тогглы Q1,
    // затем Tab (уйти с multi-таба), цифра Q2 авто-перескок, «1» на Review.
    #[test]
    fn claude_multi_then_single_question() {
        let keys = answer_keys(
            Agent::Claude,
            &q(vec![item(true, 3), item(false, 2)]),
            &[vec![1, 3], vec![1]],
            &no_texts(2),
        );
        assert_eq!(keys, seq(&["1", "3", "Tab", "1", "1"]));
    }

    // Claude, single-select со своим текстом: Down до «Other» (после N опций),
    // вставка текста, Enter. Выбранная опция игнорируется — кастом и есть выбор.
    #[test]
    fn claude_single_select_custom_goes_to_other() {
        let keys = answer_keys(
            Agent::Claude,
            &q(vec![item(false, 3)]),
            &[vec![2]],
            &[Some("свой ответ".into())],
        );
        assert_eq!(keys, seq(&["Down", "Down", "Down", "~свой ответ", "Enter"]));
    }

    // Claude, multiSelect со своим текстом: Space-тогглы по пути вниз,
    // спуск к «Other», текст, Enter (подтверждает вопрос целиком).
    #[test]
    fn claude_multi_select_custom_combines_toggles_and_other() {
        let keys = answer_keys(
            Agent::Claude,
            &q(vec![item(true, 3)]),
            &[vec![1, 3]],
            &[Some("плюс вот это".into())],
        );
        assert_eq!(
            keys,
            seq(&["Space", "Down", "Down", "Space", "Down", "~плюс вот это", "Enter"])
        );
    }

    // Claude, multiSelect: только свой текст, без тогглов — чистый спуск к «Other».
    #[test]
    fn claude_multi_select_custom_only_text() {
        let keys = answer_keys(
            Agent::Claude,
            &q(vec![item(true, 2)]),
            &[vec![]],
            &[Some("только текст".into())],
        );
        assert_eq!(keys, seq(&["Down", "Down", "~только текст", "Enter"]));
    }

    // Claude, несколько вопросов, кастом в середине: цифра Q1 авто-перескок,
    // Q2 через «Other» (Enter уводит с таба), тогглы Q3 + Tab, «1» на Review.
    #[test]
    fn claude_multiquestion_custom_in_the_middle_keeps_transitions() {
        let keys = answer_keys(
            Agent::Claude,
            &q(vec![item(false, 3), item(false, 2), item(true, 2)]),
            &[vec![2], vec![], vec![1]],
            &[None, Some("иначе".into()), None],
        );
        assert_eq!(
            keys,
            seq(&["2", "Down", "Down", "~иначе", "Enter", "1", "Tab", "1"])
        );
    }

    // Пустой/отсутствующий texts не меняет старые последовательности байт-в-байт.
    #[test]
    fn claude_empty_texts_is_bytewise_legacy() {
        let question = q(vec![item(true, 3), item(false, 2)]);
        let answers = [vec![1, 3], vec![1]];
        let legacy = answer_keys(Agent::Claude, &question, &answers, &[]);
        let with_nones = answer_keys(Agent::Claude, &question, &answers, &no_texts(2));
        let with_empty = answer_keys(
            Agent::Claude,
            &question,
            &answers,
            &[Some(String::new()), None],
        );
        assert_eq!(legacy, seq(&["1", "3", "Tab", "1", "1"]));
        assert_eq!(with_nones, legacy);
        assert_eq!(with_empty, legacy);
    }

    // Codex, single-select: стрелки вниз от опции 1, затем Enter.
    #[test]
    fn codex_single_select_navigates_down_then_enter() {
        let keys = answer_keys(Agent::Codex, &q(vec![item(false, 4)]), &[vec![3]], &[]);
        assert_eq!(keys, seq(&["Down", "Down", "Enter"]));
    }

    // Codex, multiSelect: Space на каждой выбранной по ходу вниз, затем Enter.
    #[test]
    fn codex_multi_select_space_at_each_then_enter() {
        let keys = answer_keys(Agent::Codex, &q(vec![item(true, 4)]), &[vec![1, 3]], &[]);
        assert_eq!(keys, seq(&["Space", "Down", "Down", "Space", "Enter"]));
    }

    // Codex строку «Other» не рисует — свой текст игнорируется (ipc его режет раньше).
    #[test]
    fn codex_ignores_custom_text() {
        let keys = answer_keys(
            Agent::Codex,
            &q(vec![item(false, 4)]),
            &[vec![3]],
            &[Some("мимо".into())],
        );
        assert_eq!(keys, seq(&["Down", "Down", "Enter"]));
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[test]
    fn generated_buffer_names_are_unique_and_tmux_safe() {
        let reply = unique_buffer_name(BufferKind::Reply);
        let another_reply = unique_buffer_name(BufferKind::Reply);
        let command = unique_buffer_name(BufferKind::Command);

        assert_ne!(reply, another_reply);
        assert_ne!(reply, command);
        assert!(reply.starts_with("jarvis-reply-"));
        assert!(command.starts_with("jarvis-cmd-"));
        for name in [reply, another_reply, command] {
            assert!(name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
        }
    }

    #[test]
    fn buffer_command_args_keep_each_name_and_delete_after_paste() {
        assert_eq!(
            set_buffer_args("jarvis-reply-42", "line one\nline two"),
            [
                "set-buffer",
                "-b",
                "jarvis-reply-42",
                "--",
                "line one\nline two",
            ]
        );
        assert_eq!(
            paste_buffer_args("jarvis-reply-42", "%7"),
            [
                "paste-buffer",
                "-p",
                "-d",
                "-b",
                "jarvis-reply-42",
                "-t",
                "%7",
            ]
        );
    }

    #[test]
    fn focus_command_args_always_select_the_jarvis_server() {
        assert_eq!(
            focus_args("switch-client", "%3"),
            ["-L", "jarvis", "switch-client", "-t", "%3"]
        );
        assert_eq!(
            focus_args("select-window", "%3"),
            ["-L", "jarvis", "select-window", "-t", "%3"]
        );
    }
}
