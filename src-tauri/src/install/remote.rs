//! Установка узла Jarvis на удалённую машину: `jarvis-setup remote add|status`.
//!
//! Дизайн: docs/superpowers/specs/2026-08-05-remote-agents-design.md,
//! руководство: docs/remote.md.
//!
//! Всё делается чужими руками — `ssh`, `sh` и `systemctl` на той стороне; своих
//! сетевых библиотек здесь нет и быть не должно. Аутентификация целиком ssh-шная
//! (ключи, `~/.ssh/config`, агент-форвардинг), поэтому установщик не заводит и не
//! хранит ни одного секрета — ровно тот же инвариант, что у `crate::remote`.
//!
//! Что появляется на той стороне:
//!
//! | путь | что это |
//! | --- | --- |
//! | `<dir>/bin/jarvis-node` | сам узел; слушает `<dir>/node.sock` (0600) |
//! | `<dir>/bin/jarvis-hook` | тот же шим, что локально, но стучится в `node.sock` |
//! | `~/.claude/settings.json` | хуки claude — той же формы, что ставит локальная установка |
//! | `~/.codex/hooks.json` | хуки codex, если codex там есть |
//! | `~/.config/systemd/user/jarvis-node.service` | автозапуск, `Restart=always` |
//!
//! Порядок шагов не случаен: связь → окружение → бинарь → хуки → автозапуск →
//! проверка. Хуки без узла бессмысленны, а автозапуск без бинаря — это юнит,
//! который вечно перезапускает несуществующий файл.

use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{Progress, Step};
#[path = "remote_connection.rs"]
pub mod connection;
pub use connection::Connection;

mod bundled_nodes {
    const X86: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/jarvis-node-x86_64-unknown-linux-musl.bin"));
    const ARM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/jarvis-node-aarch64-unknown-linux-musl.bin"));
    pub const BINARIES: &[(&str, &[u8])] = if X86.is_empty() || ARM.is_empty() { &[] } else {
        &[("x86_64-unknown-linux-musl", X86), ("aarch64-unknown-linux-musl", ARM)]
    };
}

/// Каталог Jarvis на той стороне по умолчанию — тот же, что в настройках ноута
/// (`crate::remote::DEFAULT_REMOTE_DIR`) и в форме вкладки «Удалённые».
const DEFAULT_DIR: &str = "~/.jarvis";

/// Имя юнита автозапуска. Совпадает с именем бинаря: искать его на чужой машине
/// человек будет именно так.
const UNIT: &str = "jarvis-node.service";

const PHASE_LINK: &str = "Связь";
const PHASE_ENV: &str = "Окружение";
const PHASE_NODE: &str = "Узел";
const PHASE_HOOKS: &str = "Хуки";
const PHASE_BOOT: &str = "Автозапуск";
const PHASE_CHECK: &str = "Проверка";
const PHASE_DONE: &str = "Готово";

/* ================= ssh: единственный транспорт установщика ================= */

/// `ssh` с общими опциями. `BatchMode=yes` — чтобы ssh не залипал на промпте
/// пароля/пассфразы посреди установки: молчаливое ожидание неотличимо от
/// зависания, а нам нужен внятный текст про ключи. Отпечаток хоста НЕ принимаем
/// автоматически: доверие к новой машине — решение человека, а не установщика.
fn ssh_cmd(host: &Connection) -> Result<Command, String> { host.command() }

/// Terminal output is displayed as plain text in the setup UI. Drop escape
/// sequences (including OSC hyperlinks/titles) and cap both scanned and shown
/// data so a failing remote command cannot flood the error surface.
fn clean_remote_error(bytes: &[u8]) -> String {
    const MAX_INPUT: usize = 64 * 1024;
    const MAX_OUTPUT: usize = 4096;
    const CUT: &str = "… (вывод сокращён)";
    #[derive(Clone, Copy)]
    enum State { Text, Escape, Intermediate, Csi, String(bool), StringEscape(bool) }
    let input = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_INPUT)]);
    let mut chars = input.chars().peekable();
    let mut state = State::Text;
    let mut output = String::new();
    let mut truncated = bytes.len() > MAX_INPUT;
    while let Some(c) = chars.next() {
        let shown = match state {
            State::Text => match c {
                '\u{1b}' => { state = State::Escape; None }
                '\u{9b}' => { state = State::Csi; None }
                '\u{9d}' => { state = State::String(true); None }
                '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => { state = State::String(false); None }
                '\r' if chars.peek() == Some(&'\n') => None,
                '\r' => Some('\n'),
                '\n' | '\t' => Some(c),
                c if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') => None,
                _ => Some(c),
            },
            State::Escape => {
                state = match c {
                    '[' => State::Csi,
                    ']' => State::String(true),
                    'P' | 'X' | '^' | '_' => State::String(false),
                    ' '..='/' => State::Intermediate,
                    _ => State::Text,
                };
                None
            }
            State::Intermediate => { if ('0'..='~').contains(&c) { state = State::Text; } None }
            State::Csi => {
                if ('@'..='~').contains(&c) { state = State::Text; }
                else if c == '\u{1b}' { state = State::Escape; }
                if matches!(c, '\n' | '\r') { state = State::Text; Some('\n') } else { None }
            }
            State::String(osc) => {
                if c == '\u{9c}' || (osc && c == '\u{7}') { state = State::Text; }
                else if c == '\u{1b}' { state = State::StringEscape(osc); }
                None
            }
            State::StringEscape(osc) => { state = if c == '\\' { State::Text } else { State::String(osc) }; None }
        };
        if let Some(c) = shown {
            if output.len() + c.len_utf8() > MAX_OUTPUT - CUT.len() { truncated = true; break; }
            output.push(c);
        }
    }
    let mut output = output.trim().to_string();
    if truncated { output.push_str(CUT); }
    output
}

fn transport_label(host: &Connection) -> &'static str {
    if host.transport == "teleport" { "Teleport" } else { "SSH" }
}

fn remote_command_error(host: &Connection, code: Option<i32>, stderr: &[u8]) -> String {
    let status = code.map(|code| format!("код {code}")).unwrap_or_else(|| "без кода возврата".into());
    let details = clean_remote_error(stderr);
    let message = format!("Команда через {} завершилась с ошибкой ({status}).", transport_label(host));
    if details.is_empty() { message } else { clean_remote_error(format!("{message}\n{details}").as_bytes()) }
}

fn remote_probe_error(host: &Connection, error: &str) -> String {
    clean_remote_error(format!("Не удалось проверить окружение на {host} через {}:\n{error}", transport_label(host)).as_bytes())
}

/// Выполнить скрипт на той стороне и забрать stdout.
///
/// Скрипт уезжает ОДНИМ элементом argv — локальный шелл его не видит вовсе,
/// поэтому кавычки внутри можно ставить свободно; интерполировать чужие строки
/// всё равно только через [`sh_quote`].
fn run_ssh(host: &Connection, script: &str) -> Result<String, String> {
    let out = host.command_with_script(script)?
        .stdin(Stdio::null())
        .output()
        .map_err(|e| clean_remote_error(format!("Не удалось запустить {}: {e}", if host.transport == "teleport" { "tsh" } else { "ssh" }).as_bytes()))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    Err(remote_command_error(host, out.status.code(), &out.stderr))
}

/// Выполнить скрипт, скормив ему `data` в stdin (так заливаются файлы).
///
/// Дедлока «пишем в stdin, а ребёнок захлебнулся в своём stdout» здесь нет:
/// скрипты на том конце пишут в stdout ноль байт, а в stderr — считанные строки,
/// то есть заведомо меньше буфера трубы.
fn send_ssh(host: &Connection, script: &str, data: &[u8]) -> Result<(), String> {
    let mut child = host.command_with_script(script)?
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| clean_remote_error(format!("Не удалось запустить {}: {e}", if host.transport == "teleport" { "tsh" } else { "ssh" }).as_bytes()))?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("Команда через {} не предоставила stdin", transport_label(host)))?;
        stdin
            .write_all(data)
            .map_err(|e| clean_remote_error(format!("Не удалось передать данные команде через {}: {e}", transport_label(host)).as_bytes()))?;
        // закрываем явно (drop в конце блока): без EOF `cat` на той стороне
        // будет ждать вечно, и установка повиснет без единого сообщения
    }
    let out = child
        .wait_with_output()
        .map_err(|e| clean_remote_error(format!("Не удалось дождаться завершения команды через {}: {e}", transport_label(host)).as_bytes()))?;
    if out.status.success() {
        return Ok(());
    }
    Err(remote_command_error(host, out.status.code(), &out.stderr))
}

/// Строка → безопасный аргумент удалённого шелла. Каталог узла приходит из рук
/// человека (`--dir`), и попадать в `sh` он должен как данные, а не как код.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Разбор вывода вида `ключ=значение` построчно. Повторяющиеся ключи (`have=`)
/// собираются отдельно — см. поле `tools` у `Remote`.
fn kv(out: &str, key: &str) -> Option<String> {
    out.lines()
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.trim().to_string())
}

/* ================= что на той стороне ================= */

/// Снимок удалённой машины: один ssh-заход вместо семи.
struct Remote {
    /// Абсолютный `$HOME`: `~` в argv ssh не раскрывается, а домашний каталог
    /// чужой машины локально неизвестен — спрашиваем.
    home: String,
    os: String,
    arch: String,
    codex_home: String,
    provider_sources: Vec<Value>,
    shell: String,
    /// Что из нужного там нашлось (`tmux`, `curl`, `claude`, `codex`, …).
    tools: Vec<String>,
}

impl Remote {
    fn has(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }

    /// Развернуть `~` в каталоге узла: и `-L` у ssh, и systemd, и сам `sh`
    /// понимают только абсолютный путь.
    fn expand(&self, dir: &str) -> String {
        let home = self.home.trim_end_matches('/');
        let abs = match dir.strip_prefix('~') {
            Some(rest) => {
                let rest = rest.trim_start_matches('/');
                if rest.is_empty() {
                    home.to_string()
                } else {
                    format!("{home}/{rest}")
                }
            }
            None => dir.to_string(),
        };
        abs.trim_end_matches('/').to_string()
    }
}

/// Разведка: кто там живёт и чем богат. `command -v` под неинтерактивным ssh
/// видит урезанный PATH (без nvm/homebrew), поэтому отсутствие агента здесь —
/// это «не нашёл», а не «не установлен»; отсюда каталог `~/.codex` вторым
/// признаком и предупреждения вместо отказа.
const PROBE: &str = r##"printf 'home=%s\n' "$HOME"
PATH="$PATH:/opt/homebrew/bin:/usr/local/bin"
export PATH
printf 'os=%s\n' "$(uname -s 2>/dev/null)"
printf 'arch=%s\n' "$(uname -m 2>/dev/null)"
printf 'codex_home=%s\n' "${CODEX_HOME:-$HOME/.codex}"
printf 'shell=%s\n' "${SHELL:-/bin/bash}"
printf 'provider=codex|%s\n' "${CODEX_HOME:-$HOME/.codex}"
printf 'provider=claude|%s\n' "${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
# Only conventional automatic candidates are filtered. Explicit environment
# homes above and the saved provider-roots manifest remain authoritative.
jarvis_auto_provider_home() (
  [ -d "$1" ] || exit 1
  suffix=${1##*/}; suffix=${suffix#*-}
  [ -n "$suffix" ] || exit 1
  tokens=$(printf '%s' "$suffix" | LC_ALL=C tr '[:upper:]' '[:lower:]' | tr '._-' '   ')
  case " $tokens " in *" backup "*|*" backups "*|*" bak "*) exit 1 ;; esac
  exit 0
)
for p in "$HOME"/.codex-*; do jarvis_auto_provider_home "$p" && printf 'provider=codex|%s\n' "$p"; done
for p in "$HOME"/.claude-*; do jarvis_auto_provider_home "$p" && printf 'provider=claude|%s\n' "$p"; done

# Ищем в три захода, и это не перестраховка: неинтерактивный ssh получает
# урезанный PATH — без nvm, ~/.local/bin и homebrew. Claude Code почти всегда
# оказывается ровно там, поэтому одного `command -v` мало: он честно отвечает
# «нет» про установленный агент.
WANT="tmux curl claude codex cargo"

# Собственный shim не доказывает наличие CLI: он устанавливается и для
# ещё не установленного агента. Проверяем только небольшой заголовок файла,
# затем продолжаем PATH за shim (в том числе nvm/fnm из login shell).
JARVIS_TOOL_PROBE='
jarvis_real_executable() (
  candidate=$1
  [ -f "$candidate" ] && [ -x "$candidate" ] || exit 1
  case "$2" in
    claude|codex)
      dd if="$candidate" bs=256 count=1 2>/dev/null | LC_ALL=C grep -Fq "# jarvis agent shim" && exit 1
      ;;
  esac
  exit 0
)
jarvis_has_tool() (
  tool=$1
  remaining=$PATH
  while :; do
    case "$remaining" in
      *:*) directory=${remaining%%:*}; remaining=${remaining#*:}; more=1 ;;
      *) directory=$remaining; more=0 ;;
    esac
    [ -n "$directory" ] || directory=.
    jarvis_real_executable "$directory/$tool" "$tool" && exit 0
    [ "$more" = 1 ] || exit 1
  done
)
for tool in tmux curl claude codex cargo systemctl; do
  jarvis_has_tool "$tool" && printf "have=%s\n" "$tool"
done
printf "provider=codex|%s\n" "${CODEX_HOME:-$HOME/.codex}"
printf "provider=claude|%s\n" "${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
'
# 1. PATH как есть. Определения остаются доступны для известных каталогов.
eval "$JARVIS_TOOL_PROBE"

# 2. Известные места установки. Дубли не мешают: ноут проверяет вхождение.
for p in "$HOME/.local/bin" "$HOME/bin" "$HOME/.cargo/bin" "$HOME/.bun/bin" \
         "$HOME/.claude/local" "$HOME/.npm-global/bin" "$HOME/.local/share/pnpm" \
         /usr/local/bin /opt/homebrew/bin /snap/bin; do
  for b in $WANT; do
    jarvis_real_executable "$p/$b" "$b" && printf 'have=%s\n' "$b"
  done
done

# 3. Логин-шелл — он прочитает профиль и подхватит nvm/fnm/asdf/mise. Под
# таймаутом: чужой профиль может ждать ввода или уходить в сеть, а разведка
# зависать не имеет права.
LSH="${SHELL:-/bin/sh}"
T=""
command -v timeout >/dev/null 2>&1 && T="timeout 10"
if [ -x "$LSH" ]; then
  export JARVIS_TOOL_PROBE
  $T "$LSH" -lc 'exec /bin/sh -c "$JARVIS_TOOL_PROBE"' 2>/dev/null
fi

[ -d "${CODEX_HOME:-$HOME/.codex}" ] && printf 'have=%s\n' codex-home
[ -d "$HOME/.claude" ] && printf 'have=%s\n' claude-home
systemctl --user show-environment >/dev/null 2>&1 && printf 'have=%s\n' systemd-user
if [ "$(id -u)" = 0 ] && [ -d /run/systemd/system ] && systemctl --system show-environment >/dev/null 2>&1; then
  printf 'have=%s\n' systemd-system
