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
fn ssh_cmd(host: &str) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
    cmd.arg(host); // хост строго после опций: после него ssh их уже не примет
    cmd
}

/// Выполнить скрипт на той стороне и забрать stdout.
///
/// Скрипт уезжает ОДНИМ элементом argv — локальный шелл его не видит вовсе,
/// поэтому кавычки внутри можно ставить свободно; интерполировать чужие строки
/// всё равно только через [`sh_quote`].
fn run_ssh(host: &str, script: &str) -> Result<String, String> {
    let out = ssh_cmd(host)
        .arg(script)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("не смог запустить ssh: {e} (ssh вообще установлен?)"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if err.is_empty() {
        format!("ssh вернул код {}", out.status.code().unwrap_or(-1))
    } else {
        err
    })
}

/// Выполнить скрипт, скормив ему `data` в stdin (так заливаются файлы).
///
/// Дедлока «пишем в stdin, а ребёнок захлебнулся в своём stdout» здесь нет:
/// скрипты на том конце пишут в stdout ноль байт, а в stderr — считанные строки,
/// то есть заведомо меньше буфера трубы.
fn send_ssh(host: &str, script: &str, data: &[u8]) -> Result<(), String> {
    let mut child = ssh_cmd(host)
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("не смог запустить ssh: {e}"))?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "ssh не дал stdin".to_string())?;
        stdin
            .write_all(data)
            .map_err(|e| format!("не смог передать данные по ssh: {e}"))?;
        // закрываем явно (drop в конце блока): без EOF `cat` на той стороне
        // будет ждать вечно, и установка повиснет без единого сообщения
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("ssh не завершился: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if err.is_empty() {
        format!("ssh вернул код {}", out.status.code().unwrap_or(-1))
    } else {
        err
    })
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
const PROBE: &str = r#"printf 'home=%s\n' "$HOME"
printf 'os=%s\n' "$(uname -s 2>/dev/null)"
printf 'arch=%s\n' "$(uname -m 2>/dev/null)"
printf 'codex_home=%s\n' "${CODEX_HOME:-$HOME/.codex}"

# Ищем в три захода, и это не перестраховка: неинтерактивный ssh получает
# урезанный PATH — без nvm, ~/.local/bin и homebrew. Claude Code почти всегда
# оказывается ровно там, поэтому одного `command -v` мало: он честно отвечает
# «нет» про установленный агент.
WANT="tmux curl claude codex cargo"

# 1. PATH как есть.
for b in $WANT systemctl; do
  command -v "$b" >/dev/null 2>&1 && printf 'have=%s\n' "$b"
done

# 2. Известные места установки. Дубли не мешают: ноут проверяет вхождение.
for p in "$HOME/.local/bin" "$HOME/bin" "$HOME/.cargo/bin" "$HOME/.bun/bin" \
         "$HOME/.claude/local" "$HOME/.npm-global/bin" "$HOME/.local/share/pnpm" \
         /usr/local/bin /opt/homebrew/bin /snap/bin; do
  for b in $WANT; do
    [ -x "$p/$b" ] && printf 'have=%s\n' "$b"
  done
done

# 3. Логин-шелл — он прочитает профиль и подхватит nvm/fnm/asdf/mise. Под
# таймаутом: чужой профиль может ждать ввода или уходить в сеть, а разведка
# зависать не имеет права.
LSH="${SHELL:-/bin/sh}"
T=""
command -v timeout >/dev/null 2>&1 && T="timeout 10"
if [ -x "$LSH" ]; then
  $T "$LSH" -lc 'for b in tmux curl claude codex cargo; do command -v $b >/dev/null 2>&1 && printf "have=%s\n" "$b"; done' 2>/dev/null
fi

[ -d "${CODEX_HOME:-$HOME/.codex}" ] && printf 'have=%s\n' codex-home
[ -d "$HOME/.claude" ] && printf 'have=%s\n' claude-home
systemctl --user show-environment >/dev/null 2>&1 && printf 'have=%s\n' systemd-user
exit 0"#;

