//! Запуск новой/возобновляемой сессии прямо из Jarvis: открыть терминал из
//! настроек, по желанию выполнить прокси-команду, затем `claude`/`codex` в
//! директории проекта. Заменяет прежнее «скопировать команду» вкладки «Проекты».
//!
//! Терминал выбирается настройкой: на macOS это Terminal.app/iTerm2 через
//! AppleScript, на Linux — эмулятор из списка (или системный
//! `x-terminal-emulator`). `custom` (sh -lc по шаблону) работает везде и
//! остаётся точкой расширения под что угодно.

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
    } else if agent == "kimi" {
        // У Kimi два «опасных» флага, и это не синонимы: `--yolo` авто-апрувит
        // обычные вызовы инструментов, но вопросы агент задавать может, а
        // `--auto` снимает и их. Берём `--yolo` — это ровный аналог
        // `--dangerously-skip-permissions` у Claude; `--auto` заодно отключил бы
        // пикер вопросов, которым Jarvis и рулит из панели.
        // Режим плана у Kimi настоящий (`--plan`), притворяться не нужно.
        let flag = match mode {
            Mode::Yolo => " --yolo",
            Mode::Plan => " --plan",
            Mode::Ask => "",
        };
        // Возобновление — `-S <id>`: `sid` уже вида `session_<uuid>`, и это тот
        // же флаг, что отдаёт `resume_cmd` бэкенда.
        match session_id {
            Some(id) => format!("kimi -S {id}{flag}"),
            None => format!("kimi{flag}"),
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

/// Убрать с дороги то, на чём агент встанет колом ещё до первого хука.
///
/// Пока такой камень один: Kimi Code в незнакомом каталоге спрашивает «Trust
/// this folder?» и ждёт клавишу. У панели человек нажал бы сам, а
/// `sessions.spawn` поднимает сессию без зрителей — вопрос висит, хук старта не
/// приходит, талон снимается по таймауту, первый промпт уезжает в никуда.
///
/// Зовётся ПОСЛЕ того, как рабочий каталог окончательно известен (worktree
/// песочницы создаётся по дороге) и до открытия терминала. Только для локальных
/// запусков: у удалённого узла свой дом Kimi, наша отметка туда не относится.
/// Не смогли — не отказываем в запуске, а говорим вслух: сессия ещё может
/// подняться, если каталог уже доверен.
pub fn prepare_workspace(agent: &str, cwd: &str) {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        return;
    }
    let res = match agent {
        "kimi" => crate::backend::kimi::ensure_workspace_trust(std::path::Path::new(cwd)),
        // У claude ровно тот же камень, проверено вживую: в незнакомом каталоге
        // он спрашивает «Is this a project you trust?» и ждёт клавишу. Флаг
        // пропуска разрешений его НЕ снимает. Бьёт по isolate: worktree — всегда
        // новый каталог, то есть по самому частому случаю подъёма агентом.
        "claude" => crate::claude_bin::ensure_workspace_trust(std::path::Path::new(cwd)),
        _ => return, // codex на этой машине не установлен — не гадаем
    };
    if let Err(e) = res {
        crate::log::line(&format!("launch: не пометил {cwd} доверенным для {agent}: {e}"));
    }
}

/// Почему сессия могла не появиться за отведённое время. `None` — причин не
/// знаем; тогда молчим о причине, как раньше.
///
/// Единственная известная — незакрытый вопрос о доверии: `prepare_workspace`
/// не смог записать отметку (нет прав, чужой `KIMI_CODE_HOME`), и kimi встал на
/// вопросе. Человеку это чинится одним нажатием, но только если он знает.
pub fn stall_hint(agent: &str, cwd: &str) -> Option<String> {
    let cwd = cwd.trim();
    if agent != "kimi" || cwd.is_empty() {
        return None;
    }
    (!crate::backend::kimi::workspace_trusted(std::path::Path::new(cwd)))
        .then(|| format!("kimi ждёт подтверждения доверия к каталогу {cwd} — открой окно и подтверди"))
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

/// Запустить команду в фоне через `sh -lc`, не дожидаясь завершения терминала.
/// Мгновенную смерть (опечатка в бинарнике → exit 127) ловим, иначе юзер видит
/// «Запускаю…» при полностью нерабочем шаблоне.
async fn spawn_detached(shell_cmd: &str) -> Result<(), String> {
    let mut child = tokio::process::Command::new("sh")
        .arg("-lc")
        .arg(shell_cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("не удалось запустить терминал: {e}"))?;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    match child.try_wait() {
        Ok(Some(status)) if !status.success() => Err(format!(
            "команда терминала сразу завершилась ({status}) — проверь настройки «Запуск»"
        )),
        _ => Ok(()),
    }
}

/// Открыть терминал из настроек и выполнить в нём `inner`.
pub async fn spawn(terminal: &str, custom_cmd: &str, inner: &str) -> Result<(), String> {
    if terminal == "custom" {
        let tmpl = custom_cmd.trim();
        if tmpl.is_empty() {
            return Err("шаблон команды терминала пуст (настройки → Запуск)".into());
        }
        if !tmpl.contains("{cmd}") {
            return Err("в шаблоне нет плейсхолдера {cmd}".into());
        }
        return spawn_detached(&tmpl.replace("{cmd}", &shell_quote(inner))).await;
    }
    imp::spawn(terminal, inner).await
}

/* ================= macOS: Terminal.app / iTerm2 ================= */

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    /// Экранирование под двойные кавычки AppleScript-строки: `\`, `"` и переводы
    /// строк (сырой `\n` внутри "…" — синтаксическая ошибка osascript).
    /// Одинарные кавычки (из shell_quote) внутри неё безопасны.
    pub(super) fn applescript_escape(s: &str) -> String {
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

    pub(super) async fn spawn(terminal: &str, inner: &str) -> Result<(), String> {
        let esc = applescript_escape(inner);
        if terminal == "iterm2" {
            // Создаём окно с дефолт-профилем и пишем команду в его сессию.
            osascript(&[
                "-e".into(), "tell application \"iTerm2\"".into(),
                "-e".into(), "set w to (create window with default profile)".into(),
                "-e".into(), format!("tell current session of w to write text \"{esc}\""),
                "-e".into(), "activate".into(),
                "-e".into(), "end tell".into(),
            ])
            .await
        } else {
            // 'terminal-app' и любое неизвестное значение → системный Terminal.app.
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

/* ================= Linux: эмуляторы терминала ================= */

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    /// Кандидаты в порядке предпочтения. `x-terminal-emulator` — альтернатива
    /// Debian/Ubuntu: указывает на терминал по умолчанию, поэтому идёт первой.
    const CANDIDATES: &[&str] = &[
        "x-terminal-emulator", "gnome-terminal", "konsole", "ptyxis", "xfce4-terminal",
        "kitty", "alacritty", "wezterm", "foot", "tilix", "terminator", "mate-terminal", "xterm",
    ];

    /// Как передать эмулятору программу для запуска.
    ///
    /// Единственное, чем терминалы тут расходятся, — это флаг: `-e`, `--`, либо
    /// вообще ничего. Различий в кавычках нет, потому что программа всегда одна
    /// и та же — путь к сгенерированному скрипту, без аргументов. Это и есть
    /// причина писать скрипт во временный файл: иначе пришлось бы угадывать,
    /// какой из тринадцати эмуляторов ждёт строку, а какой argv.
    fn launch_flag(term: &str) -> &'static [&'static str] {
        match term {
            "gnome-terminal" | "ptyxis" | "mate-terminal" => &["--"],
            "kitty" | "foot" => &[],
            "wezterm" => &["start", "--"],
            _ => &["-e"], // konsole, xfce4-terminal, alacritty, tilix, terminator, xterm, x-terminal-emulator
        }
    }

    fn exists(bin: &str) -> bool {
        std::process::Command::new("sh")
            .arg("-lc")
            .arg(format!("command -v {}", shell_quote(bin)))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Каталог под скрипты запуска; заодно подчищаем прошлые старше часа.
    /// Скрипт не удаляет себя сам: `exec "$SHELL"` в конце заменяет процесс,
    /// и никакой `trap EXIT` уже не сработает.
    fn launch_dir() -> std::path::PathBuf {
        let dir = crate::util::jarvis_dir().join("launch");
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let hour = std::time::Duration::from_secs(3600);
            for e in rd.flatten() {
                let stale = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|t| t.elapsed().map(|d| d > hour).unwrap_or(false))
                    .unwrap_or(false);
                if stale {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        dir
    }

    /// Записать команду отдельным исполняемым скриптом и вернуть путь к нему.
    fn write_script(inner: &str) -> Result<std::path::PathBuf, String> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = launch_dir().join(format!("run-{}-{}.sh", std::process::id(), nanos));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o700)
            .open(&path)
            .map_err(|e| format!("не смог подготовить команду запуска: {e}"))?;
        // после агента оставляем живую оболочку — иначе окно схлопнется мгновенно
        write!(f, "#!/bin/sh\n{inner}\nexec \"$SHELL\"\n")
            .map_err(|e| format!("не смог записать команду запуска: {e}"))?;
        Ok(path)
    }

    pub(super) async fn spawn(terminal: &str, inner: &str) -> Result<(), String> {
        // Значения из macOS-настроек на Linux ничего не значат — ищем сами.
        let explicit = match terminal {
            "terminal-app" | "iterm2" | "" => None,
            other => Some(other),
        };
        let term = match explicit {
            Some(t) if exists(t) => t.to_string(),
            Some(t) => {
                return Err(format!(
                    "терминал «{t}» не найден в PATH — выбери другой в настройках «Запуск»"
                ))
            }
            None => CANDIDATES
                .iter()
                .find(|c| exists(c))
                .map(|c| (*c).to_string())
                .ok_or_else(|| {
                    "не нашёл эмулятор терминала (пробовал gnome-terminal, konsole, kitty, \
                     alacritty, xterm и другие). Поставь любой или задай свой шаблон \
                     в настройках «Запуск»"
                        .to_string()
                })?,
        };

        let script = write_script(inner)?;
        let mut argv = vec![shell_quote(&term)];
        argv.extend(launch_flag(&term).iter().map(|f| (*f).to_string()));
        argv.push(shell_quote(&script.to_string_lossy()));
        spawn_detached(&argv.join(" ")).await
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn flags_match_terminal_conventions() {
            assert_eq!(launch_flag("gnome-terminal"), &["--"]);
            assert_eq!(launch_flag("konsole"), &["-e"]);
            assert_eq!(launch_flag("wezterm"), &["start", "--"]);
            assert!(launch_flag("kitty").is_empty());
            // незнакомый эмулятор — самый распространённый флаг
            assert_eq!(launch_flag("something-new"), &["-e"]);
        }

        #[test]
        fn script_is_executable_and_keeps_shell_alive() {
            let p = write_script("cd '/tmp' && claude").expect("скрипт пишется");
            let body = std::fs::read_to_string(&p).unwrap();
            assert!(body.starts_with("#!/bin/sh"));
            assert!(body.contains("cd '/tmp' && claude"));
            assert!(body.trim_end().ends_with(r#"exec "$SHELL""#));
            let _ = std::fs::remove_file(p);
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
        // А у Kimi план настоящий — и `--yolo`, а не `--auto`: второй снял бы и
        // вопросы, которыми Jarvis рулит из панели.
        assert_eq!(agent_command_mode("kimi", None, Mode::Ask), "kimi");
        assert_eq!(agent_command_mode("kimi", None, Mode::Plan), "kimi --plan");
        assert_eq!(agent_command_mode("kimi", None, Mode::Yolo), "kimi --yolo");
        assert_eq!(
            agent_command_mode("kimi", Some("session_abc"), Mode::Yolo),
            "kimi -S session_abc --yolo"
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
        assert_eq!(agent_command("kimi", None, false), "kimi");
        assert_eq!(agent_command("kimi", None, true), "kimi --yolo");
        assert_eq!(agent_command("kimi", Some("session_x"), false), "kimi -S session_x");
    }

    /// Команда возобновления обязана совпадать с той, что бэкенд отдаёт в панель
    /// «скопировать»: разойдись они — человек скопировал бы нерабочую строку.
    #[test]
    fn resume_matches_the_backend_for_every_agent() {
        for a in crate::backend::Agent::all() {
            let want = crate::backend::backend(*a).resume_cmd("sid-1");
            assert_eq!(agent_command(a.label(), Some("sid-1"), false), want, "{}", a.label());
        }
    }

    /// Подготовка и подсказка — только про kimi и только при известном каталоге:
    /// у claude и codex своя механика доверия, чужой отметки мы им не ставим.
    #[test]
    fn workspace_prep_and_hint_are_kimi_only() {
        for agent in ["claude", "codex"] {
            assert_eq!(stall_hint(agent, "/tmp/nowhere-at-all"), None, "{agent}");
            prepare_workspace(agent, "/tmp/nowhere-at-all"); // ничего не пишет и не паникует
        }
        assert_eq!(stall_hint("kimi", "   "), None, "без каталога причины не выдумываем");
        // недоверенный каталог у kimi — причина названа вслух, а не «просто не встал»
        let hint = stall_hint("kimi", "/tmp/jarvis-kimi-never-trusted").unwrap_or_default();
        assert!(hint.contains("доверия"), "{hint}");
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

    #[cfg(target_os = "macos")]
    #[test]
    fn trust_is_prepared_for_both_cli_and_is_actually_called() {
        // Оба CLI в незнакомом каталоге ждут клавишу — и оба били по isolate,
        // где worktree всегда новый. Проверено вживую на kimi 0.38 и claude 2.1.233.
        let me = include_str!("launch.rs");
        let src = &me[..me.find("#[cfg(test)]").expect("тесты на месте")];
        assert!(src.contains("\"kimi\" =>"), "ветка kimi ушла из подготовки каталога");
        assert!(src.contains("\"claude\" =>"), "ветка claude ушла из подготовки каталога");
        // и её кто-то зовёт: молчаливое зависание возвращается ровно так
        assert!(include_str!("ipc.rs").contains("launch::prepare_workspace("),
            "подготовку каталога перестали звать перед запуском");
    }

    #[test]
    fn applescript_escape_quotes_backslash_and_newlines() {
        assert_eq!(imp::applescript_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(imp::applescript_escape("a\nb\rc"), r"a\nb\rc");
    }
}