fi
for pm in apt-get dnf yum apk brew; do
  if command -v "$pm" >/dev/null 2>&1; then printf 'have=package-%s\n' "$pm"; break; fi
done
if [ "$(id -u)" = 0 ]; then printf 'have=package-root\n'
elif command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; then printf 'have=package-sudo\n'; fi
exit 0"##;

fn probe(host: &Connection) -> Result<Remote, String> {
    let raw = run_ssh(host, PROBE).map_err(|e| remote_probe_error(host, &e))?;
    let home = kv(&raw, "home").unwrap_or_default();
    if !home.starts_with('/') {
        return Err(format!(
            "та сторона не назвала $HOME (ответ: {:?}) — установка без него невозможна",
            raw.trim()
        ));
    }
    Ok(Remote {
        os: kv(&raw, "os").unwrap_or_default().to_lowercase(),
        arch: kv(&raw, "arch").unwrap_or_default().to_lowercase(),
        // пустой ответ превратил бы путь хуков в «/hooks.json» — писать в корень
        // чужой машины установщик не должен ни при каких обстоятельствах
        codex_home: kv(&raw, "codex_home")
            .map(|d| d.trim_end_matches('/').to_string())
            .filter(|d| d.starts_with('/'))
            .unwrap_or_else(|| format!("{}/.codex", home.trim_end_matches('/'))),
        provider_sources: parse_provider_sources(&raw),
        shell: kv(&raw, "shell").unwrap_or_else(|| "/bin/bash".into()),
        home,
        tools: raw
            .lines()
            .filter_map(|l| l.split_once('='))
            .filter(|(k, _)| *k == "have")
            .map(|(_, v)| v.trim().to_string())
            .collect(),
    })
}

fn parse_provider_sources(raw: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for line in raw.lines().filter_map(|line| line.strip_prefix("provider=")) {
        let Some((agent, path)) = line.split_once('|') else { continue; };
        let components = Path::new(path).components().collect::<Vec<_>>();
        if !matches!(agent, "codex" | "claude") || !path.starts_with('/') || path.chars().any(char::is_control)
            || !components.iter().any(|part| matches!(part,std::path::Component::Normal(_)))
            || components.iter().any(|part| matches!(part,std::path::Component::ParentDir)) { continue; }
        let row = json!({"agent":agent,"providerHome":path.trim_end_matches('/')});
        if !out.contains(&row) && out.len() < 32 { out.push(row); }
    }
    out
}

/* ================= вход по паролю (разовый) ================= */

/// Положить наш публичный ключ в `authorized_keys`, войдя по паролю.
///
/// Пароль — только для этого одного раза, и вот почему. Туннель к узлу живёт
/// в фоне и переподнимается сам: после сна ноута, смены сети, перезагрузки
/// VPS. Спросить пароль в этот момент не у кого — значит транспорт обязан
/// работать по ключу. Пароль здесь ровно затем, чтобы ключ там появился.
///
/// Пароль не пишется на диск и не попадает в argv (его увидел бы любой `ps`):
/// ssh забирает его через `SSH_ASKPASS`, а помощник читает переменную окружения
/// нашего же процесса.
pub fn authorize_key(
    progress: &Progress,
    ssh_host: &str,
    password: &str,
    public_key: &str,
) -> Result<(), String> {
    authorize_key_connection(progress, &Connection::ssh(ssh_host), password, public_key)
}

pub fn authorize_key_connection(
    progress: &Progress,
    connection: &Connection,
    password: &str,
    public_key: &str,
) -> Result<(), String> {
    connection.validate()?;
    if connection.transport == "teleport" {
        return Err("Для Teleport войди через tsh. Пароль SSH и authorized_keys не используются".into());
    }
    let key = public_key.trim();
    if !key.starts_with("ssh-") && !key.starts_with("ecdsa-") {
        return Err("это не похоже на публичный ключ (ожидаю строку вида ssh-ed25519 AAAA…)".into());
    }
    if password.is_empty() {
        return Err("нужен пароль пользователя на той машине".into());
    }
    progress(Step::start(PHASE_LINK));

    // grep -qxF по целой строке: дважды класть тот же ключ незачем, а
    // подстрочное совпадение приняло бы чужой ключ с нашим префиксом.
    let script = format!(
        r#"set -e
umask 077
mkdir -p "$HOME/.ssh"
touch "$HOME/.ssh/authorized_keys"
chmod 700 "$HOME/.ssh"
chmod 600 "$HOME/.ssh/authorized_keys"
k={key}
grep -qxF "$k" "$HOME/.ssh/authorized_keys" || printf '%s\n' "$k" >> "$HOME/.ssh/authorized_keys"
printf 'ok\n'
"#,
        key = sh_quote(key),
    );
    ssh_with_password(connection, password, &script)?;
    progress(Step::done(PHASE_LINK, "ключ добавлен в ~/.ssh/authorized_keys"));

    // Проверяем именно то, чем будем пользоваться дальше: вход по ключу без
    // пароля. Успешная запись ключа ещё не значит, что sshd его примет —
    // PubkeyAuthentication может быть выключен, а домашний каталог доступен
    // на запись группе (тогда sshd молча игнорирует authorized_keys).
    // Authenticate as the SSH account. The explicitly selected agent owner is
    // checked during preflight, after authorization has succeeded.
    let mut login = connection.clone();
    login.run_as_user = None;
    run_ssh(&login, "true").map_err(|e| {
        format!(
            "ключ записан, но вход по ключу всё равно не работает: {e}\n\
             Обычно это одно из двух: в sshd выключен PubkeyAuthentication либо \
             у $HOME или ~/.ssh слишком широкие права (sshd такие каталоги игнорирует; \
             лечится chmod go-w \"$HOME\" и chmod 700 ~/.ssh)."
        )
    })?;
    progress(Step::done(PHASE_LINK, "вход по ключу работает — пароль больше не нужен"));
    Ok(())
}

/// Один заход по паролю. Помощник для `SSH_ASKPASS` кладём во временный файл
/// с правами 0700 и убираем сразу после — он нужен ровно на время вызова.
fn password_command(connection: &Connection) -> Result<Command, String> {
    connection.validate()?;
    if connection.transport == "teleport" { return Err("Вход Teleport выполняется через tsh login".into()); }
    let mut cmd = Command::new("ssh");
    if let Some(path) = &connection.ssh_config_file { cmd.args(["-F", path]); }
    cmd.args([
        "-o", "BatchMode=no", "-o", "PubkeyAuthentication=no",
        "-o", "PreferredAuthentications=password,keyboard-interactive",
        "-o", "NumberOfPasswordPrompts=1", "-o", "StrictHostKeyChecking=accept-new",
        "-o", "ConnectTimeout=15", "-o", "ForwardAgent=no", "-o", "ForkAfterAuthentication=no",
        // A password bootstrap must establish its own authenticated connection.
        "-o", "ControlMaster=no", "-o", "ControlPath=none",
    ]).arg(&connection.ssh_host);
    Ok(cmd)
}

fn ssh_with_password(connection: &Connection, password: &str, script: &str) -> Result<String, String> {
    let mut command = password_command(connection)?;
    let host = &connection.ssh_host;
    let helper = write_askpass()?;
    let out = command
        .arg(connection::posix_script(script))
        .env("SSH_ASKPASS", &helper)
        // без force ssh спросит пароль у терминала, которого у нас нет
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", ":0") // старые сборки ssh требуют его для askpass
        .env("JARVIS_SSH_PASS", password)
        .stdin(Stdio::null())
        .output();
    let _ = fs::remove_file(&helper);
    let out = out.map_err(|e| format!("не смог запустить ssh: {e}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if err.contains("Permission denied") {
        format!("{host}: пароль не подошёл (или на сервере запрещён вход по паролю)")
    } else if err.is_empty() {
        format!("ssh вернул код {}", out.status.code().unwrap_or(-1))
    } else {
        err
    })
}

/// Помощник, который отдаёт ssh пароль из переменной окружения.
fn write_askpass() -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir().join(format!(".jarvis-askpass-{}", std::process::id()));
    fs::write(&path, "#!/bin/sh\nprintf '%s\\n' \"$JARVIS_SSH_PASS\"\n")
        .map_err(|e| format!("не смог подготовить askpass: {e}"))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("не смог выставить права askpass: {e}"))?;
    Ok(path)
}

/* ================= разведка для панели ================= */

/// Что панель показывает про машину ДО установки.
///
/// Разведка та же, что у установщика, но результат — не текст в терминале, а
/// данные: человек должен увидеть, чего на той стороне не хватает, прежде чем
/// запускать установку, а не узнать это из середины лога.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Preflight {
    pub os: String,
    pub arch: String,
    pub home: String,
    /// Куда встанет узел с учётом запрошенного каталога (`~` уже развёрнут).
    pub dir: String,
    pub tmux: bool,
    pub curl: bool,
    pub claude: bool,
    pub codex: bool,
    pub systemd: bool,
    pub cargo: bool,
    /// Как сюда попадёт бинарь узла: `local` | `download` | `build` | `none`.
    pub node_source: String,
    pub node_note: String,
    pub provider_sources: Vec<Value>,
    pub runtime_setup: RuntimeSetup,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSetup {
    pub missing: Vec<String>,
    pub automatic: bool,
    pub command: Option<String>,
}

fn runtime_setup(remote: &Remote) -> RuntimeSetup {
    let missing: Vec<String> = ["tmux", "curl"].into_iter().filter(|tool| !remote.has(tool)).map(str::to_string).collect();
    if missing.is_empty() { return RuntimeSetup::default(); }
    let Some(manager) = ["apt-get", "dnf", "yum", "apk", "brew"].into_iter().find(|pm| remote.has(&format!("package-{pm}"))) else {
        return RuntimeSetup { missing, ..RuntimeSetup::default() };
    };
    let root = remote.has("package-root");
    let automatic = if manager == "brew" { !root } else { root || remote.has("package-sudo") };
    let prefix = if root || manager == "brew" { "" } else { "sudo -n " };
    let packages = missing.join(" ");
    let command = match manager {
        "apt-get" => format!("{prefix}env DEBIAN_FRONTEND=noninteractive apt-get update -q && {prefix}env DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends {packages}"),
        "dnf" | "yum" => format!("{prefix}{manager} install -y {packages}"),
        "apk" => format!("{prefix}apk add --no-cache {packages}"),
        _ => format!("brew install {packages}"),
    };
    RuntimeSetup { missing, automatic, command: Some(command) }
}

fn prepare_runtime(progress: &Progress, host: &Connection, remote: &mut Remote) -> Result<(), String> {
    let setup = runtime_setup(remote);
    if setup.missing.is_empty() || !setup.automatic { return Ok(()); }
    progress(Step::start(PHASE_ENV));
    progress(Step::done(PHASE_ENV, format!("Устанавливаю {} для чатов и уведомлений", setup.missing.join(" + "))));
    run_ssh(host, &format!("set -e\nPATH=\"$PATH:/opt/homebrew/bin:/usr/local/bin\"\nexport PATH\n{}", setup.command.unwrap())).map_err(|error| format!("Не удалось подготовить окружение: {error}"))?;
    *remote = probe(host)?;
    let remaining = runtime_setup(remote).missing;
    if !remaining.is_empty() { return Err(format!("После установки не найдены: {}. Проверь PATH пользователя агентов", remaining.join(", "))); }
    Ok(())
}

/// Сходить на машину и рассказать, что там. Ошибка — только недоступность:
/// нехватка tmux, агента или curl это состояние машины, а не отказ.
pub fn preflight(ssh_host: &str, dir: Option<&str>) -> Result<Preflight, String> {
    preflight_connection(&Connection::ssh(ssh_host), dir)
}

pub fn preflight_connection(ssh_host: &Connection, dir: Option<&str>) -> Result<Preflight, String> {
    ssh_host.validate()?;
    let remote = probe(ssh_host)?;
    let setup = runtime_setup(&remote);
    let triples = target_triples(&remote.os, &remote.arch);
    // Текст — для человека, а не для лога: ссылку целиком тут показывать незачем
    // (она длинная и в строку панели не влезает), важно откуда и подо что.
    let (node_source, node_note) = match resolve_node(&remote, &triples) {
        Ok(NodeSource::Bundled(target, _)) => (
            "local".to_string(),
            format!("Передам серверный компонент из этого приложения ({target}). Скачивать его на VM или устанавливать Rust не нужно."),
        ),
        Ok(NodeSource::Local(p)) => (
            "local".to_string(),
            format!(
                "залью готовый бинарь с этой машины: {}",
                p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
            ),
        ),
        Ok(NodeSource::Download(_)) => (
            "download".to_string(),
            format!(
                "Попробую загрузить серверный компонент v{} ({}). Наличие файла в релизе будет проверено при установке.",
                env!("CARGO_PKG_VERSION"),
                triples.first().map(String::as_str).unwrap_or("?"),
            ),
        ),
        Ok(NodeSource::Build) => (
            "build".to_string(),
            "готовой сборки под эту платформу нет — соберу узел прямо там через cargo; \
             первый раз это несколько минут"
                .to_string(),
        ),
        Err(_) if setup.automatic => (
            "download".to_string(),
            "Установлю curl и загружу узел для этой машины".to_string(),
        ),
        Err(_) => (
            "none".to_string(),
            "взять узел неоткуда: нет ни curl (скачать), ни cargo (собрать). \
             Поставь на ту машину curl — он всё равно нужен хукам"
                .to_string(),
        ),
    };
    let requested = dir
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(DEFAULT_DIR);
    Ok(Preflight {
        dir: remote.expand(requested.trim_end_matches('/')),
        tmux: remote.has("tmux"),
        curl: remote.has("curl"),
        claude: remote.has("claude") || remote.has("claude-home"),
        codex: remote.has("codex") || remote.has("codex-home"),
        systemd: service_scope(&remote).is_some(),
        cargo: remote.has("cargo"),
        os: remote.os,
        arch: remote.arch,
        home: remote.home,
        node_source,
        node_note,
        provider_sources: remote.provider_sources.clone(),
        runtime_setup: setup,
    })
}

/* ================= бинарь узла ================= */

/// Rust-триплеты, в которые cargo складывает cross-сборку под эту машину.
/// Порядок важен: gnu вероятнее musl, и первым должен идти самый ходовой.
fn target_triples(os: &str, arch: &str) -> Vec<String> {
    let arch = match arch {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        other => other,
    };
    match os {
        "linux" => vec![
            format!("{arch}-unknown-linux-gnu"),
            format!("{arch}-unknown-linux-musl"),
        ],
        "darwin" => vec![format!("{arch}-apple-darwin")],
        _ => Vec::new(),
    }
}

/// Где искать собранный `jarvis-node`. Порядок — от самого явного к самому
/// вероятному: переменная окружения, сосед по каталогу с `jarvis-setup`
/// (в dev это `src-tauri/target/release`), артефакты cross-сборки, дерево
/// репозитория относительно текущего каталога.
fn node_candidates(triples: &[String]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Ok(explicit) = std::env::var("JARVIS_NODE_BIN") {
        if !explicit.trim().is_empty() {
            out.push(PathBuf::from(explicit.trim()));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("jarvis-node"));
            if let Some(target) = dir.parent() {
                for t in triples {
                    out.push(target.join(t).join("release/jarvis-node"));
                }
            }
        }
    }
    for root in ["src-tauri/target", "target"] {
        out.push(PathBuf::from(root).join("release/jarvis-node"));
        for t in triples {
            out.push(PathBuf::from(root).join(t).join("release/jarvis-node"));
        }
    }
    out
}

