//! tmux на стороне узла — тот же отдельный сервер `-L jarvis`, что и на ноуте
//! (его поднимает claude-шим). Это единственный канал ВВОДА узла.
//!
//! Последовательность вставки повторяет `src/tmux.rs` дословно (C-u →
//! set-buffer → paste-buffer -p → пауза → Enter): она выверена вживую, и
//! расхождение означало бы, что удалённая сессия ведёт себя иначе локальной —
//! ровно то, чего дизайн старается не допустить. Копия, а не вызов: тот модуль
//! сцеплен с моделью и реестром демона, а узел собирается без них.
//!
//! Текст всегда уходит элементом argv — никакой интерполяции в shell-строку.

use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

/// Пауза между клавишами плана ответа: пикеру нужно перерисоваться, иначе
/// следующая цифра прилетает в ещё не обновлённый экран.
const KEY_STEP: Duration = Duration::from_millis(140);
/// Сколько ждать первый экран агента, прежде чем смотреть, не спрашивает ли он
/// про доверие к каталогу. Меньше — увидим пустой экран, больше — человек ждёт.
const TRUST_PROMPT_WAIT: Duration = Duration::from_millis(1500);

/// Дать TUI дожевать bracketed-paste, иначе Enter обгоняет вставку и текст
/// остаётся в строке ввода неотправленным.
const BRACKETED_PASTE_SETTLE: Duration = Duration::from_millis(90);
/// Пауза перед проверкой экрана: подтверждение `/model` рисуется не мгновенно.
const SLASH_CONFIRM_WAIT: Duration = Duration::from_millis(700);
const TMUX_TIMEOUT: Duration = Duration::from_secs(5);

static BUFFER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Имя буфера уникально на процесс+время+счётчик: два одновременных ответа в
/// разные паны не должны затирать буфер друг другу.
fn unique_buffer_name(kind: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = BUFFER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("jarvis-node-{kind}-{}-{timestamp}-{sequence}", std::process::id())
}

/// Аргументы вставки вынесены в функции ровно как в `src/tmux.rs` — так их
/// видно тестом и не даёт разъехаться с демоном по одному флагу.
fn set_buffer_args<'a>(buffer: &'a str, text: &'a str) -> [&'a str; 5] {
    ["set-buffer", "-b", buffer, "--", text]
}

fn paste_buffer_args<'a>(buffer: &'a str, pane: &'a str) -> [&'a str; 7] {
    ["paste-buffer", "-p", "-d", "-b", buffer, "-t", pane]
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
    let out = tokio::time::timeout(TMUX_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "tmux: таймаут".to_string())?
        // самая частая причина здесь — tmux вообще не установлен на VPS;
        // это граница из дизайна, и ноут должен увидеть её текстом
        .map_err(|e| format!("tmux: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { "tmux: ошибка".into() } else { err })
    }
}

/// Вставка промпта в пану. C-u срезает недописанный черновик в строке ввода —
/// иначе вставка доклеится к нему и Enter отправит склейку.
pub async fn reply(pane: &str, prompt: &str) -> Result<(), String> {
    tmux_j(&["send-keys", "-t", pane, "C-u"]).await?;
    let buffer = unique_buffer_name("reply");
    tmux_j(&set_buffer_args(&buffer, prompt)).await?;
    tmux_j(&paste_buffer_args(&buffer, pane)).await?;
    sleep(BRACKETED_PASTE_SETTLE).await;
    tmux_j(&["send-keys", "-t", pane, "Enter"]).await?;
    Ok(())
}

/// Шаг плана ответа на вопрос агента: именованная клавиша или вставка текста.
///
/// План считает НОУТ: раскладка пикеров Claude/Codex — его знание, и меняется
/// она вместе с версиями агентов. Узел только исполняет — ровно как в дизайне
/// («выполнять `tmux send-keys`» умеет, «UI» не умеет).
pub enum Key {
    Named(String),
    Text(String),
}

/// Разобрать план с провода: `[{"key":"Down"}, {"text":"свой ответ"}]`.
/// Ключ `key` — имя клавиши для tmux, `text` — вставка через буфер.
pub fn parse_keys(v: &Value) -> Option<Vec<Key>> {
    let arr = v.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        if let Some(k) = item.get("key").and_then(Value::as_str) {
            out.push(Key::Named(k.to_string()));
        } else {
            // шаг непонятной формы валит план целиком: проиграть половину —
            // значит оставить пикер в произвольном состоянии
            let t = item.get("text").and_then(Value::as_str)?;
            out.push(Key::Text(t.to_string()));
        }
    }
    Some(out)
}

