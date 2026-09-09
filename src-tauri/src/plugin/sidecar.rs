//! Внешний рантайм: плагин = отдельный процесс, канал = JSON-lines по трубе
//! (спека `2026-08-19-everything-is-plugin-design.md` §5).
//!
//! Почему труба, а не второй UDS: сообщение из stdout заведомо принадлежит
//! этому процессу — идентичность по построению, подделывать нечем, токен для
//! управляющего канала не нужен. Токен плагин получает в окружении и предъявляет
//! **только** на вызовах капабилити по `~/.jarvis/run.sock`, где стоит гейт.
//!
//! Кадры (по одной строке JSON):
//! ```text
//! → {"id":1,"method":"go","params":{}}         ядро → плагин
//! ← {"id":1,"ok":true,"value":{}}              ответ
//! ← {"event":"register","data":{"protocol":1}} handshake (первым)
//! ← {"event":"status","data":{}}               что показать в UI
//! ← {"event":"tray","data":[]}                 секция трея
//! ← {"event":"log","data":"…"}                 строка в общий лог
//! ```

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::contract::{CallFut, HostEnv, Plugin, RunState, StartFail, Status, TrayItem};
use super::manifest::Manifest;
use crate::util::{now_ms, sock_path};

/// Версия протокола трубы. Мажор один — несовпадение = `incompatible`.
pub const PROTOCOL: u64 = 1;

/// Сколько ждём кадр `register` после spawn.
const HANDSHAKE: Duration = Duration::from_secs(3);
/// Сколько плагин должен прожить, чтобы падение считалось «новой серией».
const STABLE_MS: i64 = 60_000;
/// Больше подряд — перестаём поднимать (иначе вечный цикл рестартов).
const MAX_RESTARTS: u32 = 5;
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// Сколько ждём мягкого завершения перед SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(3);

pub struct Sidecar {
    manifest: Manifest,
    st: Arc<Side>,
}

struct Side {
    id: String,
    /// Труба в плагин. `None` — процесса сейчас нет.
    stdin: Mutex<Option<std::process::ChildStdin>>,
    pending: Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
    pid: AtomicI64,
    /// Поколение запуска: инкремент = «прежний процесс больше не наш».
    generation: AtomicU64,
    stop: AtomicBool,
    running: AtomicBool,
    restarts: AtomicU32,
    started_at: AtomicI64,
    error: Mutex<Option<String>>,
    status: Mutex<Value>,
    tray: Mutex<Value>,
}

impl Side {
    fn new(id: &str) -> Self {
        Side {
            id: id.to_string(),
            stdin: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            pid: AtomicI64::new(0),
            generation: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            running: AtomicBool::new(false),
            restarts: AtomicU32::new(0),
            started_at: AtomicI64::new(0),
            error: Mutex::new(None),
            status: Mutex::new(Value::Null),
            tray: Mutex::new(Value::Null),
        }
    }

    /// Оборвать все ждущие ответа вызовы — процесса больше нет.
    fn fail_pending(&self, why: &str) {
        let waiters: Vec<_> = self.pending.lock().unwrap().drain().collect();
        for (_, tx) in waiters {
            let _ = tx.send(Err(why.to_string()));
        }
    }

    fn send_line(&self, line: &str) -> Result<(), String> {
        let mut guard = self.stdin.lock().unwrap();
        let Some(w) = guard.as_mut() else {
            return Err("плагин не запущен".into());
        };
        w.write_all(line.as_bytes()).and_then(|_| w.write_all(b"\n")).and_then(|_| w.flush())
            .map_err(|e| format!("труба закрылась: {e}"))
    }

