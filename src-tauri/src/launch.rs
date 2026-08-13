//! Запуск новой/возобновляемой сессии прямо из Jarvis: открыть терминал из
//! настроек, по желанию выполнить прокси-команду, затем `claude`/`codex` в
//! директории проекта. Заменяет прежнее «скопировать команду» вкладки «Проекты».
//!
//! macOS-only в текущей итерации. `custom`-терминал (sh -lc по шаблону) — точка
//! расширения под Ghostty/Warp/kitty сейчас и под Windows/Linux в будущем.

use crate::util::shell_quote;
use std::path::PathBuf;
use std::process::Stdio;

/// Команда агента: новая сессия или `--resume`/`resume`, с dangerous-флагами при
/// включённом «опасном режиме». Флаги сверены с `resumeCommand` в renderer.js:
/// claude → `--dangerously-skip-permissions`, codex → `--dangerously-bypass-approvals-and-sandbox`.
pub fn agent_command(agent: &str, session_id: Option<&str>, dangerous: bool) -> String {
    let mode = if dangerous { Mode::Yolo } else { Mode::Ask };
    agent_command_mode(agent, session_id, mode)
}

/// С чем агент стартует: сколько ему позволено без вопросов.
///
/// Раньше выбор был двоичным — «спрашивает» или «делает всё молча», и жил он в
/// настройках на все задачи разом. Но режим — свойство ЗАДАЧИ: разведать чужой
/// код и переписать свой требуют разного доверия. Отсюда третий режим — план:
/// агент разбирается и предлагает, не трогая файлов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Спросит перед каждым действием — как по умолчанию у самого агента.
    Ask,
    /// Только разведка и план: правок не будет.
    Plan,
    /// Ничего не спрашивает. Для песочницы и рутины.
    Yolo,
}

impl Mode {
    /// Из строки панели. Неизвестное — самый осторожный режим: молча дать
    /// агенту больше прав, чем просили, нельзя.
    pub fn parse(s: &str) -> Mode {
        match s.trim() {
            "plan" => Mode::Plan,
            "yolo" => Mode::Yolo,
            _ => Mode::Ask,
        }
    }
}

/// Команда агента с учётом режима.
pub fn agent_command_mode(agent: &str, session_id: Option<&str>, mode: Mode) -> String {
    if agent == "codex" {
        // У codex режима плана нет; просить его «только посмотреть» словами —
        // не гарантия, поэтому не притворяемся: план = обычный режим с
        // вопросами.
        let flag = match mode {
            Mode::Yolo => " --dangerously-bypass-approvals-and-sandbox",
            _ => "",
        };
        match session_id {
            Some(id) => format!("codex resume {id}{flag}"),
            None => format!("codex{flag}"),
        }
    } else {
        let flag = match mode {
            Mode::Yolo => " --dangerously-skip-permissions",
            Mode::Plan => " --permission-mode plan",
            Mode::Ask => "",
        };
        match session_id {
            Some(id) => format!("claude --resume {id}{flag}"),
            None => format!("claude{flag}"),
        }
    }
}

