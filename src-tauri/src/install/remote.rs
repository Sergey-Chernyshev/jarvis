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

use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
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
for b in tmux curl claude codex systemctl; do
  command -v "$b" >/dev/null 2>&1 && printf 'have=%s\n' "$b"
done
[ -d "${CODEX_HOME:-$HOME/.codex}" ] && printf 'have=%s\n' codex-home
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
    match (remote.has("claude"), remote.has("codex")) {
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
    let tried = node_candidates(&triples);
    let src = tried
        .iter()
        .find(|p| p.is_file())
        .ok_or_else(|| build_hint(&remote, &triples, &tried))?;
    let bytes = fs::read(src).map_err(|e| format!("не смог прочитать {}: {e}", src.display()))?;
    let kind = binary_kind(&bytes);
    if !binary_fits(kind, &remote.os, &remote.arch) {
        let (bin_os, bin_arch) = kind.unwrap_or(("?", "?"));
        return Err(format!(
            "{} — сборка под {bin_os}/{bin_arch}, а на той стороне {}/{}: такой бинарь \
             там не запустится.\n{}",
            src.display(),
            remote.os,
            remote.arch,
            build_hint(&remote, &triples, &tried)
        ));
    }
    if kind.is_none() {
        progress(Step::info(
            PHASE_NODE,
            format!("формат {} не распознал — заливаю как есть", src.display()),
        ));
    }
    let node_path = format!("{dir}/bin/jarvis-node");
    put_file(ssh_host, &node_path, &bytes, Some("755"), false)?;
    progress(Step::done(
        PHASE_NODE,
        format!(
            "{node_path} ← {} ({} КБ)",
            src.display(),
            bytes.len() / 1024
        ),
    ));
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
    let line = format!("{{ \"name\": \"{name}\", \"sshHost\": \"{ssh_host}\", \"jarvisDir\": \"{dir_raw}\" }}");
    match record(name, ssh_host, &dir_raw) {
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