    async fn request(self: Arc<Self>, method: &str, params: Value) -> Result<Value, String> {
        if !self.running.load(Ordering::SeqCst) {
            return Err("плагин не запущен".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let frame = json!({ "id": id, "method": method, "params": params }).to_string();
        if let Err(e) = self.send_line(&frame) {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        match rx.await {
            Ok(res) => res,
            Err(_) => Err("плагин оборвал ответ".into()),
        }
    }
}

impl Sidecar {
    pub fn new(manifest: Manifest) -> Self {
        let st = Arc::new(Side::new(&manifest.id));
        Sidecar { manifest, st }
    }

    /// Убить текущий процесс, не планируя рестарт.
    fn kill(&self) {
        self.st.stop.store(true, Ordering::SeqCst);
        self.st.generation.fetch_add(1, Ordering::SeqCst);
        let pid = self.st.pid.swap(0, Ordering::SeqCst);
        self.st.running.store(false, Ordering::SeqCst);
        *self.st.stdin.lock().unwrap() = None; // закрытие трубы = EOF для плагина
        self.st.fail_pending("плагин остановлен");
        if pid > 0 {
            terminate(pid as i32);
        }
    }
}

impl<C: HostEnv> Plugin<C> for Sidecar {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn start(&self, ctx: &C, token: Option<&str>) -> Result<(), StartFail> {
        let Some(path) = self.manifest.entry_path() else {
            return Err("нет пути к исполняемому файлу".into());
        };
        if !path.exists() {
            return Err(format!("нет файла {}", path.display()).into());
        }
        self.st.stop.store(false, Ordering::SeqCst);
        self.st.restarts.store(0, Ordering::SeqCst);
        *self.st.error.lock().unwrap() = None;
        let generation = self.st.generation.fetch_add(1, Ordering::SeqCst) + 1;

        let (tx, rx) = std::sync::mpsc::channel::<Result<(), StartFail>>();
        let st = self.st.clone();
        let env = ctx.clone();
        let args = self.manifest.entry.as_ref().map(|e| e.args.clone()).unwrap_or_default();
        let token = token.unwrap_or_default().to_string();
        let dir = self.manifest.dir.clone();
        std::thread::Builder::new()
            .name(format!("plugin-{}", self.manifest.id))
            .spawn(move || {
                supervise(st, env, generation, path, args, dir, token, tx);
            })
            .map_err(|e| format!("не смог поднять поток надзора: {e}"))?;

        match rx.recv_timeout(HANDSHAKE) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(fail)) => {
                self.kill();
                Err(fail)
            }
            Err(_) => {
                self.kill();
                Err(format!("плагин не прислал register за {}с", HANDSHAKE.as_secs()).into())
            }
        }
    }

    fn stop(&self, _ctx: &C) {
        self.kill();
    }

    fn status(&self, _ctx: &C) -> Value {
        self.st.status.lock().unwrap().clone()
    }

    fn health(&self, _ctx: &C) -> Option<RunState> {
        let running = self.st.running.load(Ordering::SeqCst);
        let error = self.st.error.lock().unwrap().clone();
        Some(RunState {
            status: if running {
                Status::Running
            } else if error.is_some() {
                Status::Error
            } else {
                Status::Stopped
            },
            error,
            started_at: self.st.started_at.load(Ordering::SeqCst),
            pid: match self.st.pid.load(Ordering::SeqCst) {
                0 => None,
                p => Some(p),
            },
            restarts: self.st.restarts.load(Ordering::SeqCst),
            broken: false,
        })
    }

    fn tray(&self, _ctx: &C) -> Vec<TrayItem> {
        TrayItem::from_json_list(&self.st.tray.lock().unwrap())
    }

    fn call(&self, _ctx: C, name: String, args: Value) -> CallFut {
        let st = self.st.clone();
        Box::pin(async move { st.request(&name, args).await })
    }
}

/// Поток надзора: держит процесс живым, пока плагин включён.
#[allow(clippy::too_many_arguments)]
fn supervise<C: HostEnv>(
    st: Arc<Side>,
    env: C,
    generation: u64,
    path: std::path::PathBuf,
    args: Vec<String>,
    dir: Option<std::path::PathBuf>,
    token: String,
    handshake: std::sync::mpsc::Sender<Result<(), StartFail>>,
) {
    let mut first: Option<std::sync::mpsc::Sender<Result<(), StartFail>>> = Some(handshake);
    let mut fails: u32 = 0;
    let mut delay = BACKOFF_START;

    loop {
        if st.stop.load(Ordering::SeqCst) || st.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        let spawned = Command::new(&path)
            .args(&args)
            .current_dir(dir.clone().unwrap_or_else(|| path.parent().unwrap_or(&path).to_path_buf()))
            .env("JARVIS_SOCKET", sock_path())
            .env("JARVIS_PLUGIN_ID", &st.id)
            .env("JARVIS_TOKEN", &token)
            .env("JARVIS_PROTOCOL", PROTOCOL.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                let why = format!("не запустился: {e}");
                *st.error.lock().unwrap() = Some(why.clone());
                if let Some(tx) = first.take() {
                    let _ = tx.send(Err(why.into()));
                }
                return;
            }
        };
        let born = Instant::now();
        st.pid.store(child.id() as i64, Ordering::SeqCst);
        *st.stdin.lock().unwrap() = child.stdin.take();
        pipe_stderr(&mut child, env.clone(), &st.id);

        let incompatible = pump(&st, &env, &mut child, &mut first);

        // Процесс кончился. Общее состояние чистим, ТОЛЬКО если мы всё ещё
        // текущее поколение: иначе перезапуск (выключил-включил) выглядел бы
        // так — новый процесс уже зарегистрировался и положил свою трубу, а
        // умирающий старый её тут же обнулил, и команды уходили в никуда.
        if st.generation.load(Ordering::SeqCst) == generation {
            st.running.store(false, Ordering::SeqCst);
            *st.stdin.lock().unwrap() = None;
            st.pid.store(0, Ordering::SeqCst);
            st.fail_pending("плагин завершился");
        }
        let _ = child.wait();

        if incompatible
            || st.stop.load(Ordering::SeqCst)
            || st.generation.load(Ordering::SeqCst) != generation
        {
            return;
        }
        if born.elapsed().as_millis() as i64 >= STABLE_MS {
            fails = 0;
            delay = BACKOFF_START;
        }
        fails += 1;
        if fails > MAX_RESTARTS {
            let why = format!("падает подряд {MAX_RESTARTS} раз — больше не поднимаю");
            *st.error.lock().unwrap() = Some(why.clone());
            env.log(&format!("[plugin:{}] {why}", st.id));
            env.plugins_changed();
            return;
        }
        env.log(&format!(
            "[plugin:{}] упал, поднимаю через {}с (попытка {fails})",
            st.id,
            delay.as_secs()
        ));
        env.plugins_changed();
        std::thread::sleep(delay);
        delay = (delay * 2).min(BACKOFF_MAX);
        st.restarts.fetch_add(1, Ordering::SeqCst);
    }
}