fn probe(host: &str) -> Result<Remote, String> {
    let raw = run_ssh(host, PROBE).map_err(|e| {
        format!(
            "не достучался до {host}: {e}\n\
             Проверь руками:  ssh {host} true\n  \
             • ключ не настроен → ssh-copy-id {host} (или добавь свой публичный ключ\n    \
               в ~/.ssh/authorized_keys на той машине);\n  \
             • ключ с пассфразой → загрузи его в агент: ssh-add;\n  \
             • хост ещё не в known_hosts → зайди один раз руками и подтверди отпечаток\n    \
               (BatchMode специально не принимает чужие ключи молча);\n  \
             • нестандартный порт/пользователь → опиши алиас в ~/.ssh/config и передавай его\n    \
               вместо адреса: Host {host} / HostName … / User … / Port …"
        )
    })?;
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
        home,
        tools: raw
            .lines()
            .filter_map(|l| l.split_once('='))
            .filter(|(k, _)| *k == "have")
            .map(|(_, v)| v.trim().to_string())
            .collect(),
    })
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
    let ssh_host = ssh_host.trim();
    let key = public_key.trim();
    if ssh_host.is_empty() {
        return Err("нужен ssh-хост".into());
    }
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
    ssh_with_password(ssh_host, password, &script)?;
    progress(Step::done(PHASE_LINK, "ключ добавлен в ~/.ssh/authorized_keys"));

    // Проверяем именно то, чем будем пользоваться дальше: вход по ключу без
    // пароля. Успешная запись ключа ещё не значит, что sshd его примет —
    // PubkeyAuthentication может быть выключен, а домашний каталог доступен
    // на запись группе (тогда sshd молча игнорирует authorized_keys).
    run_ssh(ssh_host, "true").map_err(|e| {
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
fn ssh_with_password(host: &str, password: &str, script: &str) -> Result<String, String> {
    let helper = write_askpass()?;
    let out = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=no",
            // Иначе ssh перебирает ключи, упирается в отказ и до пароля не
            // доходит — а мы сюда попали именно потому, что ключи не приняты.
            "-o",
            "PubkeyAuthentication=no",
            "-o",
            "PreferredAuthentications=password,keyboard-interactive",
            "-o",
            "NumberOfPasswordPrompts=1",
            // accept-new, а не «yes»: новый хост принимаем (человек только что
            // ввёл для него пароль — он знает, куда идёт), а вот СМЕНУ
            // известного ключа по-прежнему отвергаем. Именно смена, а не первое
            // знакомство, — признак подмены.
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "ConnectTimeout=15",
        ])
        .arg(host)
        .arg(script)
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
}