/// Проиграть план в пану. Пауза между шагами — чтобы пикер успел перерисоваться
/// (то же значение, что и на ноуте: последовательность выверена вживую).
pub async fn play_keys(pane: &str, keys: &[Key]) -> Result<(), String> {
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            sleep(KEY_STEP).await;
        }
        match k {
            Key::Named(name) => {
                tmux_j(&["send-keys", "-t", pane, name]).await?;
            }
            Key::Text(text) => {
                let buffer = unique_buffer_name("answer");
                tmux_j(&set_buffer_args(&buffer, text)).await?;
                tmux_j(&paste_buffer_args(&buffer, pane)).await?;
                sleep(BRACKETED_PASTE_SETTLE).await;
            }
        }
    }
    Ok(())
}

/// Поднять сессию агента в отдельной сессии `tmux -L jarvis`.
///
/// `-d` обязателен: клиента здесь нет и быть не может — за этой машиной никто
/// не сидит, а хуки агента доедут до ноута и без подключённого терминала.
/// Команда уходит через `bash -lc`: неинтерактивная оболочка не читает профиль,
/// и `claude`, поставленный через nvm или в `~/.local/bin`, оказался бы «не
/// найден» ровно там, где он есть.
pub async fn launch(cwd: &str, cmd: &str, name: Option<&str>) -> Result<(String, String), String> {
    // std, а не tokio::fs: фича "fs" узлу больше нигде не нужна, а тянуть её
    // ради одного mkdir — лишний вес там, где вес и есть смысл крейта.
    std::fs::create_dir_all(cwd).map_err(|e| format!("не создал {cwd}: {e}"))?;
    let session = session_name(cwd, name);
    // `-P -F` заставляет tmux напечатать пану. Без неё запустивший остаётся
    // ни с чем: сессия агента ещё не зарегистрирована (хук придёт позже, а на
    // первом запуске в новом каталоге Claude сначала спрашивает, доверять ли
    // ему), и показать человеку происходящее было бы нечем.
    let wrapped = with_agent_path(cmd);
    let pane = tmux_j(&[
        "new-session", "-d", "-P", "-F", "#{pane_id}", "-s", &session, "-c", cwd,
        "bash", "-lc", &wrapped,
    ])
    .await?;
    let pane = pane.trim().to_string();

    // Первый запуск в новом каталоге Claude встречает вопросом «доверяешь ли
    // ты этой папке?» — и до ответа не стартует, то есть не шлёт ни одного
    // хука. Снаружи это выглядит как «запустил, а сессии нет»: терминала на
    // той машине никто не видит, а в списке пусто.
    //
    // Подтверждаем сами, и вот почему это не самоуправство: каталог назвал
    // человек, попросив именно в нём поднять агента. Отказ доверять папке,
    // которую ты сам только что завёл, не имеет смысла — а вопрос остаётся
    // висеть. Тот же приём уже используется для подтверждения `/model`.
    sleep(TRUST_PROMPT_WAIT).await;
    if let Ok(screen) = tmux_j(&["capture-pane", "-t", &pane, "-p"]).await {
        if needs_trust(&screen) {
            tmux_j(&["send-keys", "-t", &pane, "Enter"]).await?;
        }
    }
    Ok((session, pane))
}

/// Дополнить PATH местами, где реально живут агенты.
///
/// Узел работает под systemd, и `bash -lc` ему не помогает: логин-шелл читает
/// `~/.profile`, а npm/nvm/нативный установщик Claude Code дописывают себя в
/// `~/.bashrc`, который неинтерактивная оболочка не читает вовсе. Итог —
/// `claude` находится (шим лежит в PATH), запускается и падает с кодом 127,
/// не найдя за собой настоящий бинарь. Снаружи это выглядит как «сессия
/// поднялась и сразу исчезла»: пана есть ровно мгновение.
///
/// Дописываем в НАЧАЛО: шим Jarvis должен оставаться первым, если он есть, но
/// настоящий бинарь обязан находиться за ним.
fn with_agent_path(cmd: &str) -> String {
    format!(
        "export PATH=\"$HOME/.local/bin:$HOME/bin:$HOME/.bun/bin:$HOME/.npm-global/bin:\
         $HOME/.local/share/pnpm:$HOME/.claude/local:/usr/local/bin:/opt/homebrew/bin:$PATH\"\n{cmd}"
    )
}