/// Что это за бинарь по первым байтам: `(ос, арх)`. Проверка грубая, но ловит
/// главную ошибку установки — залить mac-сборку на Linux-VPS и получить
/// `cannot execute binary file` в логе systemd вместо внятного отказа здесь.
fn binary_kind(head: &[u8]) -> Option<(&'static str, &'static str)> {
    if head.len() >= 20 && head[..4] == [0x7f, b'E', b'L', b'F'] {
        // e_machine — 16-битное поле по смещению 18; порядок байт задаёт EI_DATA
        let machine = if head[5] == 2 {
            u16::from_be_bytes([head[18], head[19]])
        } else {
            u16::from_le_bytes([head[18], head[19]])
        };
        let arch = match machine {
            0x3e => "x86_64",
            0xb7 => "aarch64",
            _ => "неизвестная",
        };
        return Some(("linux", arch));
    }
    if head.len() >= 8 {
        // Mach-O 64: magic + cputype (оба little-endian у наших сборок);
        // 0xcafebabe — универсальный образ, архитектура внутри
        let magic = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
        let cputype = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        if magic == 0xfeed_facf {
            let arch = match cputype {
                0x0100_000c => "aarch64",
                0x0100_0007 => "x86_64",
                _ => "неизвестная",
            };
            return Some(("darwin", arch));
        }
        if u32::from_be_bytes([head[0], head[1], head[2], head[3]]) == 0xcafe_babe {
            return Some(("darwin", "universal"));
        }
    }
    None
}

/// Годится ли найденный бинарь для той машины. Незнакомый формат и незнакомая
/// архитектура внутри знакомого формата — не повод отказывать: судим только по
/// тому, в чём уверены (ELF ≠ Mach-O), иначе установка ломалась бы на экзотике,
/// которая на самом деле запустилась бы.
fn binary_fits(kind: Option<(&str, &str)>, os: &str, arch: &str) -> bool {
    let Some((bin_os, bin_arch)) = kind else {
        return true;
    };
    if bin_os != os {
        return false;
    }
    match target_triples(os, arch).first() {
        Some(triple) => bin_arch == "universal" || triple.starts_with(bin_arch) || bin_arch == "неизвестная",
        None => true,
    }
}

/// Откуда возьмётся `jarvis-node` для той машины.
///
/// Порядок выбора — от самого быстрого и предсказуемого к самому долгому.
/// Собранный локально бинарь есть только у разработчика; у человека с
/// установленным приложением его нет и быть не может, поэтому основной путь —
/// скачать готовый на самой удалённой машине. Сборка на той стороне — последний
/// рубеж: она честно работает, но требует там rust и нескольких минут.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeSource {
    /// Server binaries built from the exact same sources as the desktop app.
    Bundled(&'static str, &'static [u8]),
    /// Готовый файл на ЭТОЙ машине (dev-сборка или явный `JARVIS_NODE_BIN`).
    Local(PathBuf),
    /// Скачать на ТОЙ стороне из релиза этой же версии.
    Download(String),
    /// Собрать на той стороне из исходников — там нашёлся cargo.
    Build,
}

impl NodeSource {
    /// Короткий тег для панели.
    pub fn tag(&self) -> &'static str {
        match self {
            NodeSource::Bundled(_, _) => "local",
            NodeSource::Local(_) => "local",
            NodeSource::Download(_) => "download",
            NodeSource::Build => "build",
        }
    }
}

/// Ссылка на бинарь узла в релизе. Версия — та же, что у приложения: узел и
/// демон говорят по одному протоколу, и разъезд версий лечится ровно тем, что
/// они выпускаются вместе.
fn release_url(triple: &str) -> String {
    format!(
        "https://github.com/Sergey-Chernyshev/jarvis/releases/download/v{}/jarvis-node-{triple}",
        env!("CARGO_PKG_VERSION")
    )
}

/// Выбрать способ доставки. Ошибка — только когда не остаётся ни одного:
/// незнакомая платформа без cargo на той стороне.
fn node_sources(remote: &Remote, triples: &[String]) -> Vec<NodeSource> {
    node_sources_with_bundle(remote, triples, bundled_nodes::BINARIES)
}

fn node_sources_with_bundle(remote: &Remote, triples: &[String], bundled: &'static [(&'static str, &'static [u8])]) -> Vec<NodeSource> {
    let mut out = Vec::new();
    // A packaged app is self-contained. Do not replace its current node with
    // an older published binary that happens to share the package version.
    for &(target, bytes) in bundled {
        if triples.iter().any(|t| t == target) && binary_fits(binary_kind(bytes), &remote.os, &remote.arch) {
            out.push(NodeSource::Bundled(target, bytes));
            return out;
        }
    }
    // Локальный бинарь берём, только если он ГОДИТСЯ для той машины: залить
    // mac-сборку на Linux — самая частая ошибка установки, и молчать о ней
    // нельзя (в логе systemd это выглядит как «cannot execute binary file»).
    for path in node_candidates(triples) {
        if !path.is_file() {
            continue;
        }
        let head = read_head(&path);
        if binary_fits(binary_kind(&head), &remote.os, &remote.arch) {
            out.push(NodeSource::Local(path));
            break;
        }
    }
    if remote.has("curl") {
        for triple in triples {
            out.push(NodeSource::Download(release_url(triple)));
        }
    }
    if remote.has("cargo") {
        out.push(NodeSource::Build);
    }
    out
}

/// Первый способ из списка — его показывает разведка как «план». Текст ошибки
/// пустой: развёрнутую инструкцию собирает вызывающий, ему видны все причины.
fn resolve_node(remote: &Remote, triples: &[String]) -> Result<NodeSource, String> {
    node_sources(remote, triples)
        .into_iter()
        .next()
        .ok_or_else(String::new)
}

/// Первые байты файла — по ним `binary_kind` отличает ELF от Mach-O. Читаем
/// голову, а не файл целиком: кандидатов несколько, а весит узел мегабайты.
fn read_head(path: &Path) -> Vec<u8> {
    use std::io::Read;
    let mut buf = vec![0u8; 32];
    match fs::File::open(path).and_then(|mut f| f.read(&mut buf)) {
        Ok(n) => {
            buf.truncate(n);
            buf
        }
        Err(_) => Vec::new(),
    }
}

/// Инструкция вместо бинаря. Печатается один раз и должна быть достаточной:
/// человек ушёл собирать, вернулся, повторил команду.
fn build_hint(remote: &Remote, triples: &[String], tried: &[PathBuf]) -> String {
    let triple = triples
        .first()
        .cloned()
        .unwrap_or_else(|| format!("{}-{}", remote.arch, remote.os));
    let tried: Vec<String> = tried.iter().map(|p| p.display().to_string()).collect();
    format!(
        "нужен jarvis-node, собранный под ТУ машину ({} {}), а не под эту:\n  \
         • собрать прямо там, из копии репозитория:\n      \
           cd src-tauri && cargo build --release -p jarvis-node\n      \
           (файл появится в src-tauri/target/release/jarvis-node — его же можно\n       \
            положить в <каталог узла>/bin/ руками и chmod +x)\n  \
         • или кросс-сборкой отсюда (нужен линкер под цель):\n      \
           cd src-tauri && cargo build --release -p jarvis-node --target {triple}\n  \
         • или указать готовый файл явно:\n      \
           JARVIS_NODE_BIN=/путь/к/jarvis-node jarvis-setup remote add …\n\
         Искал здесь:\n  {}",
        remote.os,
        remote.arch,
        tried.join("\n  "),
    )
}

/// Исходники узла, вшитые в приложение. Нужны, когда бинарь взять негде, а на
/// той машине есть cargo: крейт крошечный (три зависимости), и собрать его там
/// быстрее и честнее, чем требовать от человека кросс-компиляцию.
///
/// `include_str!` — по той же причине, что и у остальных шимов: установщик не
/// должен зависеть от того, лежит ли рядом дерево исходников.
const NODE_SRC: [(&str, &str); 16] = [
    ("Cargo.toml", include_str!("../../node/Cargo.toml")),
    ("src/main.rs", include_str!("../../node/src/main.rs")),
    ("src/node/mod.rs", include_str!("../../node/src/node/mod.rs")),
    ("src/node/ring.rs", include_str!("../../node/src/node/ring.rs")),
    ("src/node/files.rs", include_str!("../../node/src/node/files.rs")),
    ("src/node/http.rs", include_str!("../../node/src/node/http.rs")),
    ("src/node/tmux.rs", include_str!("../../node/src/node/tmux.rs")),
    ("src/node/agent.rs", include_str!("../../node/src/node/agent.rs")),
    ("src/node/projects.rs", include_str!("../../node/src/node/projects.rs")),
    ("src/node/live.rs", include_str!("../../node/src/node/live.rs")),
    ("src/node/sources.rs", include_str!("../../node/src/node/sources.rs")),
    ("src/node/hooks.rs", include_str!("../../node/src/node/hooks.rs")),
    ("shared/Cargo.toml", include_str!("../../shared/Cargo.toml")),
    ("shared/src/lib.rs", include_str!("../../shared/src/lib.rs")),
    ("shared/src/codex_hooks.rs", include_str!("../../shared/src/codex_hooks.rs")),
    ("shared/src/terminal_stream.rs", include_str!("../../shared/src/terminal_stream.rs")),
];

/// The portable root embeds the private shared crate alongside the node sources.
fn portable_node_source(path: &str, body: &str) -> String {
    if path == "Cargo.toml" {
        body.replace("jarvis-node-shared = { path = \"../shared\" }", "jarvis-node-shared = { path = \"shared\" }")
    } else { body.to_string() }
}

/// Положить `jarvis-node` в `<dir>/bin` тем способом, который выбрал
/// [`resolve_node`]. Все три пути заканчиваются одинаково: рабочий бинарь на
/// боевом месте — и проверяются тоже одинаково, запуском `--version`.
fn deliver_node(
    progress: &Progress,
    host: &Connection,
    dir: &str,
    remote: &Remote,
    sources: &[NodeSource],
) -> Result<(), String> {
    let dst = format!("{dir}/bin/jarvis-node");
    let mut why: Vec<String> = Vec::new();
    for (i, src) in sources.iter().enumerate() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let candidate = format!("{dst}.jarvis-candidate-{}-{nonce}", std::process::id());
        let outcome = try_source(progress, host, dir, remote, &candidate, src)
            .and_then(|()| run_ssh(host, &activate_node_script(&candidate, &dst)).map(|_| ()));
        match outcome {
            Ok(()) => return Ok(()),
            Err(e) => {
                // Leave a working node intact if transfer, platform validation
                // or version validation fails. Only our unique staging file
                // is removed; an interrupted retry is safe to run again.
                let _ = run_ssh(host, &format!("rm -f -- {}", sh_quote(&candidate)));
                // Отказ одного способа — не конец: релиза этой версии может не
                // быть, а rust на машине есть (и наоборот). Пробуем следующий,
                // а причины копим — если не выйдет ни один, человеку нужны все.
                let last = i + 1 == sources.len();
                if !last {
                    progress(Step::warn(PHASE_NODE, one_line_short(&e)));
                }
                why.push(e);
            }
        }
    }
    Err(if why.is_empty() {
        "узел взять неоткуда".to_string()
    } else {
        why.join("\n\n")
    })
}

fn activate_node_script(candidate: &str, dst: &str) -> String {
    format!(
        "set -e\nf={}\nversion=$(\"$f\" --version)\nif [ \"$version\" != {} ]; then printf 'Несовместимая версия серверного компонента: %s\\n' \"$version\" >&2; exit 1; fi\nmv -f -- \"$f\" {}\n",
        sh_quote(candidate), sh_quote(&format!("jarvis-node {}", env!("CARGO_PKG_VERSION"))), sh_quote(dst)
    )
}

/// Первая строка ошибки — для строки лога; полный текст уходит в итоговый отказ.
fn one_line_short(e: &str) -> String {
    let first = e.lines().next().unwrap_or(e).trim();
    if first.chars().count() > 160 {
        format!("{}…", first.chars().take(159).collect::<String>())
    } else {
        first.to_string()
    }
}

/// Один способ доставки целиком: положить бинарь и убедиться, что он там живой.
fn try_source(
    progress: &Progress,
    host: &Connection,
    dir: &str,
    remote: &Remote,
    dst: &str,
    src: &NodeSource,
) -> Result<(), String> {
    let dst = dst.to_string();
    match src {
        NodeSource::Bundled(target, bytes) => {
            progress(Step::info(PHASE_NODE, "Передаю серверный компонент из Jarvis"));
            put_file(host, &dst, bytes, Some("755"), false)?;
            progress(Step::done(PHASE_NODE, format!("Компонент передан · {target} · {} КБ", bytes.len() / 1024)));
        }
        NodeSource::Local(path) => {
            let bytes =
                fs::read(path).map_err(|e| format!("не смог прочитать {}: {e}", path.display()))?;
            if binary_kind(&bytes).is_none() {
                progress(Step::info(
                    PHASE_NODE,
                    format!("формат {} не распознал — заливаю как есть", path.display()),
                ));
            }
            put_file(host, &dst, &bytes, Some("755"), false)?;
            progress(Step::done(
                PHASE_NODE,
                format!("{dst} ← {} ({} КБ)", path.display(), bytes.len() / 1024),
            ));
        }
        NodeSource::Download(url) => {
            progress(Step::info(PHASE_NODE, "Загружаю серверный компонент из релиза"));
            download_node(host, &dst, url)?;
            progress(Step::done(
                PHASE_NODE,
                format!("{dst} ← релиз v{}", env!("CARGO_PKG_VERSION")),
            ));
        }
        NodeSource::Build => {
            progress(Step::info(
                PHASE_NODE,
                "собираю узел на той машине — в первый раз это пара минут: cargo тянет зависимости",
            ));
            build_node(host, dir, &dst)?;
            progress(Step::done(
                PHASE_NODE,
                format!("{dst} ← собран на той стороне"),
            ));
        }
    }
    // Одна проверка на все три пути: файл на месте и ЗАПУСКАЕТСЯ там. Скачанный
    // мог оказаться страницей 404, собранный — не тем таргетом, залитый —
    // сборкой под другую архитектуру. Все три случая выглядят одинаково: узел
    // молчит, а причина всплывает только в логе systemd.
    let out = run_ssh(host, &format!("set -e\nchmod +x {f}\n{f} --version\n", f = sh_quote(&dst)))
        .map_err(|e| {
            format!(
                "узел лёг на место, но не запускается на {} {}: {e}\n\
                 Так выглядит бинарь не под ту платформу или оборванная закачка.",
                remote.os, remote.arch
            )
        })?;
    progress(Step::done(PHASE_NODE, clean_remote_error(out.as_bytes())));
    Ok(())
}