/// Читать кадры плагина до EOF. Возвращает true, если протокол несовместим.
fn pump<C: HostEnv>(
    st: &Arc<Side>,
    env: &C,
    child: &mut Child,
    first: &mut Option<std::sync::mpsc::Sender<Result<(), StartFail>>>,
) -> bool {
    let Some(out) = child.stdout.take() else {
        return false;
    };
    for line in BufReader::new(out).lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            env.log(&format!("[plugin:{}] не кадр, а мусор: {line}", st.id));
            continue;
        };
        if let Some(event) = v.get("event").and_then(|e| e.as_str()) {
            match event {
                "register" => {
                    let proto =
                        v.pointer("/data/protocol").and_then(|p| p.as_u64()).unwrap_or(0);
                    if proto != PROTOCOL {
                        let why = format!("протокол плагина {proto}, ядру нужен {PROTOCOL}");
                        *st.error.lock().unwrap() = Some(why.clone());
                        if let Some(tx) = first.take() {
                            let _ = tx.send(Err(StartFail::incompatible(why)));
                        }
                        return true;
                    }
                    st.running.store(true, Ordering::SeqCst);
                    st.started_at.store(now_ms(), Ordering::SeqCst);
                    *st.error.lock().unwrap() = None;
                    if let Some(tx) = first.take() {
                        let _ = tx.send(Ok(()));
                    } else {
                        // рестарт: панель должна увидеть, что плагин снова жив
                        env.plugins_changed();
                    }
                }
                "status" => {
                    *st.status.lock().unwrap() =
                        v.get("data").cloned().unwrap_or(Value::Null);
                    env.plugins_changed();
                }
                "tray" => {
                    *st.tray.lock().unwrap() = v.get("data").cloned().unwrap_or(Value::Null);
                    env.plugins_changed();
                }
                "log" => {
                    let msg = v.get("data").and_then(|d| d.as_str()).unwrap_or("");
                    env.log(&format!("[plugin:{}] {msg}", st.id));
                }
                _ => {} // неизвестные события игнорируем (forward-compat)
            }
            continue;
        }
        if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
            let waiter = st.pending.lock().unwrap().remove(&id);
            if let Some(tx) = waiter {
                let ok = v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
                let res = if ok {
                    Ok(v.get("value").cloned().unwrap_or(Value::Null))
                } else {
                    Err(v
                        .get("error")
                        .and_then(|e| e.as_str())
                        .unwrap_or("плагин отказал без объяснений")
                        .to_string())
                };
                let _ = tx.send(res);
            }
        }
    }
    false
}