/// Стоит ли пана на вопросе о доверии к каталогу.
///
/// Ищем связку признаков, а не одну фразу: заголовок вопроса и вариант «да».
/// Одиночное «trust» встречается и в обычном выводе агента, а ошибиться здесь —
/// значит нажать Enter там, где спрашивали совсем о другом.
fn needs_trust(screen: &str) -> bool {
    let tail: String = screen.lines().rev().take(20).collect::<Vec<_>>().join("\n").to_lowercase();
    tail.contains("trust this folder") && tail.contains("do you trust")
        || tail.contains("trust this folder") && tail.contains("yes, i trust")
}

/// Видимый экран паны — то, что увидел бы человек, подключившись к ней.
///
/// Нужен ровно для случая «запустил, а сессии нет»: агент жив, но стоит на
/// вопросе (например, «доверять этому каталогу?») и потому ещё не прислал ни
/// одного хука. Без экрана это неотличимо от «ничего не запустилось».
pub async fn screen(pane: &str) -> Result<String, String> {
    tmux_j(&["capture-pane", "-t", pane, "-p"]).await
}

/// Имя tmux-сессии: человекочитаемое и уникальное. Совпадение имён tmux не
/// прощает — вторая сессия того же проекта просто не создалась бы.
fn session_name(cwd: &str, name: Option<&str>) -> String {
    let base = name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            cwd.trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("project")
                .to_string()
        });
    let safe: String = base
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let safe = safe.trim_matches('-');
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{}-{stamp}", if safe.is_empty() { "project" } else { safe })
}

/// Пульт: слэш-команда с аргументом (`/model sonnet`, `/effort high`).
/// На длинной сессии `/model` показывает «Switch model?» — подтверждаем
/// выделенный по умолчанию вариант ещё одним Enter, если он есть.
pub async fn slash(pane: &str, text: &str) -> Result<(), String> {
    tmux_j(&["send-keys", "-t", pane, "C-u"]).await?; // не клеимся к черновику
    let buffer = unique_buffer_name("cmd");
    tmux_j(&set_buffer_args(&buffer, text)).await?;
    tmux_j(&paste_buffer_args(&buffer, pane)).await?;
    sleep(BRACKETED_PASTE_SETTLE).await;
    tmux_j(&["send-keys", "-t", pane, "Enter"]).await?;
    sleep(SLASH_CONFIRM_WAIT).await;
    if let Ok(screen) = tmux_j(&["capture-pane", "-t", pane, "-p"]).await {
        if needs_confirm(&screen) {
            tmux_j(&["send-keys", "-t", pane, "Enter"]).await?;
        }
    }
    Ok(())
}

/// Те же маркеры подтверждения, что у демона, но подстроками, а не regex:
/// ради трёх фраз узел не тянет ещё один крейт. Смотрим только хвост экрана —
/// выше по скроллу эти слова могли остаться от прошлых ходов.
fn needs_confirm(screen: &str) -> bool {
    // 11, не 12: у JS slice(-12) последний элемент — пустой хвост от trailing \n
    let tail: Vec<&str> = screen.lines().rev().take(11).collect();
    let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n").to_lowercase();
    ["switch model?", "enter to select", "to confirm"]
        .into_iter()
        .any(|marker| tail.contains(marker))
}

/// Живая пана сервера jarvis. Ноут по этому списку понимает, что сессия на VPS
/// ещё существует, — статусы он ведёт сам.
pub struct Pane {
    pub pane: String,
    pub session: String,
    pub pid: i64,
    pub cwd: String,
}

impl Pane {
    pub fn to_json(&self) -> Value {
        json!({ "pane": self.pane, "session": self.session, "pid": self.pid, "cwd": self.cwd })
    }
}

/// Разделитель полей — таб: ни id, ни имя сессии, ни pid его не содержат,
/// а путь идёт последним полем и потому может содержать что угодно.
const PANE_FORMAT: &str = "#{pane_id}\t#{session_name}\t#{pane_pid}\t#{pane_current_path}";

pub async fn list_panes() -> Result<Vec<Pane>, String> {
    let out = tmux_j(&["list-panes", "-a", "-F", PANE_FORMAT]).await?;
    Ok(parse_panes(&out))
}