/// Скачать бинарь на удалённой машине.
///
/// `-f` обязателен: без него curl бодро сохраняет страницу «404 Not Found» под
/// именем узла, и ошибка всплыла бы уже в логе systemd, а не здесь.
fn download_node(host: &Connection, dst: &str, url: &str) -> Result<(), String> {
    let script = format!(
        r#"set -e
f={dst}
mkdir -p "$(dirname "$f")"
t="$f.jarvis-new.$$"
trap 'rm -f "$t"' EXIT HUP INT TERM
curl -fsSL --max-time 300 -o "$t" {url}
chmod 755 "$t"
mv -f "$t" "$f"
"#,
        dst = sh_quote(dst),
        url = sh_quote(url),
    );
    run_ssh(host, &script).map(|_| ()).map_err(|e| {
        format!(
            "Серверный компонент из релиза недоступен: {e}\n\
             В этой сборке приложения нет компонента для выбранной платформы. \
             Обнови Jarvis до сборки со встроенным серверным компонентом.\n{url}"
        )
    })
}

/// Собрать узел из вшитых исходников прямо на той машине.
///
/// Без `--locked`: lock-файла у нас с собой нет (в репозитории он общий на весь
/// воркспейс приложения и этому крейту не подходит), поэтому cargo разрешает
/// версии сам — зависимостей три, и все с полуоткрытыми границами.
fn build_node(host: &Connection, dir: &str, dst: &str) -> Result<(), String> {
    let src_dir = format!("{dir}/src/jarvis-node");
    // Каталог пересоздаём: остатки прошлой попытки (или другой версии узла)
    // дали бы сборку неизвестно чего.
    run_ssh(
        host,
        &format!("set -e\nrm -rf {d}\nmkdir -p {d}/src/node\n", d = sh_quote(&src_dir)),
    )
    .map_err(|e| format!("не подготовил каталог сборки: {e}"))?;
    for (rel, body) in NODE_SRC {
        put_file(host, &format!("{src_dir}/{rel}"), portable_node_source(rel, body).as_bytes(), Some("644"), false)?;
    }
    // PATH дополняем руками: rustup прописывает себя в ~/.profile, который
    // неинтерактивный ssh не читает — без этой строки cargo «не найден» на
    // машине, где он стоит.
    // Вывод сборки — в файл, а не в пайп: `set -e` не видит код cargo сквозь
    // `| tail`, и провалившаяся сборка выглядела бы удачной ровно до `cp`.
    // Хвост лога при отказе печатаем сами — без него «не собралось» бесполезно.
    // После удачной сборки чистим за собой: `target` тянет сотни мегабайт, и
    // оставлять их на чужой VPS ради редкой переустановки невежливо.
    let script = format!(
        r#"set -e
export PATH="$HOME/.cargo/bin:$PATH"
cd {src}
if ! cargo build --release > build.log 2>&1; then
  echo "--- хвост сборки ---" >&2
  tail -40 build.log >&2
  exit 1
fi
cp -f target/release/jarvis-node {dst}
chmod 755 {dst}
cd /
rm -rf {src}
"#,
        src = sh_quote(&src_dir),
        dst = sh_quote(dst),
    );
    run_ssh(host, &script)
        .map(|_| ())
        .map_err(|e| format!("сборка на той стороне не удалась: {e}"))
}

/* ================= файлы на той стороне ================= */

/// Залить файл: временное имя → chmod → `mv`.
///
/// Именно так, а не `cat > файл`: во-первых, перезапись работающего бинаря даёт
/// ETXTBSY, а `mv` подменяет запись в каталоге и живой процесс доживает на старом
/// inode; во-вторых, оборванная связь оставит недописанный временный файл, а не
/// половину узла на боевом пути.
///
/// `mode` = `None` — сохранить права уже существующего файла (для чужих конфигов
/// вроде `~/.claude/settings.json`), иначе выставить указанные.
fn put_file(
    host: &Connection,
    path: &str,
    data: &[u8],
    mode: Option<&str>,
    backup: bool,
) -> Result<(), String> {
    let q = sh_quote(path);
    let mut script = format!("set -e\numask 077\nf={q}\nmkdir -p \"$(dirname \"$f\")\"\nt=\"$f.jarvis-new.$$\"\ntrap 'rm -f \"$t\"' EXIT HUP INT TERM\n");
    script.push_str("if [ -f \"$f\" ]; then cp -p \"$f\" \"$t\"; fi\ncat > \"$t\"\n");
    if let Some(mode) = mode { script.push_str(&format!("chmod {mode} \"$t\"\n")); }
    script.push_str("if [ -f \"$f\" ] && cmp -s \"$f\" \"$t\"; then ");
    if let Some(mode) = mode { script.push_str(&format!("chmod {mode} \"$f\"; ")); }
    script.push_str("exit 0; fi\n");
    if backup { script.push_str("if [ -f \"$f\" ]; then cp -p \"$f\" \"$f.bak-$(date -u +%Y-%m-%dT%H-%M-%SZ).$$\"; fi\n"); }
    script.push_str("mv -f \"$t\" \"$f\"\n");
    send_ssh(host, &script, data).map_err(|e| format!("не записал {path}: {e}"))
}

/// Тот же `jarvis-hook`, что и локально, но стучащийся в сокет УЗЛА.
///
/// Шим вычисляет сокет от собственного расположения и получает `<dir>/run.sock` —
/// это путь ДЕМОНА; узел слушает `node.sock`, потому что на одной машине они
/// могут стоять рядом. Правим ровно шаблон пути и проверяем, что он нашёлся
/// ровно один раз: если шим когда-нибудь изменится, установка обязана упасть
/// громко, а не поставить хук, который молча стучится в никуда.
fn node_hook_src() -> Result<String, String> {
    const FROM: &str = "/run.sock}";
    const TO: &str = "/node.sock}";
    if super::HOOK_SRC.matches(FROM).count() != 1 {
        return Err(
            "в bin/jarvis-hook больше не видно шаблона сокета — почини install/remote.rs \
             (узлу нужен node.sock, а не run.sock)"
                .into(),
        );
    }
    Ok(super::HOOK_SRC.replace(FROM, TO))
}

/// Хуки агента на той стороне: читаем конфиг по ssh, мержим ТОЙ ЖЕ функцией, что
/// и локальная установка, пишем обратно. Merge, а не overwrite: на VPS вполне
/// могут жить чужие хуки, и сносить их установщик Jarvis не вправе.
fn remote_hooks(
    progress: &Progress,
    host: &Connection,
    path: &str,
    label: &str,
    events: &[(&str, &str)],
    hook_bin: &str,
) -> Result<(), String> {
    let raw = run_ssh(host, &format!("f={}; if [ -e \"$f\" ]; then cat \"$f\"; fi", sh_quote(path)))?;
    let mut json: Value = if raw.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(&raw) {
            Ok(v) => v,
            // битый чужой JSON не трогаем — ровно как локальная установка
            Err(_) => {
                return Err(format!("{path} — невалидный JSON; файл сохранён без изменений, хуки {label} не установлены"));
            }
        }
    };
    if !json.is_object() || json.get("hooks").is_some_and(|hooks| !hooks.is_object()) {
        return Err(format!("{path}: некорректная структура hooks; файл не изменён"));
    }
    if json.get("hooks").and_then(Value::as_object).is_some_and(|hooks| hooks.values().any(|value| !value.is_array())) {
        return Err(format!("{path}: hooks event должен быть массивом; файл не изменён"));
    }
    let (added, healed) = super::merge_hooks(&mut json, hook_bin, label, events);
    if added.is_empty() && healed.is_empty() {
        progress(Step::done(PHASE_HOOKS, format!("{label}: уже установлены")));
        return Ok(());
    }
    let body = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())? + "\n";
    put_file(host, path, body.as_bytes(), None, true)?;
    progress(Step::done(
        PHASE_HOOKS,
        format!("{label}: {}", super::hooks_msg(&added, &healed)),
    ));
    Ok(())
}

/// Preserve explicit configured homes across reinstall; no agent settings or
/// credentials are read to discover roots.
fn install_sources(host: &Connection, remote: &Remote, dir: &str, explicit: &[Value]) -> Result<Vec<Value>, String> {
    let path = format!("{dir}/provider-roots.json");
    let raw = run_ssh(host, &format!("f={}; if [ -e \"$f\" ]; then cat \"$f\"; fi", sh_quote(&path)))?;
    let mut sources = remote.provider_sources.clone();
    for row in explicit {
        let agent = row["agent"].as_str().unwrap_or("");
        let path = row["providerHome"].as_str().unwrap_or("");
        let parsed = parse_provider_sources(&format!("provider={agent}|{path}"));
        if parsed.is_empty() { return Err("Некорректный явно выбранный источник агента".into()); }
        if !sources.contains(&parsed[0]) { sources.push(parsed[0].clone()); }
    }
    if !raw.trim().is_empty() {
        let value: Value = serde_json::from_str(&raw).map_err(|_| "provider-roots.json повреждён; не изменён")?;
        let rows = value["sources"].as_array().ok_or("provider-roots.json: ожидается массив sources")?;
        for row in rows {
            let agent = row["agent"].as_str().unwrap_or("");
            let path = row["providerHome"].as_str().unwrap_or("");
            let parsed = parse_provider_sources(&format!("provider={agent}|{path}"));
            if parsed.is_empty() { return Err("provider-roots.json содержит некорректный root; не изменён".into()); }
            if !sources.contains(&parsed[0]) { sources.push(parsed[0].clone()); }
        }
    }
    if sources.len() > 32 { return Err("Узел поддерживает не больше 32 источников агентов".into()); }
    let body = serde_json::to_string_pretty(&json!({"version":1,"sources":sources})).map_err(|e| e.to_string())? + "\n";
    put_file(host, &path, body.as_bytes(), Some("600"), false)?;
    Ok(sources)
}

const PATH_START: &str = "# >>> Jarvis remote transport >>>";
const PATH_END: &str = "# <<< Jarvis remote transport <<<";
fn path_profile(raw: &str, dir: &str) -> Result<String, String> {
    if raw.matches(PATH_START).count() > 1 || raw.matches(PATH_END).count() > 1 { return Err("Повторяющиеся PATH-блоки Jarvis; профиль не изменён".into()); }
    let mut content = raw.to_string();
    if let Some(start) = content.find(PATH_START) {
        let end = content[start..].find(PATH_END).map(|n| start + n + PATH_END.len())
            .ok_or("Незавершённый PATH-блок Jarvis; профиль не изменён")?;
        content.replace_range(start..end, "");
    } else if content.contains(PATH_END) { return Err("Повреждённый PATH-блок Jarvis; профиль не изменён".into()); }
    let base = content.trim_end_matches('\n');
    Ok(format!("{base}\n{PATH_START}\nexport PATH={}:\"$PATH\"\n{PATH_END}\n",sh_quote(&format!("{dir}/shims"))))
}
fn install_transport(progress: &Progress, host: &Connection, remote: &Remote, dir: &str) -> Result<(), String> {
    let shim = node_shim_src()?;
    for (name, body, marker) in [("shims/claude",shim.as_str(),"# jarvis agent shim"),
        ("shims/codex",shim.as_str(),"# jarvis agent shim"), ("tmux.conf",super::TMUX_CONF_SRC,"# tmux-конфиг отдельного сервера Jarvis")] {
        let path = format!("{dir}/{name}");
        let raw = run_ssh(host,&format!("f={}; if [ -e \"$f\" ]; then cat \"$f\"; fi",sh_quote(&path)))?;
        if !raw.is_empty() && !raw.contains(marker) { return Err(format!("{path} — чужой файл; транспорт не перезаписан")); }
        put_file(host,&path,body.as_bytes(),Some(if name.ends_with(".conf") {"644"} else {"755"}),false)?;
    }
    let profiles = if remote.shell.ends_with("zsh") { vec![".zshrc",".zprofile"] } else { vec![".bashrc",".profile"] };
    for profile in profiles {
        let path = format!("{}/{profile}",remote.home);
        let raw = run_ssh(host,&format!("f={}; if [ -e \"$f\" ]; then cat \"$f\"; fi",sh_quote(&path)))?;
        let body = path_profile(&raw,dir)?;
        if body != raw { put_file(host,&path,body.as_bytes(),None,true)?; }
    }
    progress(Step::done(PHASE_HOOKS,"Транспорт Claude/Codex установлен; новые терминалы сохраняют выбранный аккаунт"));
    Ok(())
}

fn node_shim_src() -> Result<String, String> {
    const FROM: &str = "JARVIS_SOCK=${JARVIS_SOCK:-$JARVIS_DIR/run.sock}";
    if super::SHIM_SRC.matches(FROM).count() != 1 { return Err("Шаблон сокета agent-shim изменился; удалённый транспорт не установлен".into()); }
    Ok(super::SHIM_SRC.replace(FROM, "JARVIS_SOCK=${JARVIS_SOCK:-$JARVIS_DIR/node.sock}"))
}