/// Сходить на машину и рассказать, что там. Ошибка — только недоступность:
/// нехватка tmux, агента или curl это состояние машины, а не отказ.
pub fn preflight(ssh_host: &str, dir: Option<&str>) -> Result<Preflight, String> {
    let ssh_host = ssh_host.trim();
    if ssh_host.is_empty() {
        return Err("нужен ssh-хост".into());
    }
    let remote = probe(ssh_host)?;
    let triples = target_triples(&remote.os, &remote.arch);
    // Текст — для человека, а не для лога: ссылку целиком тут показывать незачем
    // (она длинная и в строку панели не влезает), важно откуда и подо что.
    let (node_source, node_note) = match resolve_node(&remote, &triples) {
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
                "скачаю прямо на ту машину из релиза v{} (сборка {}) — там есть curl, собирать ничего не придётся",
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
        systemd: remote.has("systemd-user"),
        cargo: remote.has("cargo"),
        os: remote.os,
        arch: remote.arch,
        home: remote.home,
        node_source,
        node_note,
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
    let mut out = Vec::new();
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
    if let Some(triple) = triples.first() {
        if remote.has("curl") {
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
const NODE_SRC: [(&str, &str); 7] = [
    ("Cargo.toml", include_str!("../../node/Cargo.toml")),
    ("src/main.rs", include_str!("../../node/src/main.rs")),
    ("src/node/mod.rs", include_str!("../../node/src/node/mod.rs")),
    ("src/node/ring.rs", include_str!("../../node/src/node/ring.rs")),
    ("src/node/files.rs", include_str!("../../node/src/node/files.rs")),
    ("src/node/http.rs", include_str!("../../node/src/node/http.rs")),
    ("src/node/tmux.rs", include_str!("../../node/src/node/tmux.rs")),
];

/// Положить `jarvis-node` в `<dir>/bin` тем способом, который выбрал
/// [`resolve_node`]. Все три пути заканчиваются одинаково: рабочий бинарь на
/// боевом месте — и проверяются тоже одинаково, запуском `--version`.
fn deliver_node(
    progress: &Progress,
    host: &str,
    dir: &str,
    remote: &Remote,
    sources: &[NodeSource],
) -> Result<(), String> {
    let dst = format!("{dir}/bin/jarvis-node");
    let mut why: Vec<String> = Vec::new();
    for (i, src) in sources.iter().enumerate() {
        match try_source(progress, host, dir, remote, &dst, src) {
            Ok(()) => return Ok(()),
            Err(e) => {
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
    host: &str,
    dir: &str,
    remote: &Remote,
    dst: &str,
    src: &NodeSource,
) -> Result<(), String> {
    let dst = dst.to_string();
    match src {
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
            progress(Step::info(PHASE_NODE, format!("качаю на той стороне: {url}")));
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
    progress(Step::done(PHASE_NODE, out.trim().to_string()));
    Ok(())
}

/// Скачать бинарь на удалённой машине.
///
/// `-f` обязателен: без него curl бодро сохраняет страницу «404 Not Found» под
/// именем узла, и ошибка всплыла бы уже в логе systemd, а не здесь.
fn download_node(host: &str, dst: &str, url: &str) -> Result<(), String> {
    let script = format!(
        r#"set -e
f={dst}
mkdir -p "$(dirname "$f")"
t="$f.jarvis-new.$$"
curl -fsSL --max-time 300 -o "$t" {url}
chmod 755 "$t"
mv -f "$t" "$f"
"#,
        dst = sh_quote(dst),
        url = sh_quote(url),
    );
    run_ssh(host, &script).map(|_| ()).map_err(|e| {
        format!(
            "не скачался {url}: {e}\n\
             Релиза этой версии может ещё не быть. Тогда: поставить на ту машину rust \
             (узел соберётся там сам) или собрать бинарь самому и указать его через \
             JARVIS_NODE_BIN."
        )
    })
}

/// Собрать узел из вшитых исходников прямо на той машине.
///
/// Без `--locked`: lock-файла у нас с собой нет (в репозитории он общий на весь
/// воркспейс приложения и этому крейту не подходит), поэтому cargo разрешает
/// версии сам — зависимостей три, и все с полуоткрытыми границами.
fn build_node(host: &str, dir: &str, dst: &str) -> Result<(), String> {
    let src_dir = format!("{dir}/src/jarvis-node");
    // Каталог пересоздаём: остатки прошлой попытки (или другой версии узла)
    // дали бы сборку неизвестно чего.
    run_ssh(
        host,
        &format!("set -e\nrm -rf {d}\nmkdir -p {d}/src/node\n", d = sh_quote(&src_dir)),
    )
    .map_err(|e| format!("не подготовил каталог сборки: {e}"))?;
    for (rel, body) in NODE_SRC {
        put_file(host, &format!("{src_dir}/{rel}"), body.as_bytes(), Some("644"), false)?;
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
    host: &str,
    path: &str,
    data: &[u8],
    mode: Option<&str>,
    backup: bool,
) -> Result<(), String> {
    let q = sh_quote(path);
    let mut script = format!(
        "set -e\nf={q}\nmkdir -p \"$(dirname \"$f\")\"\nt=\"$f.jarvis-new.$$\"\n"
    );
    if backup {
        // бэкап перед записью — тот же принцип, что у локальной установки;
        // `cp -p` заодно клонирует права, поэтому дальше их можно не восстанавливать
        script.push_str(
            "if [ -f \"$f\" ]; then cp -p \"$f\" \"$f.bak-$(date -u +%Y-%m-%dT%H-%M-%SZ)\"; fi\n",
        );
    }
    script.push_str("if [ -f \"$f\" ]; then cp -p \"$f\" \"$t\"; fi\ncat > \"$t\"\n");
    if let Some(m) = mode {
        script.push_str(&format!("chmod {m} \"$t\"\n"));
    }
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
    host: &str,
    path: &str,
    label: &str,
    events: &[(&str, &str)],
    hook_bin: &str,
) -> Result<(), String> {
    let raw = run_ssh(host, &format!("cat {} 2>/dev/null || true", sh_quote(path)))?;
    let mut json: Value = if raw.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(&raw) {
            Ok(v) => v,
            // битый чужой JSON не трогаем — ровно как локальная установка
            Err(_) => {
                progress(Step::warn(
                    PHASE_HOOKS,
                    format!("{path} на той стороне — невалидный JSON, не трогаю; хуки {label} придётся вписать руками"),
                ));
                return Ok(());
            }
        }
    };
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

/* ================= автозапуск ================= */

/// Юнит systemd --user. Кавычки вокруг путей — на случай пробелов в домашнем
/// каталоге: systemd разбирает строку сам и без них споткнулся бы.
fn unit_text(dir: &str) -> String {
    format!(
        "[Unit]\n\
         Description=Jarvis node — приём хуков агентов для удалённого Jarvis\n\
         Documentation=https://github.com/Sergey-Chernyshev/jarvis/blob/master/docs/remote.md\n\
         After=default.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         Environment=\"JARVIS_DIR={dir}\"\n\
         ExecStart=\"{dir}/bin/jarvis-node\"\n\
         Restart=always\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

/// Автозапуск узла. Своего супервизора не изобретаем: есть systemd --user —
/// пользуемся им, нет — честно говорим и показываем ручной путь.
fn install_service(progress: &Progress, host: &str, remote: &Remote, dir: &str) -> bool {
    if !remote.has("systemd-user") {
        progress(Step::warn(
            PHASE_BOOT,
            "systemctl --user на той стороне недоступен (нет systemd, нет сессии \
             пользователя или запрещён linger) — автозапуск не настроен",
        ));
        progress(Step::info(
            PHASE_BOOT,
            format!("запустить сейчас:  nohup {dir}/bin/jarvis-node >> {dir}/node.log 2>&1 &"),
        ));
        progress(Step::info(
            PHASE_BOOT,
            format!(
                "поднимать после перезагрузки:  crontab -e → @reboot {dir}/bin/jarvis-node >> {dir}/node.log 2>&1"
            ),
        ));
        return false;
    }
    let path = format!("{}/.config/systemd/user/{UNIT}", remote.home);
    if let Err(e) = put_file(host, &path, unit_text(dir).as_bytes(), Some("644"), false) {
        progress(Step::warn(PHASE_BOOT, format!("юнит не записан: {e}")));
        return false;
    }
    // restart, а не start: повторная установка должна поднимать НОВЫЙ бинарь,
    // а не оставлять работать залитый в прошлый раз
    let start = format!(
        "systemctl --user daemon-reload && systemctl --user enable {UNIT} && systemctl --user restart {UNIT}"
    );
    if let Err(e) = run_ssh(host, &start) {
        progress(Step::warn(PHASE_BOOT, format!("узел не запустился: {e}")));
        progress(Step::info(
            PHASE_BOOT,
            format!("посмотреть причину:  ssh {host} 'systemctl --user status {UNIT}; journalctl --user -u {UNIT} -n 50'"),
        ));
        return false;
    }
    progress(Step::done(
        PHASE_BOOT,
        format!("{UNIT}: enabled + запущен (Restart=always)"),
    ));
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

/// Дописать узел в `~/.jarvis/settings.json`.
///
/// Формат и способ записи — как у остального settings-кода (`crate::settings`):
/// весь файл целиком, `to_string_pretty` + перевод строки, права 0600, tmp+rename.
/// Не переиспользуем сам `Store` по прозаической причине: `jarvis-setup`
/// собирается без остального крейта, а `Store` тянет `util`/`log`.
fn record(name: &str, ssh_host: &str, dir: &str) -> Result<Recorded, String> {
    let path = super::jarvis_settings_path();
    let raw = fs::read_to_string(&path).unwrap_or_default();
    let mut root: Value = if raw.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&raw)
            .map_err(|_| format!("{} — невалидный JSON, не трогаю", path.display()))?
    };
    let Some(obj) = root.as_object_mut() else {
        return Err(format!("{} — не объект, не трогаю", path.display()));
    };
    let entry = json!({ "name": name, "sshHost": ssh_host, "jarvisDir": dir });
    let list = obj.entry("remotes").or_insert_with(|| json!([]));
    if !list.is_array() {
        *list = json!([]);
    }
    let arr = list.as_array_mut().unwrap();
    let same = arr
        .iter()
        .position(|r| r.get("name").and_then(Value::as_str) == Some(name));
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
fn from_settings(name: &str) -> Result<(String, String), String> {
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
    Ok((host, dir))
}

/* ================= команды ================= */

/// `jarvis-setup remote add <name> <ssh-host> [--dir <путь>]`.
pub fn add(progress: &Progress, name: &str, ssh_host: &str, dir: Option<&str>) -> Result<(), String> {
    let name = name.trim();
    let ssh_host = ssh_host.trim();
    if name.is_empty() || ssh_host.is_empty() {
        return Err("нужны имя узла и ssh-хост".into());
    }
    check_name(name)?;
    let dir_raw = dir
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(DEFAULT_DIR)
        .trim_end_matches('/')
        .to_string();
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
    let remote = probe(ssh_host)?;
    let dir = remote.expand(&dir_raw);
    progress(Step::done(
        PHASE_LINK,
        format!("{ssh_host}: {} {}, $HOME={}", remote.os, remote.arch, remote.home),
    ));

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
    remote_hooks(
        progress,
        ssh_host,
        &format!("{}/.claude/settings.json", remote.home),
        "claude",
        &super::EVENTS,
        &hook_path,
    )?;
    // codex — только если он там есть: создавать ~/.codex/hooks.json для
    // несуществующего CLI незачем (та же логика, что в install_core)
    if remote.has("codex") || remote.has("codex-home") {
        remote_hooks(
            progress,
            ssh_host,
            &format!("{}/hooks.json", remote.codex_home.trim_end_matches('/')),
            "codex",
            &super::CODEX_EVENTS,
            &hook_path,
        )?;
    } else {
        progress(Step::info(PHASE_HOOKS, "codex не найден — его хуки пропускаю"));
    }

    // 5. Автозапуск.
    progress(Step::start(PHASE_BOOT));
    let supervised = install_service(progress, ssh_host, &remote, &dir);

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
        Err(e) => {
            progress(Step::warn(PHASE_DONE, format!("{e} — впиши узел руками")));
            progress(Step::info(
                PHASE_DONE,
                format!("в \"remotes\" файла {}: {line}", super::jarvis_settings_path().display()),
            ));
        }
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
    // Шим и PATH-блок на ту сторону НЕ ставим: там нет ни панели, ни iTerm, а
    // трогать чужие ~/.bashrc установщик не должен. Значит tmux-обёртку человек
    // заводит сам — без пары `tmux -L jarvis` ответ и пульт работать не будут
    // (события и статусы будут: их шлёт хук, а не tmux).
    progress(Step::info(
        PHASE_DONE,
        format!("сессии на {ssh_host} запускай внутри своего tmux: tmux -L jarvis new -s work, а уже там claude/codex"),
    ));
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
mod tests {
    use super::*;

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
        // curl есть → качаем готовый: это быстрее и не требует там rust
        match resolve_node(&bare(&["curl", "cargo"]), &triples) {
            Ok(NodeSource::Download(url)) => {
                assert!(url.contains("x86_64-unknown-linux-gnu"), "{url}");
                assert!(url.contains(env!("CARGO_PKG_VERSION")), "версия узла = версия приложения");
            }
            other => panic!("ждал скачивание, получил {other:?}"),
        }
        // без curl остаётся сборка на месте
        assert_eq!(resolve_node(&bare(&["cargo"]), &triples), Ok(NodeSource::Build));
        // не осталось ничего — вызывающий подставит развёрнутую инструкцию
        assert!(resolve_node(&bare(&["tmux"]), &triples).is_err());
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
        let got = resolve_node(&bare(&["curl"]), &target_triples("linux", "x86_64"));
        std::env::remove_var("JARVIS_NODE_BIN");
        assert!(
            matches!(got, Ok(NodeSource::Download(_))),
            "mac-бинарь не годится для linux — ждал скачивание, получил {got:?}"
        );
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
        let u = unit_text("/home/bob/.jarvis");
        assert!(u.contains("ExecStart=\"/home/bob/.jarvis/bin/jarvis-node\""));
        assert!(u.contains("Environment=\"JARVIS_DIR=/home/bob/.jarvis\""));
        assert!(u.contains("Restart=always"));
        assert!(u.contains("WantedBy=default.target"));
    }
}