fn parse_panes(out: &str) -> Vec<Pane> {
    out.lines()
        .filter_map(|line| {
            let mut it = line.splitn(4, '\t');
            let pane = it.next()?.trim();
            if pane.is_empty() {
                return None;
            }
            Some(Pane {
                pane: pane.to_string(),
                session: it.next().unwrap_or("").trim().to_string(),
                pid: it.next().unwrap_or("").trim().parse::<i64>().unwrap_or(0),
                cwd: it.next().unwrap_or("").trim().to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_names_are_unique_and_tmux_safe() {
        let a = unique_buffer_name("reply");
        let b = unique_buffer_name("reply");
        assert_ne!(a, b);
        assert!(a.starts_with("jarvis-node-reply-"));
        for name in [a, b] {
            assert!(name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
        }
    }

    // Расхождение с демоном хотя бы во флаге означало бы, что удалённая пана
    // ведёт себя иначе локальной; сверяем аргументы дословно с src/tmux.rs.
    #[test]
    fn paste_args_match_the_daemon_byte_for_byte() {
        assert_eq!(
            set_buffer_args("jarvis-node-reply-42", "строка\nвторая"),
            ["set-buffer", "-b", "jarvis-node-reply-42", "--", "строка\nвторая"]
        );
        assert_eq!(
            paste_buffer_args("jarvis-node-reply-42", "%7"),
            ["paste-buffer", "-p", "-d", "-b", "jarvis-node-reply-42", "-t", "%7"]
        );
    }

    #[test]
    fn confirm_is_detected_only_near_the_bottom() {
        assert!(needs_confirm("bla\nSwitch model?\n> "));
        assert!(needs_confirm("press ENTER TO SELECT"));
        // 20 строк мусора после маркера — это уже история, а не живой запрос
        let stale = format!("Switch model?\n{}", "x\n".repeat(20));
        assert!(!needs_confirm(&stale));
        assert!(!needs_confirm("обычный вывод агента"));
    }

    #[test]
    fn panes_parse_keeps_paths_with_spaces() {
        let panes = parse_panes("%1\tjarvis\t4242\t/home/me/my project\n\n%2\tdev\tx\t/srv\n");
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].pane, "%1");
        assert_eq!(panes[0].session, "jarvis");
        assert_eq!(panes[0].pid, 4242);
        assert_eq!(panes[0].cwd, "/home/me/my project");
        assert_eq!(panes[1].pid, 0, "нечисловой pid не должен ронять разбор");
    }

    #[test]
    fn parse_keys_accepts_named_and_text_steps() {
        let plan = parse_keys(&json!([{ "key": "Down" }, { "text": "свой ответ" }])).unwrap();
        assert_eq!(plan.len(), 2);
        assert!(matches!(&plan[0], Key::Named(k) if k == "Down"));
        assert!(matches!(&plan[1], Key::Text(t) if t == "свой ответ"));
    }

    #[test]
    fn parse_keys_rejects_unknown_step() {
        // шаг непонятной формы валит весь план: проиграть его наполовину —
        // значит оставить пикер в произвольном состоянии
        assert!(parse_keys(&json!([{ "key": "Down" }, { "wat": 1 }])).is_none());
        assert!(parse_keys(&json!("Down")).is_none());
    }

    #[test]
    fn session_name_is_readable_and_unique() {
        let a = session_name("/home/bob/my proj/", None);
        assert!(a.starts_with("my-proj-"), "{a}");
        // tmux не прощает совпадения имён: у второй сессии проекта должно
        // получиться другое имя, иначе она просто не создастся
        assert!(session_name("/srv/x", Some("явное имя")).starts_with("явное-имя-"));
        assert!(session_name("/", None).starts_with("project-"));
    }

    #[test]
    fn trust_prompt_is_recognised_but_not_guessed() {
        let real = "Quick safety check: Is this a project you created or one you trust?\n                    Claude Code'll be able to read, edit, and execute files here.\n                    > 1. Yes, I trust this folder\n  2. No, exit\n Enter to confirm";
        assert!(needs_trust(real));
        // «trust» в обычном выводе — не повод жать Enter вслепую
        assert!(!needs_trust("Я не стал бы trust этому коду, надо проверить"));
        assert!(!needs_trust("Do you trust the output of this tool?"));
        assert!(!needs_trust(""));
    }

    #[test]
    fn launch_command_carries_agent_paths() {
        let out = with_agent_path("claude --dangerously-skip-permissions");
        assert!(out.starts_with("export PATH="), "PATH дополняем ДО запуска");
        assert!(out.contains("$HOME/.local/bin"), "нативный установщик Claude Code кладёт сюда");
        assert!(out.contains(":$PATH\""), "прежний PATH обязан сохраниться");
        assert!(out.ends_with("claude --dangerously-skip-permissions"));
    }
}