/// Verify the running protocol and execute the installed hook end-to-end.
/// The health event has no session_id and cannot create a user-facing chat.
fn verify_node(host: &Connection, dir: &str) -> Result<(), String> {
    let socket = sh_quote(&format!("{dir}/node.sock"));
    let raw = run_ssh(host,&format!("curl -fsS --max-time 5 --unix-socket {socket} http://jarvis/hello"))?;
    let hello: Value = serde_json::from_str(&raw).map_err(|_| "Узел не вернул JSON /hello")?;
    if hello["node"] != "jarvis-node" || hello["protocol"].as_u64().unwrap_or(0) < 2 {
        return Err("Запущен несовместимый узел; нужна версия с протоколом 2".into());
    }
    let cursor = hello["cursor"].as_u64().ok_or("Узел не вернул курсор")?;
    let nonce = format!("jarvis-health-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    let payload = serde_json::to_string(&json!({"nonce":nonce})).unwrap();
    let command = format!("printf '%s' {} | env -u JARVIS_IGNORE JARVIS_SOCK={socket} {} jarvis health >/dev/null\ncurl -fsS --max-time 5 --unix-socket {socket} 'http://jarvis/events?since={cursor}'",sh_quote(&payload),sh_quote(&format!("{dir}/bin/jarvis-hook")));
    let page: Value = serde_json::from_str(&run_ssh(host,&command)?).map_err(|_| "Узел не вернул события проверки")?;
    if !page["events"].as_array().is_some_and(|events| events.iter().any(|event| event["envelope"]["payload"]["nonce"] == nonce)) {
        return Err("Установленный хук не доставил проверочное событие в узел".into());
    }
    Ok(())
}

/* ================= автозапуск ================= */

/// Порт узла на петле по умолчанию. Нужен мобильному клиенту: форвард на
/// unix-сокет — расширение OpenSSH, которого SSH-библиотеки под Android не
/// умеют, а обычный TCP-форвард умеют все.
pub const DEFAULT_TCP_PORT: u16 = 7717;

/// Юнит systemd --user. Кавычки вокруг путей — на случай пробелов в домашнем
/// каталоге: systemd разбирает строку сам и без них споткнулся бы.
///
/// `tcp` — порт на петле или `None`. Наружу он не открывает ничего: узел
/// откажется стартовать на любом адресе кроме петли. Разница с сокетом одна:
/// сокет закрыт правами 0600, а к порту на петле может подключиться любой
/// пользователь ТОЙ машины — поэтому это выключаемо (`--no-tcp`).
/// KillMode=process deliberately keeps independently running tmux agents alive
/// when the node is upgraded or restarted; their lifecycle belongs to the user.
fn unit_text(dir: &str, tcp: Option<u16>) -> String {
    let dir = dir.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%");
    let executable_dir = dir.replace('$', "$$");
    let tcp_line = match tcp {
        Some(port) => format!("Environment=\"JARVIS_NODE_TCP=127.0.0.1:{port}\"\n"),
        None => String::new(),
    };
    format!(
        "[Unit]\n\
         Description=Jarvis node — приём хуков агентов для удалённого Jarvis\n\
         Documentation=https://github.com/Sergey-Chernyshev/jarvis/blob/master/docs/remote.md\n\
         After=default.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         Environment=\"JARVIS_DIR={dir}\"\n\
         {tcp_line}\
         ExecStart=\"{executable_dir}/bin/jarvis-node\"\n\
         Restart=always\n\
         RestartSec=2\n\
         KillMode=process\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

fn service_scope(remote: &Remote) -> Option<&'static str> {
    if remote.has("systemd-user") { Some("--user") }
    else if remote.has("systemd-system") && remote.has("package-root") { Some("--system") }
    else { None }
}

fn system_unit_text(dir: &str, home: &str, tcp: Option<u16>) -> String {
    let home = home.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%");
    unit_text(dir, tcp)
        .replace("After=default.target", "After=network.target")
        .replace("Type=simple\n", &format!("Type=simple\nUser=root\nEnvironment=\"HOME={home}\"\n"))
        .replace("WantedBy=default.target", "WantedBy=multi-user.target")
}

/// Автозапуск узла. Своего супервизора не изобретаем: есть systemd --user —
/// пользуемся им, нет — честно говорим и показываем ручной путь.
fn install_service(
    progress: &Progress,
    host: &Connection,
    remote: &Remote,
    dir: &str,
    tcp: Option<u16>,
) -> bool {
    let Some(scope) = service_scope(remote) else {
        progress(Step::warn(
            PHASE_BOOT,
            "systemctl --user на той стороне недоступен (нет systemd, нет сессии \
             пользователя или запрещён linger) — автозапуск не настроен",
        ));
        let env = match tcp {
            Some(port) => format!("JARVIS_NODE_TCP=127.0.0.1:{port} "),
            None => String::new(),
        };
        progress(Step::info(
            PHASE_BOOT,
            format!("запустить сейчас:  {env}nohup {dir}/bin/jarvis-node >> {dir}/node.log 2>&1 &"),
        ));
        progress(Step::info(
            PHASE_BOOT,
            format!(
                "поднимать после перезагрузки:  crontab -e → @reboot {dir}/bin/jarvis-node >> {dir}/node.log 2>&1"
            ),
        ));
        return false;
    };
    let system = scope == "--system";
    let path = if system { format!("/etc/systemd/system/{UNIT}") }
        else { format!("{}/.config/systemd/user/{UNIT}", remote.home) };
    let body = if system { system_unit_text(dir, &remote.home, tcp) } else { unit_text(dir, tcp) };
    // The root fallback may write a system service. Never replace a foreign
    // service that happens to use the same name.
    if system {
        let existing = run_ssh(host, &format!("f={}; if [ -e \"$f\" ]; then cat \"$f\"; fi", sh_quote(&path)));
        match existing {
            Ok(text) if text.is_empty() || text.contains("Description=Jarvis node —") => {},
            Ok(_) => { progress(Step::warn(PHASE_BOOT, format!("{path} принадлежит другой службе; файл не изменён"))); return false; },
            Err(error) => { progress(Step::warn(PHASE_BOOT, error)); return false; },
        }
    }
    if let Err(e) = put_file(host, &path, body.as_bytes(), Some("644"), true) {
        progress(Step::warn(PHASE_BOOT, format!("юнит не записан: {e}")));
        return false;
    }
    // restart, а не start: повторная установка должна поднимать НОВЫЙ бинарь,
    // а не оставлять работать залитый в прошлый раз
    let start = format!(
        "systemctl {scope} daemon-reload && systemctl {scope} enable {UNIT} && systemctl {scope} restart {UNIT}"
    );
    if let Err(e) = run_ssh(host, &start) {
        progress(Step::warn(PHASE_BOOT, format!("узел не запустился: {e}")));
        progress(Step::info(
            PHASE_BOOT,
            format!("посмотреть причину: systemctl {scope} status {UNIT}"),
        ));
        return false;
    }
    progress(Step::done(
        PHASE_BOOT,
        format!("{UNIT}: enabled + запущен (Restart=always, {scope})"),
    ));
    if system { return true; }
    // Без linger менеджер пользователя гаснет вместе с последней сессией и уносит
    // узел с собой — то есть ровно тогда, когда он и нужен: ноут отключился.
    match run_ssh(host, "loginctl enable-linger \"$(id -un)\"") {
        Ok(_) => progress(Step::info(
            PHASE_BOOT,
            "linger включён — узел живёт и после выхода из ssh",
        )),
        Err(e) => {
            progress(Step::warn(
                PHASE_BOOT,
                format!("не смог включить linger ({e}) — без него узел умрёт вместе с последней ssh-сессией"),
            ));
            progress(Step::info(
                PHASE_BOOT,
                format!("включи руками:  ssh {host} 'sudo loginctl enable-linger $(id -un)'"),
            ));
        }
    }
    true
}

/* ================= запись в настройки ноута ================= */

/// Имя узла — это и пространство имён сессий (`<remote>:<id>`), и часть имени
/// файла курсора. Поэтому вместо тихой санитизации (как в `crate::remote`)
/// требуем сразу пригодное имя: подменять то, что человек написал, установщик
/// не должен — потом не сойдётся с настройками.
fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !name.chars().all(|c| c == '.');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "имя «{name}» не годится: только латиница, цифры и «.», «-», «_» — оно \
             становится префиксом идентификаторов сессий и именем файла курсора"
        ))
    }
}

/// Что случилось с записью в settings.json.
enum Recorded {
    Added,
    Updated,
}

fn connection_record(name: &str, connection: &Connection, dir: &str, existing: Option<&Value>) -> Value {
    // Explicit nulls clear optional transport/account settings. Connection's
    // compact Serialize form omits None; using it here would resurrect old
    // fields while preserving unrelated metadata from an existing record.
    let mut entry = json!({"name":name,"jarvisDir":dir,"sshHost":connection.ssh_host,
        "transport":if connection.transport.is_empty() {"ssh"} else {&connection.transport},
        "sshConfigFile":connection.ssh_config_file,"teleportProxy":connection.teleport_proxy,
        "teleportCluster":connection.teleport_cluster,
        "nodeTcpPort":connection.node_tcp_port,"runAsUser":connection.run_as_user});
    if let Some(existing) = existing.and_then(Value::as_object) {
        for (key,value) in existing { if entry.get(key).is_none() { entry[key] = value.clone(); } }
    }
    entry
}

/// Дописать узел в `~/.jarvis/settings.json`.
///
/// Формат и способ записи — как у остального settings-кода (`crate::settings`):
/// весь файл целиком, `to_string_pretty` + перевод строки, права 0600, tmp+rename.
/// Не переиспользуем сам `Store` по прозаической причине: `jarvis-setup`
/// собирается без остального крейта, а `Store` тянет `util`/`log`.
fn record(name: &str, ssh_host: &Connection, dir: &str) -> Result<Recorded, String> {
    let path = super::jarvis_settings_path();
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw, Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("не прочитал {}: {error}", path.display())),
    };
    let mut root: Value = if raw.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&raw)
            .map_err(|_| format!("{} — невалидный JSON, не трогаю", path.display()))?
    };
    let Some(obj) = root.as_object_mut() else {
        return Err(format!("{} — не объект, не трогаю", path.display()));
    };
    let list = obj.entry("remotes").or_insert_with(|| json!([]));
    if !list.is_array() { return Err("Настройки remotes повреждены; файл не изменён".into()); }
    let arr = list.as_array_mut().unwrap();
    let same = arr
        .iter()
        .position(|r| r.get("name").and_then(Value::as_str) == Some(name));
    let entry = connection_record(name, ssh_host, dir, same.and_then(|index| arr.get(index)));
    let what = match same {
        // повторный `remote add` — это правка узла, а не второй узел с тем же
        // именем: дубли по имени ноут всё равно отбрасывает
        Some(i) => {
            arr[i] = entry;
            Recorded::Updated
        }
        None => {
            arr.push(entry);
            Recorded::Added
        }
    };
    let body = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())? + "\n";
    super::atomic_write_mode(&path, &body, 0o600)
        .map_err(|e| format!("не записал {}: {e}", path.display()))?;
    Ok(what)
}

/// Узел по имени из настроек: `(ssh-хост, каталог)`.
fn from_settings(name: &str) -> Result<(Connection, String), String> {
    let path = super::jarvis_settings_path();
    let raw = fs::read_to_string(&path)
        .map_err(|_| format!("нет {} — узлов ещё не заводили", path.display()))?;
    let root: Value = serde_json::from_str(&raw)
        .map_err(|_| format!("{} — невалидный JSON", path.display()))?;
    let node = root
        .get("remotes")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter()
                .find(|r| r.get("name").and_then(Value::as_str) == Some(name))
        })
        .ok_or_else(|| {
            format!("узла «{name}» нет в настройках — заведи его: jarvis-setup remote add {name} <ssh-хост>")
        })?;
    let host = node
        .get("sshHost")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if host.is_empty() {
        return Err(format!("у узла «{name}» не задан sshHost"));
    }
    let dir = node
        .get("jarvisDir")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(DEFAULT_DIR)
        .to_string();
    let connection: Connection = serde_json::from_value(node.clone()).map_err(|e| format!("Некорректное подключение: {e}"))?;
    connection.validate()?;
    Ok((connection, dir))
}

/* ================= команды ================= */

/// `jarvis-setup remote add <name> <ssh-host> [--dir <путь>]`.
/// `tcp` — порт узла на петле (`None` — не поднимать). По умолчанию включён:
/// без него мобильный клиент к узлу не подключится вовсе, а форвард на
/// unix-сокет SSH-библиотеки под Android не умеют. Выключается `--no-tcp`.
pub fn add(
    progress: &Progress,
    name: &str,
    ssh_host: &str,
    dir: Option<&str>,
    tcp: Option<u16>,
) -> Result<(), String> {
    add_connection(progress, name, &Connection::ssh(ssh_host), dir, tcp)
}

pub fn add_connection(
    progress: &Progress, name: &str, ssh_host: &Connection,
    dir: Option<&str>, tcp: Option<u16>,
) -> Result<(), String> { add_connection_sources(progress, name, ssh_host, dir, tcp, &[]) }