/// Каталоги, которые надо явно добавить в PATH запускаемой команды.
///
/// Терминал выполняет нашу строку в НЕинтерактивном шелле, а PATH-блок Jarvis
/// живёт в `~/.zshrc` / `~/.bashrc`, которые читают только интерактивные. Без
/// этого `claude` резолвится в настоящий бинарь мимо шима — сессия поднимается
/// вне tmux, и Jarvis не может ни ответить в неё, ни рулить пультом. Ровно тот
/// баг, ради которого функция и появилась.
///
/// Тем же махом кладём привычные места установки: шим ищет `tmux` через
/// `command -v`, а в урезанном PATH Homebrew-tmux не находится и обёртка молча
/// отключается.
pub fn launch_path_dirs() -> Vec<PathBuf> {
    let shims = crate::util::jarvis_dir().join("shims");
    let mut dirs = Vec::new();
    // шим — строго первым, иначе он же и не сработает
    if shims.is_dir() {
        dirs.push(shims);
    }
    for extra in [
        crate::util::home_dir().join(".local/bin"), // Linux: npm -g, pipx
        PathBuf::from("/opt/homebrew/bin"),         // macOS: Apple Silicon
        PathBuf::from("/usr/local/bin"),
    ] {
        if extra.is_dir() && !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    dirs
}

/// `export PATH=…` для команды терминала. `None`, если добавлять нечего.
fn path_prefix(dirs: &[PathBuf]) -> Option<String> {
    if dirs.is_empty() {
        return None;
    }
    let joined = dirs
        .iter()
        .map(|d| shell_quote(&d.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(":");
    Some(format!("export PATH={joined}:\"$PATH\""))
}

/// Полная команда для терминала:
/// `export PATH=… && [<proxy> && ][cd '<cwd>' && ]<agent_cmd>`.
///
/// Пустой `cwd` допустим (сессии без известной директории, группа «другое»):
/// тогда `cd` опускается — как в прежнем «скопировать команду».
pub fn inner_command(cwd: &str, proxy_cmd: &str, agent_cmd: &str, path_dirs: &[PathBuf]) -> String {
    let mut parts: Vec<String> = Vec::new();
    // PATH — первым: и прокси-команда, и агент должны видеть шим.
    if let Some(p) = path_prefix(path_dirs) {
        parts.push(p);
    }
    let proxy = proxy_cmd.trim();
    if !proxy.is_empty() {
        parts.push(proxy.to_string());
    }
    if !cwd.trim().is_empty() {
        parts.push(format!("cd {}", shell_quote(cwd)));
    }
    parts.push(agent_cmd.to_string());
    parts.join(" && ")
}

/// Есть ли docker на этой машине. Спрашиваем перед запуском: «не нашёлся
/// docker» человек чинит за минуту, а молчаливо не открывшийся терминал — нет.
pub async fn has_docker() -> bool {
    tokio::process::Command::new("docker")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Обернуть команду агента в контейнер.
///
/// Вторая изоляция из Air: worktree разводит ФАЙЛЫ, контейнер — инструменты и
/// зависимости. Задача трогает свой node_modules и свой питон, а не общие.
///
/// Три вещи, без которых контейнер был бы бесполезен, и потому они не
/// настройки, а часть команды:
///
/// * каталог проекта монтируется ПО ТОМУ ЖЕ пути. Пути из хуков (cwd сессии,
///   файлы ходов) уезжают в панель как есть; переименуй мы каталог внутри — и
///   ни один файл из чата не открылся бы;
/// * `~/.jarvis` — там сокет демона и шимы: через него сессия внутри
///   контейнера вообще становится видимой панели. Без него агент отработает
///   молча и в списке не появится;
/// * `~/.claude` — авторизация и настройки агента; без них он попросит логин в
///   контейнере, где его некому дать.
///
/// `TMUX_PANE` пробрасываем переменной: пану держит хост (docker запускается
/// внутри неё), а хук внутри контейнера читает её из окружения — иначе панель
/// не сможет ни ответить в сессию, ни нажать клавишу.
pub fn docker_command(image: &str, cwd: &str, home: &str, agent_cmd: &str) -> String {
    let mounts = [
        format!("-v {}:{}", shell_quote(cwd), shell_quote(cwd)),
        format!("-v {}/.jarvis:{}/.jarvis", shell_quote(home), shell_quote(home)),
        format!("-v {}/.claude:{}/.claude", shell_quote(home), shell_quote(home)),
    ]
    .join(" ");
    format!(
        "docker run --rm -it {mounts} -w {cwd} -e TMUX_PANE -e JARVIS_DIR {image} \
         bash -lc {inner}",
        cwd = shell_quote(cwd),
        image = shell_quote(image),
        inner = shell_quote(agent_cmd),
    )
}

/// Экранирование под двойные кавычки AppleScript-строки: `\`, `"` и переводы
/// строк (сырой `\n` внутри "…" — синтаксическая ошибка osascript).
/// Одинарные кавычки (из shell_quote) внутри неё безопасны.
fn applescript_escape(s: &str) -> String {
    s.replace('\\', r"\\").replace('"', "\\\"").replace('\n', r"\n").replace('\r', r"\r")
}

async fn osascript(args: &[String]) -> Result<(), String> {
    let out = tokio::process::Command::new("osascript")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("не удалось запустить osascript: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr);
        Err(if msg.trim().is_empty() { "терминал не открылся".into() } else { msg.trim().to_string() })
    }
}

/// Открыть терминал из настроек и выполнить в нём `inner`.
pub async fn spawn(terminal: &str, custom_cmd: &str, inner: &str) -> Result<(), String> {
    match terminal {
        "iterm2" => {
            let esc = applescript_escape(inner);
            // Создаём окно с дефолт-профилем и пишем команду в его сессию.
            osascript(&[
                "-e".into(), "tell application \"iTerm2\"".into(),
                "-e".into(), "set w to (create window with default profile)".into(),
                "-e".into(), format!("tell current session of w to write text \"{esc}\""),
                "-e".into(), "activate".into(),
                "-e".into(), "end tell".into(),
            ])
            .await
        }
        "custom" => {
            let tmpl = custom_cmd.trim();
            if tmpl.is_empty() {
                return Err("шаблон команды терминала пуст (настройки → Запуск)".into());
            }
            if !tmpl.contains("{cmd}") {
                return Err("в шаблоне нет плейсхолдера {cmd}".into());
            }
            let expanded = tmpl.replace("{cmd}", &shell_quote(inner));
            let mut child = tokio::process::Command::new("sh")
                .arg("-lc")
                .arg(&expanded)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("не удалось запустить терминал: {e}"))?;
            // Терминал живёт своей жизнью — завершения не ждём. Но мгновенную
            // смерть (опечатка в бинарнике → exit 127) ловим, иначе юзер видит
            // «Запускаю…» при полностью нерабочем шаблоне.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            match child.try_wait() {
                Ok(Some(status)) if !status.success() => Err(format!(
                    "команда терминала сразу завершилась ({status}) — проверь шаблон в настройках «Запуск»"
                )),
                _ => Ok(()),
            }
        }
        // 'terminal-app' и любое неизвестное значение → системный Terminal.app.
        _ => {
            let esc = applescript_escape(inner);
            osascript(&[
                "-e".into(), "tell application \"Terminal\"".into(),
                "-e".into(), format!("do script \"{esc}\""),
                "-e".into(), "activate".into(),
                "-e".into(), "end tell".into(),
            ])
            .await
        }
    }
}

#[cfg(test)]
mod tests {

    /// Контейнер обязан монтировать проект ПО ТОМУ ЖЕ пути: пути из хуков
    /// уезжают в панель как есть, и переименование каталога внутри оставило бы
    /// её с файлами, которых «нет».
    #[test]
    fn docker_keeps_the_project_path_and_carries_the_socket() {
        let cmd = docker_command("jarvis/agent", "/Users/bob/proj", "/Users/bob", "claude");
        assert!(cmd.contains("-v '/Users/bob/proj':'/Users/bob/proj'"), "{cmd}");
        // Сокет демона и авторизация агента — иначе сессия не появится в
        // панели, а агент попросит логин там, где его некому дать.
        assert!(cmd.contains("/.jarvis:"), "{cmd}");
        assert!(cmd.contains("/.claude:"), "{cmd}");
        // Пану держит хост, внутрь её отдаём переменной.
        assert!(cmd.contains("-e TMUX_PANE"), "{cmd}");
        assert!(cmd.contains("-w '/Users/bob/proj'"), "{cmd}");
        assert!(cmd.contains("'jarvis/agent'"), "{cmd}");
    }

    /// Команда агента уезжает внутрь строкой — кавычки обязаны её удержать.
    #[test]
    fn the_agent_command_is_quoted_whole() {
        let cmd = docker_command("img", "/p", "/h", "claude --resume abc --dangerously-skip-permissions");
        assert!(
            cmd.contains("'claude --resume abc --dangerously-skip-permissions'"),
            "{cmd}"
        );
    }

    /// Режим — свойство задачи, и «непонятное» обязано быть самым осторожным:
    /// молча дать агенту больше прав, чем просили, нельзя.
    #[test]
    fn unknown_mode_is_the_careful_one() {
        assert_eq!(Mode::parse("plan"), Mode::Plan);
        assert_eq!(Mode::parse("yolo"), Mode::Yolo);
        assert_eq!(Mode::parse(""), Mode::Ask);
        assert_eq!(Mode::parse("что-то новое"), Mode::Ask);
    }

    #[test]
    fn mode_shapes_the_command() {
        assert_eq!(agent_command_mode("claude", None, Mode::Plan), "claude --permission-mode plan");
        assert_eq!(
            agent_command_mode("claude", Some("abc"), Mode::Yolo),
            "claude --resume abc --dangerously-skip-permissions"
        );
        assert_eq!(agent_command_mode("claude", None, Mode::Ask), "claude");
        // У codex плана нет — притворяться не будем.
        assert_eq!(agent_command_mode("codex", None, Mode::Plan), "codex");
        assert_eq!(
            agent_command_mode("codex", None, Mode::Yolo),
            "codex --dangerously-bypass-approvals-and-sandbox"
        );
    }
    use super::*;

    #[test]
    fn agent_command_variants() {
        assert_eq!(agent_command("claude", None, false), "claude");
        assert_eq!(agent_command("claude", None, true), "claude --dangerously-skip-permissions");
        assert_eq!(agent_command("claude", Some("abc"), true), "claude --resume abc --dangerously-skip-permissions");
        assert_eq!(agent_command("codex", None, false), "codex");
        assert_eq!(agent_command("codex", None, true), "codex --dangerously-bypass-approvals-and-sandbox");
        assert_eq!(agent_command("codex", Some("x1"), false), "codex resume x1");
    }

    #[test]
    fn inner_command_with_and_without_proxy() {
        assert_eq!(inner_command("/tmp/p", "", "claude", &[]), "cd '/tmp/p' && claude");
        assert_eq!(
            inner_command("/tmp/p", "export X=1", "claude", &[]),
            "export X=1 && cd '/tmp/p' && claude"
        );
    }

    #[test]
    fn inner_command_without_cwd_skips_cd() {
        assert_eq!(inner_command("", "", "claude --resume abc", &[]), "claude --resume abc");
        assert_eq!(
            inner_command("  ", "export X=1", "codex resume x1", &[]),
            "export X=1 && codex resume x1"
        );
    }

    /// Главное в этой правке: PATH идёт ПЕРВЫМ, до прокси и до cd. Иначе шим
    /// не подхватится и сессия поднимется вне tmux — тот самый баг.
    #[test]
    fn inner_command_puts_path_first() {
        let dirs = vec![PathBuf::from("/home/u/.jarvis/shims"), PathBuf::from("/opt/homebrew/bin")];
        let expected = concat!(
            "export PATH='/home/u/.jarvis/shims':'/opt/homebrew/bin':\"$PATH\"",
            " && export X=1 && cd '/tmp/p' && claude",
        );
        assert_eq!(inner_command("/tmp/p", "export X=1", "claude", &dirs), expected);
    }

    #[test]
    fn path_prefix_quotes_and_keeps_existing_path() {
        let p = path_prefix(&[PathBuf::from("/a b/shims")]).unwrap();
        assert_eq!(p, "export PATH='/a b/shims':\"$PATH\"");
        assert!(path_prefix(&[]).is_none());
    }

    #[test]
    fn applescript_escape_quotes_backslash_and_newlines() {
        assert_eq!(applescript_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(applescript_escape("a\nb\rc"), r"a\nb\rc");
    }
}