/// stderr плагина — в общий лог, чтобы «почему не работает» было где прочитать.
fn pipe_stderr<C: HostEnv>(child: &mut Child, env: C, id: &str) {
    let Some(err) = child.stderr.take() else { return };
    let id = id.to_string();
    let _ = std::thread::Builder::new().name(format!("plugin-err-{id}")).spawn(move || {
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            env.log(&format!("[plugin:{id}] {line}"));
        }
    });
}

/// SIGTERM, а через паузу — SIGKILL: даём плагину прибраться, но не вечно.
fn terminate(pid: i32) {
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let _ = std::thread::Builder::new().name("plugin-kill".into()).spawn(move || {
        std::thread::sleep(TERM_GRACE);
        // 0 — «жив ли процесс»: если да, добиваем
        if unsafe { libc::kill(pid, 0) } == 0 {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::host::PluginHost;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;

    /// Фейковое ядро для тестов рантайма (тот же контракт, что у демона).
    #[derive(Clone, Default)]
    struct Env(Arc<EnvInner>);

    #[derive(Default)]
    struct EnvInner {
        settings: Mutex<serde_json::Map<String, Value>>,
        changed: AtomicUsize,
        log: Mutex<Vec<String>>,
    }

    impl HostEnv for Env {
        fn plugin_settings(&self, id: &str, defaults: Value) -> Value {
            let mut out = defaults;
            if let Some(saved) = self.0.settings.lock().unwrap().get(id).and_then(|v| v.as_object())
            {
                if let Some(dst) = out.as_object_mut() {
                    for (k, v) in saved {
                        dst.insert(k.clone(), v.clone());
                    }
                }
            }
            out
        }
        fn set_plugin_settings(&self, id: &str, patch: serde_json::Map<String, Value>) {
            let mut s = self.0.settings.lock().unwrap();
            let block = s.entry(id.to_string()).or_insert_with(|| json!({}));
            let obj = block.as_object_mut().unwrap();
            for (k, v) in patch {
                obj.insert(k, v);
            }
        }
        fn issue_token(&self, id: &str, _c: &[crate::capability::contract::RiskClass]) -> Option<String> {
            Some(format!("tok-{id}"))
        }
        fn revoke_token(&self, _id: &str) {}
        fn plugins_changed(&self) {
            self.0.changed.fetch_add(1, Ordering::SeqCst);
        }
        fn log(&self, line: &str) {
            self.0.log.lock().unwrap().push(line.to_string());
        }
    }

    /// Каталог плагина с исполняемым скриптом и манифестом.
    fn plugin_dir(name: &str, script: &str, entry_args: Value) -> PathBuf {
        use std::sync::atomic::AtomicU32;
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("jarvis-side-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let bin = dir.join("bin/plug");
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let manifest = json!({
            "id": name,
            "name": name,
            "version": "1.0.0",
            "kind": "external",
            "entry": { "path": "bin/plug", "args": entry_args },
            "capabilities": ["read"],
            "tray": true,
            "commands": [{ "name": "echo" }, { "name": "boom" }]
        });
        std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
        dir
    }

    /// Живой плагин на POSIX sh: handshake, status, tray, эхо на команды.
    const GOOD: &str = r#"#!/bin/sh
printf '{"event":"register","data":{"protocol":1,"pid":%s}}\n' "$$"
printf '{"event":"status","data":{"hello":"мир"}}\n'
printf '{"event":"tray","data":[{"type":"label","text":"Плагин"},{"type":"action","id":"go","text":"Поехали"}]}\n'
while IFS= read -r line; do
  id=`printf '%s' "$line" | sed 's/.*"id":\([0-9]*\).*/\1/'`
  case "$line" in
    *'"method":"boom"'*) printf '{"id":%s,"ok":false,"error":"я так не умею"}\n' "$id" ;;
    *) printf '{"id":%s,"ok":true,"value":{"echoed":true}}\n' "$id" ;;
  esac
done
"#;

    fn sidecar_from(dir: &PathBuf) -> Sidecar {
        Sidecar::new(Manifest::load_dir(dir).expect("манифест валиден"))
    }

    #[test]
    fn handshake_then_status_and_tray_arrive() {
        let dir = plugin_dir("good", GOOD, json!([]));
        let env = Env::default();
        let s = sidecar_from(&dir);
        Plugin::<Env>::start(&s, &env, Some("tok")).expect("плагин поднялся");

        // status и tray — отдельные кадры сразу после register. Ждём ОБА:
        // ожидание только по одному из них делало тест мигающим под нагрузкой.
        let mut tray = Vec::new();
        for _ in 0..250 {
            tray = Plugin::<Env>::tray(&s, &env);
            if !Plugin::<Env>::status(&s, &env).is_null() && !tray.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(Plugin::<Env>::status(&s, &env)["hello"], json!("мир"));
        let health = Plugin::<Env>::health(&s, &env).unwrap();
        assert_eq!(health.status, Status::Running);
        assert!(health.pid.unwrap() > 0);
        assert_eq!(tray.len(), 2, "секция трея разобралась");

        Plugin::<Env>::stop(&s, &env);
        assert_eq!(Plugin::<Env>::health(&s, &env).unwrap().status, Status::Stopped);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commands_round_trip_over_the_pipe() {
        let dir = plugin_dir("rt", GOOD, json!([]));
        let env = Env::default();
        let s = Arc::new(sidecar_from(&dir));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let host: PluginHost<Env> = PluginHost::new();
        host.register(s.clone()).unwrap();
        assert_eq!(host.set_enabled(&env, "rt", true)["ok"], json!(true), "плагин не поднялся");

        let ok = rt.block_on(host.cmd(&env, "rt", "echo", json!({ "x": 1 })));
        assert_eq!(ok["ok"], json!(true));
        assert_eq!(ok["value"]["echoed"], json!(true));

        let bad = rt.block_on(host.cmd(&env, "rt", "boom", json!({})));
        assert_eq!(bad["ok"], json!(false));
        assert_eq!(bad["error"], json!("я так не умею"));

        Plugin::<Env>::stop(s.as_ref(), &env);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toggle_off_and_on_leaves_a_working_pipe() {
        // Регресс: умирающий процесс прошлого поколения обнулял трубу уже
        // поднявшегося нового — плагин выглядел живым, а команды пропадали.
        let dir = plugin_dir("cycle", GOOD, json!([]));
        let env = Env::default();
        let s = Arc::new(sidecar_from(&dir));
        let host: PluginHost<Env> = PluginHost::new();
        host.register(s.clone()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();

        for круг in 1..=3 {
            assert_eq!(host.set_enabled(&env, "cycle", true)["ok"], json!(true), "круг {круг}");
            let out = rt.block_on(host.cmd(&env, "cycle", "echo", json!({})));
            assert_eq!(out["ok"], json!(true), "круг {круг}: {out}");
            host.set_enabled(&env, "cycle", false);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn silent_plugin_fails_handshake_and_is_killed() {
        let dir = plugin_dir("mute", "#!/bin/sh\nsleep 30\n", json!([]));
        let env = Env::default();
        let s = sidecar_from(&dir);
        let err = Plugin::<Env>::start(&s, &env, Some("tok")).unwrap_err();
        assert!(err.message.contains("register"), "{}", err.message);
        assert!(!err.incompatible);
        assert_eq!(Plugin::<Env>::health(&s, &env).unwrap().pid, None, "процесс прибит");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_protocol_is_incompatible_not_retried() {
        let script = "#!/bin/sh\nprintf '{\"event\":\"register\",\"data\":{\"protocol\":99}}\\n'\nsleep 5\n";
        let dir = plugin_dir("old", script, json!([]));
        let env = Env::default();
        let s = sidecar_from(&dir);
        let err = Plugin::<Env>::start(&s, &env, Some("tok")).unwrap_err();
        assert!(err.incompatible, "мажор протокола не наш — рестарт бессмыслен");
        assert!(err.message.contains("99"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_binary_is_a_plain_error() {
        let dir = plugin_dir("gone", GOOD, json!([]));
        std::fs::remove_file(dir.join("bin/plug")).unwrap();
        let env = Env::default();
        let s = sidecar_from(&dir);
        let err = Plugin::<Env>::start(&s, &env, Some("tok")).unwrap_err();
        assert!(err.message.contains("нет файла"), "{}", err.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_is_restarted_with_backoff() {
        // Счётчик запусков — в файле, путь приходит аргументом из манифеста
        // (окружение процесса-теста трогать нельзя: тесты идут в потоках).
        let script = r#"#!/bin/sh
n=$(cat "$1" 2>/dev/null || echo 0)
n=$((n + 1))
printf '%s' "$n" > "$1"
printf '{"event":"register","data":{"protocol":1}}\n'
if [ "$n" -eq 1 ]; then exit 1; fi
sleep 5
"#;
        let dir = plugin_dir("flap", script, json!([]));
        let counter = dir.join("count");
        // манифест уже записан — перепишем entry.args с путём к счётчику
        let mut m: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap())
                .unwrap();
        m["entry"]["args"] = json!([counter.to_string_lossy()]);
        std::fs::write(dir.join("manifest.json"), m.to_string()).unwrap();

        let env = Env::default();
        let s = sidecar_from(&dir);
        Plugin::<Env>::start(&s, &env, Some("tok")).expect("первый запуск удался");

        // backoff первой попытки — 1с; ждём подъёма второго процесса
        let mut restarted = false;
        for _ in 0..80 {
            std::thread::sleep(Duration::from_millis(100));
            if std::fs::read_to_string(&counter).unwrap_or_default().trim() == "2" {
                restarted = true;
                break;
            }
        }
        assert!(restarted, "надзор обязан поднять упавший плагин");
        assert!(Plugin::<Env>::health(&s, &env).unwrap().restarts >= 1);
        Plugin::<Env>::stop(&s, &env);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Референсный плагин из репозитория — не картинка в документации, а
    /// рабочий пример: он обязан подниматься и отвечать по этому же протоколу.
    #[test]
    fn shipped_example_plugin_actually_speaks_the_protocol() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("plugins/example-hello");
        let m = Manifest::load_dir(&dir).expect("манифест примера валиден");
        assert_eq!(m.id, "example-hello");

        let env = Env::default();
        let s = Arc::new(Sidecar::new(m));
        let host: PluginHost<Env> = PluginHost::new();
        host.register(s.clone()).unwrap();
        assert_eq!(host.set_enabled(&env, "example-hello", true)["ok"], json!(true));

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let out = rt.block_on(host.cmd(&env, "example-hello", "привет", json!({})));
        assert_eq!(out["ok"], json!(true), "{out}");
        assert_eq!(out["value"]["сказал"], json!("привет"));

        // и его секция трея доезжает до ядра разобранной
        let mut items = Vec::new();
        for _ in 0..250 {
            items = host.tray_items(&env);
            if !items.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!items.is_empty(), "пример не отдал секцию трея");
        assert!(
            host.route_tray("plug:example-hello:hi").is_some(),
            "клик по пункту примера некуда маршрутизировать"
        );
        host.set_enabled(&env, "example-hello", false);
    }

    #[test]
    fn call_on_dead_plugin_errors_instead_of_hanging() {
        let dir = plugin_dir("dead", GOOD, json!([]));
        let env = Env::default();
        let s = sidecar_from(&dir);
        Plugin::<Env>::start(&s, &env, Some("tok")).unwrap();
        Plugin::<Env>::stop(&s, &env);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let out = rt.block_on(Plugin::<Env>::call(&s, env.clone(), "echo".into(), json!({})));
        assert!(out.is_err(), "мёртвому плагину звонить некуда");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