pub fn add_connection_sources(
    progress: &Progress, name: &str, ssh_host: &Connection,
    dir: Option<&str>, tcp: Option<u16>, explicit_sources: &[Value],
) -> Result<(), String> {
    let name = name.trim();
    ssh_host.validate()?;
    check_name(name)?;
    if (ssh_host.transport == "teleport" && ssh_host.node_tcp_port.is_none()) || (ssh_host.node_tcp_port.is_some() && tcp != ssh_host.node_tcp_port) {
        return Err("Укажи один и тот же TCP-порт для узла и SSH/Teleport-подключения".into());
    }
    let dir_raw = dir
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(DEFAULT_DIR)
        .trim_end_matches('/')
        .to_string();
    if dir_raw.chars().any(char::is_control) { return Err("Каталог узла содержит управляющие символы".into()); }
    if Path::new(&dir_raw).components().any(|part| matches!(part,std::path::Component::ParentDir))
        || dir_raw.starts_with('/') && !Path::new(&dir_raw).components().any(|part| matches!(part,std::path::Component::Normal(_)))
        || dir_raw.starts_with('~') && dir_raw != "~" && !dir_raw.starts_with("~/") {
        return Err("Укажи прямой путь каталога узла без .. или ~другого-пользователя".into());
    }
    // Относительный путь ssh не переварит: `-L порт:путь` он отдаёт удалённому
    // sshd как есть, и «jarvis/node.sock» зависит от того, где тот оказался.
    if !dir_raw.starts_with('/') && !dir_raw.starts_with('~') {
        return Err(format!(
            "каталог «{dir_raw}» должен начинаться с ~ или / — относительный путь \
             ssh-туннелю не годится"
        ));
    }

    // 1. Связь. Она же разведка: один заход вместо семи.
    progress(Step::start(PHASE_LINK));
    let mut remote = probe(ssh_host)?;
    let dir = remote.expand(&dir_raw);
    progress(Step::done(
        PHASE_LINK,
        format!("{ssh_host}: {} {}, $HOME={}", remote.os, remote.arch, remote.home),
    ));
    prepare_runtime(progress, ssh_host, &mut remote)?;

    // 2. Окружение. Всё здесь — предупреждения: узел ставится и на голую машину,
    // агента и tmux можно доставить позже, а вот curl критичен — без него шим
    // не сможет доставить в узел ни одного события.
    progress(Step::start(PHASE_ENV));
    if remote.has("tmux") {
        progress(Step::done(PHASE_ENV, "tmux есть"));
    } else {
        progress(Step::warn(
            PHASE_ENV,
            "tmux не найден — события и уведомления работать будут, а ответ в сессию \
             и пульт нет (apt install tmux / dnf install tmux)",
        ));
    }
    if !remote.has("curl") {
        progress(Step::warn(
            PHASE_ENV,
            "curl не найден — jarvis-hook отправляет события только им, без curl узел \
             не получит НИЧЕГО (apt install curl)",
        ));
    }
    match (
        remote.has("claude") || remote.has("claude-home"),
        remote.has("codex") || remote.has("codex-home"),
    ) {
        (false, false) => progress(Step::warn(
            PHASE_ENV,
            "ни claude, ни codex не нашёл (или они не в PATH неинтерактивного ssh) — \
             узел поставится, агента можно доставить потом: хуки уже будут ждать",
        )),
        (c, x) => progress(Step::done(
            PHASE_ENV,
            format!(
                "агенты: {}",
                [(c, "claude"), (x, "codex")]
                    .iter()
                    .filter(|(found, _)| *found)
                    .map(|(_, n)| *n)
                    .collect::<Vec<_>>()
                    .join(" + ")
            ),
        )),
    }

    // Каталог узла не должен совпадать с каталогом ЛОКАЛЬНОГО Jarvis той
    // машины: узел кладёт туда свой `bin/jarvis-hook` (тот стучится в
    // node.sock вместо run.sock) и молча ломает её собственную интеграцию.
    // Дешевле отказать и попросить другой каталог, чем чинить это потом.
    if let Ok(out) = run_ssh(
        ssh_host,
        &format!(
            "d={q}\n[ -d \"$d/shims\" ] && [ ! -f \"$d/provider-roots.json\" ] && printf 'host=yes\\n'\n[ -S \"$d/run.sock\" ] && printf 'host=yes\\n'\nexit 0",
            q = sh_quote(&dir)
        ),
    ) {
        if out.contains("host=yes") {
            return Err(format!(
                "в {dir} уже живёт свой Jarvis (там shims/ или run.sock). Узел кладёт \
                 туда свой jarvis-hook и сломал бы её собственную интеграцию.\n\
                 Возьми другой каталог, например ~/jarvis-node."
            ));
        }
    }

    // 3. Сам узел.
    progress(Step::start(PHASE_NODE));
    let triples = target_triples(&remote.os, &remote.arch);
    let sources = node_sources(&remote, &triples);
    if sources.is_empty() {
        return Err(build_hint(&remote, &triples, &node_candidates(&triples)));
    }
    deliver_node(progress, ssh_host, &dir, &remote, &sources)?;
    let hook_path = format!("{dir}/bin/jarvis-hook");
    put_file(ssh_host, &hook_path, node_hook_src()?.as_bytes(), Some("755"), false)?;
    progress(Step::done(PHASE_NODE, format!("{hook_path} (→ {dir}/node.sock)")));

    // 4. Хуки агентов — той же формы, что ставит локальная установка.
    progress(Step::start(PHASE_HOOKS));
    let sources = install_sources(ssh_host, &remote, &dir, explicit_sources)?;
    for source in &sources {
        let agent = source["agent"].as_str().unwrap();
        let home = source["providerHome"].as_str().unwrap();
        if agent == "codex" && !remote.has("codex") && !remote.has("codex-home") && home == remote.codex_home { continue; }
        let (file, events): (&str, &[(&str,&str)]) = if agent == "codex" { ("hooks.json", &super::CODEX_EVENTS) } else { ("settings.json", &super::EVENTS) };
        remote_hooks(progress, ssh_host, &format!("{home}/{file}"), agent, events, &hook_path)?;
    }
    install_transport(progress, ssh_host, &remote, &dir)?;
    // File installation alone does not enable current Codex hooks: the CLI
    // must acknowledge only these exact user hooks using its own key/hash.
    // A stale binary must not interpret an unknown flag as daemon startup.
    let repair = format!("set -e\nd={}\nif ! \"$d/bin/jarvis-node\" --help | grep -q -- --repair-hooks; then echo 'Нужен обновлённый jarvis-node с поддержкой доверия Codex hooks' >&2; exit 1; fi\nJARVIS_DIR=\"$d\" \"$d/bin/jarvis-node\" --repair-hooks 1>&2", sh_quote(&dir));
    if remote.has("codex") {
        run_ssh(ssh_host, &repair).map_err(|error| format!("Не удалось подтвердить хуки Codex: {error}"))?;
        progress(Step::done(PHASE_HOOKS, "Codex подтвердил доверие к установленным хукам Jarvis"));
    } else if remote.has("codex-home") {
        progress(Step::warn(PHASE_HOOKS, "История Codex найдена, но Codex CLI недоступен этому пользователю. Уведомления Codex потребуют настройки доверия после установки CLI; Claude и просмотр истории доступны."));
    }

    // 5. Автозапуск.
    progress(Step::start(PHASE_BOOT));
    let supervised = install_service(progress, ssh_host, &remote, &dir, tcp);

    // 6. Проверка: узел должен ответить своей версией и открыть сокет.
    progress(Step::start(PHASE_CHECK));
    let check = format!(
        "set -u\nd={q}\nsleep 1\nif [ -S \"$d/node.sock\" ]; then echo sock=yes; else echo sock=no; fi\necho \"version=$(\"$d/bin/jarvis-node\" --version 2>/dev/null)\"\nexit 0",
        q = sh_quote(&dir)
    );
    match run_ssh(ssh_host, &check) {
        Ok(out) => {
            let version = kv(&out, "version").unwrap_or_default();
            if version.is_empty() {
                progress(Step::warn(
                    PHASE_CHECK,
                    "бинарь не ответил на --version — проверь архитектуру и права",
                ));
            } else {
                progress(Step::done(PHASE_CHECK, version));
            }
            if kv(&out, "sock").as_deref() == Some("yes") {
                progress(Step::done(PHASE_CHECK, format!("сокет {dir}/node.sock открыт")));
            } else if supervised {
                progress(Step::warn(
                    PHASE_CHECK,
                    format!("сокета {dir}/node.sock нет — смотри journalctl --user -u {UNIT}"),
                ));
            } else {
                progress(Step::info(
                    PHASE_CHECK,
                    "сокета нет — узел ещё не запущен (автозапуск не настроен)",
                ));
            }
        }
        Err(e) => progress(Step::warn(PHASE_CHECK, format!("проверка не удалась: {e}"))),
    }

    verify_node(ssh_host, &dir)?;
    progress(Step::done(PHASE_CHECK, "Протокол 2 и доставка установленного хука подтверждены"));

    if let Some(port) = tcp {
        progress(Step::done(
            PHASE_BOOT,
            format!(
                "узел слушает и 127.0.0.1:{port} — для мобильного клиента. \
                 Наружу это ничего не открывает, но к порту на петле может \
                 подключиться любой пользователь той машины: не нужен — ставь с --no-tcp"
            ),
        ));
    }

    // 7. Настройки ноута + памятка.
    progress(Step::start(PHASE_DONE));
    // Каталог сохраняем РАЗВЁРНУТЫМ. `~` в `-L` не раскрывает никто, и туннелю
    // пришлось бы спрашивать $HOME по ssh при каждом подъёме — лишняя точка
    // отказа там, где ответ уже получен разведкой и не меняется.
    let line = format!("{{ \"name\": \"{name}\", \"sshHost\": \"{ssh_host}\", \"jarvisDir\": \"{dir}\" }}");
    match record(name, ssh_host, &dir) {
        Ok(Recorded::Added) => progress(Step::done(
            PHASE_DONE,
            format!("узел записан в {}: {line}", super::jarvis_settings_path().display()),
        )),
        Ok(Recorded::Updated) => progress(Step::done(
            PHASE_DONE,
            format!("запись узла обновлена в {}: {line}", super::jarvis_settings_path().display()),
        )),
        Err(e) => return Err(format!("Узел установлен, но подключение не сохранено: {e}")),
    }
    progress(Step::info(
        PHASE_DONE,
        "если панель Jarvis открыта — перезапусти её: настройки живут в её кэше, \
         и она перезапишет файл своей копией",
    ));
    progress(Step::info(
        PHASE_DONE,
        format!("проверить связь: Настройки → «Удалённые» → «Проверить» либо jarvis-setup remote status {name}"),
    ));
    progress(Step::info(PHASE_DONE, "Открой новый терминал на узле: интерактивные Claude/Codex автоматически получают управляемый транспорт. Уже открытые сессии продолжают работать как раньше."));
    progress(Step::info(
        PHASE_DONE,
        "как это работает, что делать при обрывах и чем это ограничено — docs/remote.md",
    ));
    Ok(())
}

/// `jarvis-setup remote status <name>` — жив ли узел на той стороне.
pub fn status(progress: &Progress, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("нужно имя узла".into());
    }
    let (ssh_host, dir_raw) = from_settings(name)?;
    let phase = format!("Узел {name}");
    progress(Step::start(&phase));
    let remote = probe(&ssh_host)?;
    let dir = remote.expand(&dir_raw);
    progress(Step::info(&phase, format!("{ssh_host} · {dir}")));

    let script = format!(
        "set -u\nd={q}\nif [ -S \"$d/node.sock\" ]; then echo sock=yes; else echo sock=no; fi\n\
         echo \"version=$(\"$d/bin/jarvis-node\" --version 2>/dev/null)\"\n\
         echo \"pids=$(pgrep -x jarvis-node 2>/dev/null | tr '\\n' ' ')\"\n\
         echo \"unit=$(systemctl --user is-active {unit} 2>/dev/null)\"\n\
         if command -v tmux >/dev/null 2>&1; then echo \"panes=$(tmux -L jarvis list-panes -a 2>/dev/null | wc -l | tr -d ' ')\"; else echo panes=нет-tmux; fi\n\
         exit 0",
        q = sh_quote(&dir),
        unit = UNIT
    );
    let out = run_ssh(&ssh_host, &script)?;

    let pids = kv(&out, "pids").unwrap_or_default();
    if pids.trim().is_empty() {
        progress(Step::warn(&phase, "процесс: не запущен"));
    } else {
        progress(Step::done(&phase, format!("процесс: жив (pid {})", pids.trim())));
    }

    if kv(&out, "sock").as_deref() == Some("yes") {
        progress(Step::done(&phase, format!("сокет: {dir}/node.sock")));
    } else {
        // без сокета хуки уходят в никуда: шим проверяет `[ -S ]` и молча выходит
        progress(Step::warn(&phase, format!("сокет: нет ({dir}/node.sock)")));
    }

    match kv(&out, "version").unwrap_or_default() {
        v if v.is_empty() => progress(Step::warn(&phase, "версия: бинарь не ответил")),
        v => progress(Step::done(&phase, format!("версия: {v}"))),
    }

    match kv(&out, "unit").unwrap_or_default() {
        u if u.is_empty() => progress(Step::info(&phase, format!("systemd: юнита {UNIT} нет"))),
        u if u == "active" => progress(Step::done(&phase, format!("systemd: {UNIT} active"))),
        u => progress(Step::warn(&phase, format!("systemd: {UNIT} {u}"))),
    }

    match kv(&out, "panes").unwrap_or_default() {
        p if p == "нет-tmux" => progress(Step::warn(
            &phase,
            "tmux не установлен — ответ в сессию и пульт там работать не будут",
        )),
        p => progress(Step::info(&phase, format!("живых пан tmux -L jarvis: {p}"))),
    }
    Ok(())
}

#[cfg(test)]
mod error_text_tests {
    use super::*;

    #[test]
    fn sanitizer_removes_color_cursor_and_control_sequences_preserving_readable_text() {
        let raw = "\u{1b}[31mzsh: no matches found\u{1b}[0m\r\n/home/coder/.codex-*\u{7}\u{8}\u{0}\u{7f}\rследующая\tстрока\u{202e}";
        assert_eq!(clean_remote_error(raw.as_bytes()), "zsh: no matches found\n/home/coder/.codex-*\nследующая\tстрока");
        assert_eq!(clean_remote_error(b"before\x1b[2J\x1b[Hafter"), "beforeafter");
    }

    #[test]
    fn sanitizer_strips_terminal_strings_and_c1_sequences_without_showing_their_payloads() {
        let raw = "\u{1b}]0;terminal title\u{7}\u{1b}]8;;https://example.test/private\u{1b}\\visible\u{1b}]8;;\u{1b}\\\u{1b}Pprivate device data\u{1b}\\\u{9b}31m text\u{9b}0m";
        assert_eq!(clean_remote_error(raw.as_bytes()), "visible text");
        assert_eq!(clean_remote_error(b"message\x1b]unterminated hidden text"), "message");
    }

    #[test]
    fn sanitizer_bounds_scanning_and_output_on_utf8_boundaries() {
        let output = clean_remote_error("Ошибка ".repeat(20_000).as_bytes());
        assert!(output.len() <= 4096);
        assert!(output.starts_with("Ошибка "));
        assert!(output.ends_with("… (вывод сокращён)"));
        let hidden = format!("prefix\u{1b}]{}", "x".repeat(100_000));
        assert_eq!(clean_remote_error(hidden.as_bytes()), "prefix… (вывод сокращён)");
    }

    #[test]
    fn sanitizer_handles_invalid_utf8_and_incomplete_escape_sequences() {
        assert_eq!(clean_remote_error(b"bad \xff output\x1b["), "bad \u{fffd} output");
        assert_eq!(clean_remote_error(b"\x1b[31m\x1b[0m\x00"), "");
    }

    #[test]
    fn probe_script_failure_does_not_claim_an_authentication_failure() {
        let host = Connection { transport: "teleport".into(), ..Connection::ssh("coder@hermes") };
        let command = remote_command_error(&host, Some(1), b"\x1b[31mzsh: no matches found: /home/coder/.codex-*\x1b[0m");
        let error = remote_probe_error(&host, &command);
        assert!(error.contains("Не удалось проверить окружение на coder@hermes через Teleport"));
        assert!(error.contains("код 1"));
        assert!(error.contains("zsh: no matches found: /home/coder/.codex-*"));
        for misleading in ["подключиться", "вход", "ssh-copy-id", "ключ", "\u{1b}"] { assert!(!error.contains(misleading), "{error}"); }
    }

    #[test]
    fn command_errors_preserve_client_diagnostics_and_use_explicit_status_fallbacks() {
        let host = Connection::ssh("coder@hermes");
        let error = remote_command_error(&host, Some(255), b"Permission denied (publickey).");
        assert!(error.contains("SSH") && error.contains("код 255") && error.contains("Permission denied (publickey)."));
        assert_eq!(remote_command_error(&host, Some(1), b"\x1b[31m\x1b[0m"), "Команда через SSH завершилась с ошибкой (код 1).");
        assert!(remote_command_error(&host, None, b"").contains("без кода возврата"));
        assert!(remote_probe_error(&host, &"x".repeat(10_000)).len() <= 4096);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_bootstrap_preserves_ssh_config_and_rejects_teleport_before_side_effects() {
        let connection = Connection { ssh_config_file: Some("/tmp/private config".into()), run_as_user: Some("agent".into()), ..Connection::ssh("root@node") };
        let command = password_command(&connection).unwrap();
        let args: Vec<_> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert!(args.windows(2).any(|pair| pair == ["-F", "/tmp/private config"]));
        assert_eq!(args.last().unwrap(), "root@node");
        assert!(!args.iter().any(|arg| arg.contains("sudo")));
        assert!(args.contains(&"ControlPath=none".into()));
        let teleport = Connection { transport: "teleport".into(), ..Connection::ssh("user@node") };
        assert!(password_command(&teleport).is_err());
        assert!(authorize_key_connection(&|_| panic!("must not start"), &teleport, "unused", "ssh-ed25519 unused").is_err());
        assert!(password_command(&Connection::ssh("-oProxyCommand=bad")).is_err());
    }

    #[test]
    fn runtime_dependencies_only_install_missing_tools_with_available_privileges() {
        let mut machine = remote("/home/test");
        assert!(runtime_setup(&machine).missing.is_empty());
        machine.tools = vec!["curl".into(), "package-apt-get".into()];
        let plan = runtime_setup(&machine);
        assert_eq!(plan.missing, ["tmux"]);
        assert!(!plan.automatic);
        assert!(plan.command.unwrap().ends_with("--no-install-recommends tmux"));
        machine.tools.push("package-sudo".into());
        let plan = runtime_setup(&machine);
        assert!(plan.automatic);
        assert!(plan.command.unwrap().starts_with("sudo -n env "));
        machine.tools = vec!["package-apk".into(), "package-root".into()];
        let plan = runtime_setup(&machine);
        assert!(plan.automatic);
        assert_eq!(plan.command.as_deref(), Some("apk add --no-cache tmux curl"));
        machine.tools = vec!["package-brew".into(), "package-root".into()];
        assert!(!runtime_setup(&machine).automatic, "Homebrew must never run as root");
        machine.tools = vec!["package-brew".into()];
        assert!(runtime_setup(&machine).automatic);
        machine.tools.clear();
        let plan = runtime_setup(&machine);
        assert!(!plan.automatic && plan.command.is_none());
    }

    #[test]
    fn hook_points_at_node_socket() {
        let hook = node_hook_src().expect("шаблон сокета должен находиться");
        assert!(hook.contains("/node.sock}"), "шим должен стучаться в сокет узла");
        assert!(
            !hook.contains("/run.sock}"),
            "путь демона в шиме узла не должен остаться"
        );
        // комментарий про run.sock трогать не за чем — правим только код
        assert_eq!(hook.lines().count(), super::super::HOOK_SRC.lines().count());
    }

    #[test]
    fn sh_quote_survives_quotes_and_spaces() {
        assert_eq!(sh_quote("/home/bob/.jarvis"), "'/home/bob/.jarvis'");
        assert_eq!(sh_quote("/tmp/a b"), "'/tmp/a b'");
        // одинарная кавычка внутри — единственный опасный символ для sh
        assert_eq!(sh_quote("it's"), r#"'it'\''s'"#);
        assert_eq!(sh_quote("$(rm -rf /)"), "'$(rm -rf /)'");
    }

    fn remote(home: &str) -> Remote {
        Remote {
            home: home.into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            codex_home: format!("{home}/.codex"),
            provider_sources: vec![json!({"agent":"claude","providerHome":format!("{home}/.claude")}),json!({"agent":"codex","providerHome":format!("{home}/.codex")})],
            shell: "/bin/bash".into(),
            tools: vec!["tmux".into(), "curl".into()],
        }
    }

    /// Свой каталог под тест: кандидаты на бинарь узла ищутся в том числе
    /// относительно текущего, и мусор от соседнего теста сбивал бы выбор.
    fn sandbox(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jarvis-remote-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Копия `remote()` без инструментов — для проверок выбора источника узла.
    fn bare(tools: &[&str]) -> Remote {
        let mut r = remote("/home/bob");
        r.tools = tools.iter().map(|t| (*t).to_string()).collect();
        r
    }

    #[test]
    fn node_source_falls_back_from_download_to_build() {
        let triples = target_triples("linux", "x86_64");
        // This fallback test must not depend on packaged server binaries or
        // locally built cross-compilation artifacts on the test runner.
        let fallback = |tools: &[&str]| node_sources_with_bundle(&bare(tools), &triples, &[])
            .into_iter().filter(|source| !matches!(source, NodeSource::Local(_))).collect::<Vec<_>>();
        // curl есть → качаем готовый: это быстрее и не требует там rust
        let sources = fallback(&["curl", "cargo"]);
        match sources.first() {
            Some(NodeSource::Download(url)) => {
                assert!(url.contains("x86_64-unknown-linux-gnu"), "{url}");
                assert!(url.contains(env!("CARGO_PKG_VERSION")), "версия узла = версия приложения");
            }
            other => panic!("ждал скачивание, получил {other:?}"),
        }
        assert_eq!(sources.last(), Some(&NodeSource::Build));
        // без curl остаётся сборка на месте
        assert_eq!(fallback(&["cargo"]), vec![NodeSource::Build]);
        // не осталось ничего — вызывающий подставит развёрнутую инструкцию
        assert!(fallback(&["tmux"]).is_empty());
    }

    #[test]
    fn node_source_ignores_a_binary_for_the_wrong_platform() {
        // Главная ошибка установки: залить mac-сборку на Linux-VPS. Локальный
        // кандидат должен отсеиваться ДО заливки, а не всплывать в логе systemd.
        let dir = sandbox("wrong-arch");
        let bin = dir.join("jarvis-node");
        // Mach-O 64: magic feedfacf + cputype arm64
        fs::write(&bin, [0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01]).unwrap();
        std::env::set_var("JARVIS_NODE_BIN", &bin);
        let got = node_sources_with_bundle(&bare(&["curl"]), &target_triples("linux", "x86_64"), &[]);
        std::env::remove_var("JARVIS_NODE_BIN");
        assert!(!got.iter().any(|source| matches!(source, NodeSource::Local(path) if path == &bin)));
        assert!(
            got.iter().any(|source| matches!(source, NodeSource::Download(_))),
            "mac-бинарь не годится для linux — ждал скачивание, получил {got:?}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    const PACKAGED_TEST_ELF_X86: &[u8] = &[0x7f, b'E', b'L', b'F', 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x3e, 0];
    const PACKAGED_TEST_ELF_ARM: &[u8] = &[0x7f, b'E', b'L', b'F', 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xb7, 0];
    const PACKAGED_TEST_MACHO: &[u8] = &[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 1];

    #[test]
    fn packaged_node_installs_without_remote_curl_or_cargo() {
        const BUNDLE: &[(&str, &[u8])] = &[("x86_64-unknown-linux-gnu", PACKAGED_TEST_ELF_X86)];
        let sources = node_sources_with_bundle(&bare(&[]), &target_triples("linux", "x86_64"), BUNDLE);
        assert_eq!(sources, vec![NodeSource::Bundled("x86_64-unknown-linux-gnu", PACKAGED_TEST_ELF_X86)]);
    }

    #[test]
    fn packaged_node_skips_wrong_target_os_and_architecture() {
        const WRONG: &[(&str, &[u8])] = &[
            ("aarch64-apple-darwin", PACKAGED_TEST_ELF_X86),
            ("x86_64-unknown-linux-gnu", PACKAGED_TEST_MACHO),
            ("x86_64-unknown-linux-gnu", PACKAGED_TEST_ELF_ARM),
        ];
        const WITH_MATCH: &[(&str, &[u8])] = &[
            ("aarch64-apple-darwin", PACKAGED_TEST_ELF_X86),
            ("x86_64-unknown-linux-gnu", PACKAGED_TEST_MACHO),
            ("x86_64-unknown-linux-gnu", PACKAGED_TEST_ELF_ARM),
            ("x86_64-unknown-linux-gnu", PACKAGED_TEST_ELF_X86),
        ];
        let triples = target_triples("linux", "x86_64");
        let fallback = node_sources_with_bundle(&bare(&["cargo"]), &triples, WRONG);
        assert!(!fallback.iter().any(|source| matches!(source, NodeSource::Bundled(_, _))));
        assert!(fallback.contains(&NodeSource::Build));
        assert_eq!(node_sources_with_bundle(&bare(&[]), &triples, WITH_MATCH),
            vec![NodeSource::Bundled("x86_64-unknown-linux-gnu", PACKAGED_TEST_ELF_X86)]);
    }

    #[test]
    fn packaged_node_wins_over_a_release_with_the_same_package_version() {
        const BUNDLE: &[(&str, &[u8])] = &[("x86_64-unknown-linux-gnu", PACKAGED_TEST_ELF_X86)];
        let triples = target_triples("linux", "x86_64");
        let remote = bare(&["curl", "cargo"]);
        assert!(node_sources_with_bundle(&remote, &triples, &[]).contains(&NodeSource::Download(release_url(&triples[0]))));
        // Downloading the same semver could still replace current embedded
        // sources with an older published build. The bundle is the sole source.
        assert_eq!(node_sources_with_bundle(&remote, &triples, BUNDLE),
            vec![NodeSource::Bundled("x86_64-unknown-linux-gnu", PACKAGED_TEST_ELF_X86)]);
    }

    fn write_executable_fixture(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn activation_replaces_a_valid_candidate_atomically_with_quoted_paths() {
        use std::io::Read;
        use std::os::unix::fs::MetadataExt;
        let dir = sandbox("activate-valid ' space");
        let dst = dir.join("jarvis-node's current");
        let candidate = dir.join("jarvis-node's candidate");
        let previous = "#!/bin/sh\nprintf 'previous component\\n'\n";
        write_executable_fixture(&dst, previous);
        let mut open_previous = fs::File::open(&dst).unwrap();
        let candidate_body = format!("#!/bin/sh\n[ \"$#\" -eq 1 ] && [ \"$1\" = --version ] || exit 91\nprintf '%s\\n' 'jarvis-node {}'\n", env!("CARGO_PKG_VERSION"));
        write_executable_fixture(&candidate, &candidate_body);
        let candidate_inode = fs::metadata(&candidate).unwrap().ino();
        let result = Command::new("/bin/sh").arg("-c").arg(activate_node_script(candidate.to_str().unwrap(), dst.to_str().unwrap())).output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        assert!(!candidate.exists());
        assert_eq!(fs::read_to_string(&dst).unwrap(), candidate_body);
        assert_eq!(fs::metadata(&dst).unwrap().ino(), candidate_inode, "activation must rename the verified candidate");
        let mut still_open = String::new(); open_previous.read_to_string(&mut still_open).unwrap();
        assert_eq!(still_open, previous, "existing processes must keep the previous inode intact");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn activation_failure_preserves_the_existing_node_and_candidate() {
        use std::os::unix::fs::MetadataExt;
        let dir = sandbox("activate-invalid ' space");
        let dst = dir.join("jarvis-node's current");
        let candidate = dir.join("jarvis-node's candidate");
        let previous = "#!/bin/sh\nprintf 'existing component\\n'\n";
        write_executable_fixture(&dst, previous);
        let previous_inode = fs::metadata(&dst).unwrap().ino();
        for body in [
            "#!/bin/sh\nprintf 'jarvis-node 0.0.0-wrong\\n'\n".to_string(),
            format!("#!/bin/sh\nprintf 'jarvis-node {}\\n'\nexit 7\n", env!("CARGO_PKG_VERSION")),
            "#!/bin/sh\nexit 0\n".to_string(),
        ] {
            write_executable_fixture(&candidate, &body);
            let result = Command::new("/bin/sh").arg("-c").arg(activate_node_script(candidate.to_str().unwrap(), dst.to_str().unwrap())).output().unwrap();
            assert!(!result.status.success(), "invalid candidate activated: {body}");
            assert_eq!(fs::read_to_string(&dst).unwrap(), previous);
            assert_eq!(fs::metadata(&dst).unwrap().ino(), previous_inode);
            assert_eq!(fs::read_to_string(&candidate).unwrap(), body);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn embedded_node_sources_are_complete() {
        // include_str! молча возьмёт любой файл по пути: если крейт узла
        // переедет, сборка на той стороне сломается не здесь, а на VPS.
        let (name, cargo) = NODE_SRC[0];
        assert_eq!(name, "Cargo.toml");
        assert!(cargo.contains("name = \"jarvis-node\""), "это не манифест узла");
        assert!(NODE_SRC.iter().all(|(_, body)| !body.trim().is_empty()));
        assert!(
            NODE_SRC.iter().any(|(n, _)| *n == "src/main.rs"),
            "без main.rs cargo соберёт пустоту"
        );
    }

    #[test]
    #[ignore = "compiles a temporary offline package; run explicitly"]
    fn embedded_node_sources_build_offline() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("jarvis-portable-node-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        for (relative, body) in NODE_SRC {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, portable_node_source(relative, body)).unwrap();
        }
        let output = std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args(["check", "--offline", "--manifest-path"])
            .arg(root.join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", std::env::temp_dir().join("jarvis-portable-node-check-target"))
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_PROFILE_DEV_DEBUG", "0")
            .output().unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(output.status.success(), "portable package did not build offline: {}", String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    fn expand_resolves_tilde_and_trims_slash() {
        let r = remote("/home/bob");
        assert_eq!(r.expand("~/.jarvis"), "/home/bob/.jarvis");
        assert_eq!(r.expand("~"), "/home/bob");
        assert_eq!(r.expand("/srv/jarvis/"), "/srv/jarvis");
        assert_eq!(r.expand("~/jarvis/"), "/home/bob/jarvis");
    }

    #[test]
    fn tools_lookup_is_exact() {
        let r = remote("/home/bob");
        assert!(r.has("tmux"));
        assert!(!r.has("claude"));
        assert!(!r.has("tmu"), "подстрока — не признак наличия");
    }

    #[test]
    fn probe_output_parses() {
        let raw = "home=/home/bob\nos=Linux\narch=x86_64\nhave=tmux\nhave=curl\n";
        assert_eq!(kv(raw, "home").as_deref(), Some("/home/bob"));
        assert_eq!(kv(raw, "arch").as_deref(), Some("x86_64"));
        assert_eq!(kv(raw, "nope"), None);
        let tools: Vec<&str> = raw
            .lines()
            .filter_map(|l| l.split_once('='))
            .filter(|(k, _)| *k == "have")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(tools, ["tmux", "curl"]);
    }

    #[test]
    fn triples_normalize_arch_names() {
        assert_eq!(target_triples("linux", "amd64")[0], "x86_64-unknown-linux-gnu");
        assert_eq!(target_triples("linux", "aarch64")[0], "aarch64-unknown-linux-gnu");
        assert_eq!(target_triples("darwin", "arm64"), ["aarch64-apple-darwin"]);
        assert!(target_triples("freebsd", "x86_64").is_empty());
    }

    // Главная ошибка установки — залить mac-сборку на Linux-VPS.
    #[test]
    fn binary_kind_tells_elf_from_macho() {
        let mut elf = vec![0u8; 24];
        elf[..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        elf[5] = 1; // little-endian
        elf[18] = 0x3e;
        assert_eq!(binary_kind(&elf), Some(("linux", "x86_64")));

        let macho = [0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];
        assert_eq!(binary_kind(&macho), Some(("darwin", "aarch64")));

        let fat = [0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2];
        assert_eq!(binary_kind(&fat), Some(("darwin", "universal")));

        assert_eq!(binary_kind(b"#!/bin/sh\n"), None, "скрипт — не бинарь");
        assert_eq!(binary_kind(b""), None);
    }

    #[test]
    fn binary_fits_blocks_only_what_it_understands() {
        assert!(!binary_fits(Some(("darwin", "aarch64")), "linux", "x86_64"));
        assert!(!binary_fits(Some(("linux", "aarch64")), "linux", "x86_64"));
        assert!(binary_fits(Some(("linux", "x86_64")), "linux", "amd64"));
        assert!(binary_fits(Some(("darwin", "universal")), "darwin", "arm64"));
        // незнакомый формат/архитектура — не повод отказывать: вдруг запустится
        assert!(binary_fits(None, "linux", "x86_64"));
        assert!(binary_fits(Some(("linux", "неизвестная")), "linux", "riscv64"));
    }

    #[test]
    fn name_must_survive_a_file_path() {
        assert!(check_name("vps").is_ok());
        assert!(check_name("my-box_1.2").is_ok());
        // имя уходит и в ключ реестра `<remote>:<id>`, и в имя файла курсора
        assert!(check_name("../evil").is_err());
        assert!(check_name("a/b").is_err());
        assert!(check_name("..").is_err());
        assert!(check_name("").is_err());
        assert!(check_name("узел").is_err(), "кириллица в ключах ни к чему");
    }

    #[test]
    fn unit_quotes_paths_and_restarts_always() {
        let u = unit_text("/home/bob/.jarvis", Some(DEFAULT_TCP_PORT));
        assert!(u.contains("ExecStart=\"/home/bob/.jarvis/bin/jarvis-node\""));
        assert!(u.contains("Environment=\"JARVIS_DIR=/home/bob/.jarvis\""));
        // порт для телефона — в юните, и только на петле
        assert!(u.contains("Environment=\"JARVIS_NODE_TCP=127.0.0.1:7717\""));
        assert!(
            !unit_text("/home/bob/.jarvis", None).contains("JARVIS_NODE_TCP"),
            "--no-tcp обязан убирать строку целиком, а не выставлять пустое значение"
        );
        assert!(u.contains("Restart=always"));
        assert!(u.lines().any(|line| line == "KillMode=process"), "node restart must not kill active tmux agents");
        assert!(u.contains("WantedBy=default.target"));
    }

    #[test]
    fn service_scope_requires_a_working_manager_and_bounds_system_scope_to_root() {
        let cases: &[(&[&str], Option<&str>)] = &[
            (&[], None),
            (&["systemd"], None),
            (&["package-root"], None),
            (&["systemd-system"], None),
            (&["systemd-system", "package-sudo"], None),
            (&["systemd-system", "package-root"], Some("--system")),
            (&["systemd-user"], Some("--user")),
            (&["systemd-user", "systemd-system", "package-root"], Some("--user")),
        ];
        for (tools, expected) in cases {
            assert_eq!(service_scope(&bare(tools)), *expected, "capabilities: {tools:?}");
        }
    }

    #[test]
    fn system_service_preserves_the_agent_home_and_uses_system_boot_targets() {
        let unit = system_unit_text("/root/agent's node", r#"/root/agent\profile"100%"#, Some(7777));
        assert!(unit.contains(r#"Environment="HOME=/root/agent\\profile\"100%%""#), "{unit}");
        assert!(unit.contains("After=network.target"));
        assert!(unit.contains("WantedBy=multi-user.target"));
        assert!(!unit.contains("default.target"));
        assert!(unit.lines().any(|line| line == "User=root"));
        assert!(unit.lines().any(|line| line == "KillMode=process"));
        assert!(unit.contains("ExecStart=\"/root/agent's node/bin/jarvis-node\""));
        assert!(unit.contains("Environment=\"JARVIS_NODE_TCP=127.0.0.1:7777\""));
        assert!(!system_unit_text("/root/.jarvis", "/root", None).contains("JARVIS_NODE_TCP"));
        let user = unit_text("/home/agent/.jarvis", None);
        assert!(user.contains("WantedBy=default.target"));
        assert!(!user.contains("Environment=\"HOME="));
    }

    #[test]
    fn managed_path_block_preserves_foreign_profile_and_reinstalls_exactly() {
        let old = "# user configuration\nexport KEEP='unchanged'\n";
        let first = path_profile(old, "/home/some one/.jarvis").unwrap();
        assert!(first.starts_with(old));
        assert!(first.contains("export PATH='/home/some one/.jarvis/shims':\"$PATH\""));
        assert_eq!(first, path_profile(&first, "/home/some one/.jarvis").unwrap());
        assert!(path_profile(PATH_START, "/tmp/node").is_err());
    }
    #[test]
    fn probe_cli_lookup_skips_managed_shims_and_finds_real_underneath() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = sandbox("probe-cli");
        let shim = dir.join("managed shims");
        let real = dir.join("real cli");
        let utils = dir.join("tools");
        for path in [&shim, &real, &utils] { fs::create_dir_all(path).unwrap(); }
        for tool in ["dd", "grep"] {
            let source = [format!("/usr/bin/{tool}"), format!("/bin/{tool}")]
                .into_iter().find(|path| Path::new(path).is_file()).unwrap();
            symlink(source, utils.join(tool)).unwrap();
        }
        for tool in ["claude", "codex"] {
            fs::write(shim.join(tool), "#!/bin/sh\n# jarvis agent shim\nexit 99\n").unwrap();
            fs::set_permissions(shim.join(tool), fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(real.join(tool), "#!/bin/sh\nexit 98\n").unwrap();
            fs::set_permissions(real.join(tool), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let lookup = PROBE.split_once("JARVIS_TOOL_PROBE='").unwrap().1
            .split_once("\n'\n").unwrap().0;
        let run = |path: &str, suffix: &str| {
            let output = Command::new("/bin/sh").args(["-c", &format!("{lookup}\n{suffix}")])
                .env_clear().env("HOME", &dir).env("PATH", path).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            String::from_utf8(output.stdout).unwrap()
        };
        let path = format!("{}:{}", shim.display(), utils.display());
        let absent = run(&path, "");
        assert!(!absent.contains("have=codex\n") && !absent.contains("have=claude\n"));
        let present = run(&format!("{path}:{}", real.display()), "");
        assert!(present.contains("have=codex\n") && present.contains("have=claude\n"));
        // The known-directory pass uses the same file check, including symlinks.
        let alias = dir.join("codex-alias"); symlink(shim.join("codex"), &alias).unwrap();
        let direct = run(&path, &format!(
            "if jarvis_real_executable {} codex; then echo shim=yes; fi\nif jarvis_real_executable {} codex; then echo real=yes; fi",
            sh_quote(alias.to_str().unwrap()), sh_quote(real.join("codex").to_str().unwrap())
        ));
        assert!(!direct.contains("shim=yes")); assert!(direct.contains("real=yes"));
        // The login-shell path executes the very same lookup after importing its PATH.
        let output = Command::new("/bin/sh")
            .args(["-c", "exec /bin/sh -c \"$JARVIS_TOOL_PROBE\""])
            .env_clear().env("HOME", &dir).env("PATH", format!("{path}:{}", real.display()))
            .env("JARVIS_TOOL_PROBE", lookup).output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8(output.stdout).unwrap().contains("have=codex\n"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn provider_discovery_keeps_all_explicit_homes_but_rejects_broad_roots() {
        let got = parse_provider_sources("provider=codex|/home/me/.codex\nprovider=codex|/home/me/.codex-work\nprovider=claude|/srv/claude profile\nprovider=codex|/\nprovider=codex|relative\nprovider=unknown|/tmp/x\nprovider=codex|/home/me/.codex\n");
        assert_eq!(got.len(),3);
        assert!(got.iter().any(|row| row["providerHome"] == "/srv/claude profile"));
        assert!(parse_provider_sources("provider=codex|/tmp/..\nprovider=codex|/.\nprovider=codex|//\n").is_empty());
    }
    #[test]
    fn probe_provider_discovery_skips_only_automatic_backup_homes() {
        let dir = sandbox("probe-provider-backups");
        let backup_names = [".claude-backups", ".claude-work-backup-2026", ".codex-BACKUP", ".codex-personal_BaK.2026"];
        let profile_names = [".claude-work", ".claude-personal", ".claude-backupworks", ".claude-old", ".codex-work", ".codex-archive", ".codex-archives", ".codex-gold", ".codex-bakery"];
        for name in backup_names.iter().chain(profile_names.iter()) {
            fs::create_dir_all(dir.join(name)).unwrap();
        }
        fs::create_dir_all(dir.join(".claude-")).unwrap();
        fs::write(dir.join(".claude-not-a-directory"), "fixture").unwrap();
        let discovery = PROBE.split_once("\n# Ищем в три захода").unwrap().0;
        let run = |explicit: bool| {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", &format!("{discovery}\nexit 0")])
                .env_clear().env("HOME", &dir).env("PATH", "/usr/bin:/bin").env("SHELL", "/bin/sh");
            if explicit {
                command.env("CLAUDE_CONFIG_DIR", dir.join(".claude-backups"))
                    .env("CODEX_HOME", dir.join(".codex-BACKUP"));
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            parse_provider_sources(&String::from_utf8(output.stdout).unwrap())
        };
        let automatic = run(false);
        for name in backup_names {
            assert!(!automatic.iter().any(|row| row["providerHome"] == dir.join(name).to_str().unwrap()), "backup discovered as profile: {name}");
        }
        for name in profile_names {
            assert!(automatic.iter().any(|row| row["providerHome"] == dir.join(name).to_str().unwrap()), "legitimate profile was filtered: {name}");
        }
        assert_eq!(automatic.len(), profile_names.len() + 2, "unexpected profile or non-directory candidate");
        let explicit = run(true);
        for name in [".claude-backups", ".codex-BACKUP"] {
            assert!(explicit.iter().any(|row| row["providerHome"] == dir.join(name).to_str().unwrap()), "explicit environment home was filtered: {name}");
        }
        // Manifest entries use this same parser, not the automatic-name filter.
        assert_eq!(parse_provider_sources(&format!("provider=claude|{}", dir.join(".claude-backups").display())).len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn embedded_node_contains_every_declared_module() {
        let node = NODE_SRC.iter().find(|(name,_)| *name == "src/node/mod.rs").unwrap().1;
        for line in node.lines().filter_map(|line| line.strip_prefix("pub mod ")) {
            let name = format!("src/node/{}.rs",line.trim_end_matches(';'));
            assert!(NODE_SRC.iter().any(|(path,_)| *path == name), "embedded node is missing {name}");
        }
        let main = NODE_SRC.iter().find(|(name,_)| *name == "src/main.rs").unwrap().1;
        let portable = portable_node_source("src/main.rs", main);
        assert!(portable.contains("use jarvis_node_shared::{codex_hooks, terminal_stream};"));
        for name in ["shared/Cargo.toml", "shared/src/lib.rs", "shared/src/codex_hooks.rs", "shared/src/terminal_stream.rs"] {
            assert!(NODE_SRC.iter().any(|(path,_)| *path == name), "missing {name}");
        }
        let manifest = portable_node_source("Cargo.toml", NODE_SRC[0].1);
        assert!(manifest.contains("path = \"shared\""));
        assert!(!manifest.contains("../shared"));
    }

    #[test]
    fn remote_shim_and_hook_agree_on_node_socket() {
        let shim = node_shim_src().unwrap();
        assert!(shim.contains("JARVIS_SOCK=${JARVIS_SOCK:-$JARVIS_DIR/node.sock}"));
        assert!(!shim.contains("JARVIS_SOCK=${JARVIS_SOCK:-$JARVIS_DIR/run.sock}"));
        assert!(node_hook_src().unwrap().contains("/node.sock}"));
    }

    #[test]
    fn reinstall_clears_old_transport_and_owner_but_preserves_foreign_metadata() {
        let old = json!({"name":"vm","sshHost":"root@old","jarvisDir":"/old","transport":"teleport",
            "sshConfigFile":"/old/ssh.config","teleportProxy":"old.proxy","teleportCluster":"old.cluster","nodeTcpPort":7717,
            "runAsUser":"old-owner","origin":"agent-vm","vmName":"fixture","custom":{"keep":true}});
        let next = connection_record("vm", &Connection::ssh("new-host"), "/new", Some(&old));
        let connection: Connection = serde_json::from_value(next.clone()).unwrap();
        assert_eq!(connection.ssh_host,"new-host"); assert_eq!(connection.transport,"ssh");
        assert!(connection.run_as_user.is_none() && connection.ssh_config_file.is_none());
        assert!(connection.teleport_proxy.is_none() && connection.teleport_cluster.is_none() && connection.node_tcp_port.is_none());
        assert_eq!(next["origin"],"agent-vm"); assert_eq!(next["vmName"],"fixture");
        assert_eq!(next["custom"],old["custom"]); assert_eq!(next["jarvisDir"],"/new");
        assert_eq!(connection_record("vm", &connection, "/new", Some(&next)), next);
    }

    #[test]
    #[ignore = "requires an explicitly selected running Linux SSH QA fixture; only writes its /tmp directory and a uniquely named runtime unit"]
    fn real_linux_install_components_are_idempotent() {
        let root = std::env::var("JARVIS_QA_GUEST_ROOT").expect("select existing guest fixture");
        assert!(root.starts_with("/tmp/jarvis-linux-qa-") && root.split('/').count() == 3);
        let connection = Connection { ssh_config_file: Some(std::env::var("JARVIS_QA_SSH_CONFIG").unwrap()),
            ..Connection::ssh(&std::env::var("JARVIS_QA_SSH_HOST").unwrap()) };
        let owner = format!("{root}/installer owner"); let dir = format!("{root}/installed node");
        let hook = format!("{dir}/bin/jarvis-hook");
        let personal = format!("{owner}/.codex-personal"); let work = format!("{owner}/.codex-work");
        let remote = Remote { home:owner.clone(), os:"linux".into(), arch:"aarch64".into(), codex_home:personal.clone(),
            provider_sources:vec![json!({"agent":"codex","providerHome":personal}),json!({"agent":"codex","providerHome":work})],
            shell:"/bin/bash".into(),tools:vec!["codex".into(),"tmux".into()] };
        let progress = |_: Step| {};
        let foreign = "# user's profile\nexport FOREIGN_VALUE='preserved'\n";
        put_file(&connection,&format!("{owner}/.profile"),foreign.as_bytes(),Some("600"),false).unwrap();
        put_file(&connection,&hook,node_hook_src().unwrap().as_bytes(),Some("755"),false).unwrap();
        for home in [&personal,&work] {
            put_file(&connection,&format!("{home}/hooks.json"),b"{\"foreign\":true,\"hooks\":{\"Stop\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"echo FOREIGN_FIXTURE\"}]}]}}",Some("600"),false).unwrap();
        }
        let install = || {
            let sources = install_sources(&connection,&remote,&dir,&[]).unwrap(); assert_eq!(sources.len(),2);
            for home in [&personal,&work] { remote_hooks(&progress,&connection,&format!("{home}/hooks.json"),"codex",&super::super::CODEX_EVENTS,&hook).unwrap(); }
            install_transport(&progress,&connection,&remote,&dir).unwrap();
        };
        install();
        let snapshot = || run_ssh(&connection,&format!("find {} {} -type f -exec sha256sum '{{}}' + | sort",sh_quote(&owner),sh_quote(&dir))).unwrap();
        let first = snapshot(); install(); assert_eq!(snapshot(),first,"second install changed content or created extra backups");
        for home in [&personal,&work] {
            let text = run_ssh(&connection,&format!("cat {}",sh_quote(&format!("{home}/hooks.json")))).unwrap();
            let value: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["foreign"],true); assert!(text.contains("echo FOREIGN_FIXTURE"));
            assert_eq!(value["hooks"]["Stop"].as_array().unwrap().len(),2);
        }
        assert!(run_ssh(&connection,&format!("cat {}",sh_quote(&format!("{owner}/.profile")))).unwrap().starts_with(foreign));
        // The real production unit body runs under a unique *runtime* link.
        // Drop always removes only that unit, even after a failed assertion.
        let unit = format!("jarvis-qa-install-{}.service",root.rsplit('-').next().unwrap().to_lowercase());
        struct UnitGuard(Connection,String);
        impl Drop for UnitGuard { fn drop(&mut self) { let _ = run_ssh(&self.0,&format!("systemctl --user disable --runtime --now {} >/dev/null 2>&1 || true",sh_quote(&self.1))); } }
        let _guard = UnitGuard(connection.clone(),unit.clone());
        run_ssh(&connection,&format!("cp {} {}",sh_quote(&format!("{root}/portable/target/debug/jarvis-node")),sh_quote(&format!("{dir}/bin/jarvis-node")))).unwrap();
        let unit_path = format!("{root}/{unit}");
        let body = unit_text(&dir,None).replace("Type=simple\n",&format!("Type=simple\nEnvironment=\"HOME={owner}\"\n"));
        put_file(&connection,&unit_path,body.as_bytes(),Some("600"),false).unwrap();
        run_ssh(&connection,&format!("systemctl --user link --runtime {} && systemctl --user start {}",sh_quote(&unit_path),sh_quote(&unit))).unwrap();
        let mut verified = false;
        for _ in 0..30 { if verify_node(&connection,&dir).is_ok() { verified = true; break; } std::thread::sleep(std::time::Duration::from_millis(200)); }
        assert!(verified,"production unit + installed hook failed end-to-end");
    }

}
