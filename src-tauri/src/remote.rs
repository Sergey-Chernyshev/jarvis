//! Клиент удалённых узлов: ssh-туннель, HTTP к узлу, поллер событий.
//!
//! Ноут спит, закрывается и переезжает между сетями, а агент на VPS в это время
//! работает. Поэтому события копит УЗЕЛ (кольцевой буфер с курсором), а этот
//! модуль их только забирает — с курсора, который переживает перезапуск ноута.
//! Интерпретацию (статусы, ходы, уведомления) делает существующее ядро: события
//! приезжают сюда сырыми, ровно в том виде, в каком их шлют локальные хуки.
//!
//! Транспорт — только ssh: узел слушает unix-сокет 0600 и не открывает наружу
//! ни одного порта. Отсюда главный инвариант модуля — НИКАКИХ своих секретов:
//! аутентификация целиком ssh-шная (ключи, ~/.ssh/config, агент-форвардинг),
//! Jarvis ничего не заводит и не пишет на диск.
//!
//! Контракт узла — зеркало `node::http` (спека 2026-08-05-remote-agents-design):
//!   GET  /hello                    → {version, host, uptime_ms, cursor, buffered}
//!   GET  /events?since=<u64>       → {cursor, events:[{cursor, at, envelope}]}
//!                                    либо {gap:true, cursor} (long-poll ≤25с)
//!   GET  /file?path=<p>&from=<off> → {path, from, next, size, eof, data}; 404 —
//!                                    транскрипта ещё нет (ожидание, не отказ)
//!   POST /reply    {pane, text}
//!   POST /control  {pane, cmd}
//!   GET  /panes                    → {panes:[{pane, session, pid, cwd}], error?}

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::util::jarvis_dir;

/// Каталог Jarvis на той стороне, если в настройках не указан свой.
const DEFAULT_REMOTE_DIR: &str = "~/.jarvis";
/// Таймаут коротких запросов (hello/reply/control/panes/file).
const HTTP_TIMEOUT_SECS: u64 = 10;
/// Long-poll `/events`: узел держит соединение до 25с — берём с запасом, иначе
/// клиент рвал бы каждый холостой цикл и молотил ssh-туннель впустую.
const POLL_TIMEOUT_SECS: u64 = 35;
/// Потолок backoff: мёртвый VPS переспрашиваем раз в полминуты, не чаще.
const MAX_BACKOFF_SECS: u64 = 30;
/// Сколько ждать, пока форвард начнёт принимать. Раньше здесь была глухая пауза
/// в 700 мс — и на любой связи, где ssh не успевал за неё аутентифицироваться,
/// туннель не поднимался НИКОГДА: рукопожатие падало, супервизор ронял ssh и
/// уходил в паузу, круг за кругом. Теперь ждём событие, а не угадываем тайминг.
const TUNNEL_READY_SECS: u64 = 15;
// A valid Teleport certificate does not make the proxy/node handshake instant.
// Observed tsh forwards take ~29s; killing them at 15s prevents every retry
// from ever reaching readiness. An exited tsh still fails immediately.
const TELEPORT_READY_SECS: u64 = 45;
/// Минимальная пауза между кругами поллера. Узел без long-poll (или отдавший
/// пустую страницу мгновенно) не должен превращать поллер в busy-loop.
const POLL_FLOOR_MS: u64 = 1000;

/* ======================= 1. конфиг узлов ======================= */

/// Узел из настроек: имя (оно же пространство имён сессий), ssh-хост в терминах
/// `~/.ssh/config` и каталог Jarvis на той стороне.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RemoteCfg {
    pub name: String,
    pub ssh_host: String,
    pub jarvis_dir: String,
    pub ssh_config_file: Option<String>,
    pub transport: String,
    pub teleport_proxy: Option<String>,
    pub teleport_cluster: Option<String>,
    pub node_tcp_port: Option<u16>,
    pub run_as_user: Option<String>,
}

impl RemoteCfg {
    pub fn connection(&self) -> crate::install::remote::Connection {
        crate::install::remote::Connection { ssh_host: self.ssh_host.clone(),
            ssh_config_file: self.ssh_config_file.clone(), transport: self.transport.clone(),
            teleport_proxy: self.teleport_proxy.clone(), teleport_cluster: self.teleport_cluster.clone(), node_tcp_port: self.node_tcp_port,
            run_as_user: self.run_as_user.clone() }
    }
}

/// Разобрать ключ `remotes` настроек.
///
/// Кривые записи пропускаем молча: список редактируется руками (и, позже,
/// `jarvis-setup remote add`), один битый узел не должен уносить остальные.
/// Дубли по имени тоже отбрасываем — имя это ключ реестра `<remote>:<id>`,
/// два узла под одним именем смешали бы сессии разных машин.
pub fn parse_remotes(settings: &Value) -> Vec<RemoteCfg> {
    let Some(arr) = settings.get("remotes").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out: Vec<RemoteCfg> = Vec::new();
    for item in arr {
        let Ok(mut cfg) = serde_json::from_value::<RemoteCfg>(item.clone()) else {
            continue; // не объект / чужие типы полей
        };
        cfg.name = cfg.name.trim().to_string();
        cfg.ssh_host = cfg.ssh_host.trim().to_string();
        cfg.jarvis_dir = cfg.jarvis_dir.trim().trim_end_matches('/').to_string();
        if cfg.name.is_empty() || cfg.connection().validate().is_err() {
            continue; // без имени или хоста узел неадресуем
        }
        if cfg.jarvis_dir.is_empty() {
            cfg.jarvis_dir = DEFAULT_REMOTE_DIR.to_string();
        }
        if out.iter().any(|c| c.name == cfg.name) {
            continue;
        }
        out.push(cfg);
    }
    out
}

/// Узлы из настроек демона. Пустой список = удалённый слой выключен целиком.
pub fn load_remotes(store: &crate::settings::Store) -> Vec<RemoteCfg> {
    parse_remotes(&store.load())
}

/* ======================= 2. ssh-туннель ======================= */

/// Путь к сокету узла на той стороне.
pub fn node_sock(jarvis_dir: &str) -> String {
    format!("{}/node.sock", jarvis_dir.trim_end_matches('/'))
}

/// Аргументы `ssh` для туннеля «локальный порт → unix-сокет узла».
///
/// Почему именно такой набор опций:
/// * `-N` — никакой удалённой команды, туннель и только туннель;
/// * `BatchMode=yes` — фоновый процесс демона не имеет куда спросить пароль;
///   без этого ssh залипал бы на промпте, а туннель выглядел бы «поднимающимся»;
/// * `ExitOnForwardFailure=yes` — занятый порт/недоступный сокет должны убивать
///   ssh, а не оставлять живое соединение без форварда (тогда мы считали бы
///   туннель рабочим и вечно ловили connection refused);
/// * `ServerAlive*` — сон ноута и смена сети рвут TCP молча; keepalive
///   превращает «мёртвую» сессию в честный выход, и супервизор её переподнимает.
pub fn ssh_args(ssh_host: &str, port: u16, sock: &str) -> Vec<String> {
    vec![
        "-N".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
        "-o".to_string(),
        "ConnectTimeout=10".to_string(),
        "-o".to_string(),
        "ServerAliveInterval=15".to_string(),
        "-o".to_string(),
        "ServerAliveCountMax=3".to_string(),
        "-L".to_string(),
        format!("127.0.0.1:{port}:{sock}"),
        ssh_host.to_string(),
    ]
}

/// Болтовня ssh, которая не является причиной отказа. Показать её как ошибку
/// значит увести человека не туда: туннель после такой строки прекрасно живёт.
fn is_ssh_noise(line: &str) -> bool {
    line.starts_with("Warning: Permanently added")
        || line.starts_with("Pseudo-terminal")
        || line.contains("setlocale")
}

/// Пауза перед следующей попыткой: 1,2,4,8,16,30…с. Растёт от числа подряд
/// неудачных кругов — недоступный VPS не должен превращаться в шторм ssh.
pub fn backoff_secs(fails: u64) -> u64 {
    let shift = fails.min(5) as u32;
    (1u64 << shift).min(MAX_BACKOFF_SECS)
}

/// Свободный TCP-порт петли. Гонка (порт займут между bind и стартом ssh)
/// теоретически возможна, но `ExitOnForwardFailure=yes` делает её громкой:
/// ssh падает, следующая попытка берёт другой порт.
fn free_port() -> Option<u16> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?;
    l.local_addr().ok().map(|a| a.port())
}

/// Развернуть `~` в каталоге узла. ssh тильду в `-L` НЕ раскрывает, а домашний
/// каталог удалённой машины локально неизвестен — спрашиваем его один раз у той
/// стороны. Дешевле, чем требовать абсолютный путь и молча ломаться, когда
/// человек напишет привычное `~/.jarvis`.
fn resolve_home(connection: &crate::install::remote::Connection, dir: &str) -> Option<String> {
    let rest = dir.strip_prefix('~')?.trim_start_matches('/');
    let out = connection.command_with_script("printf 'JARVIS_HOME=%s\\n' \"$HOME\"").ok()?
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let home = parse_home(&String::from_utf8_lossy(&out.stdout))?;
    Some(if rest.is_empty() {
        home
    } else {
        format!("{}/{rest}", home.trim_end_matches('/'))
    })
}

/// Достать `$HOME` из ответа той стороны: берём помеченную строку, всё
/// остальное — чужой вывод, который не наше дело фильтровать по одному.
fn parse_home(out: &str) -> Option<String> {
    let home = out
        .lines()
        .find_map(|l| l.trim().strip_prefix("JARVIS_HOME="))?
        .trim();
    // Хвостовой слэш не трогаем: его снимает вызывающий, а «/» как $HOME
    // (бывает у root) обрезкой превратился бы в пустую строку.
    (home.starts_with('/')).then(|| home.to_string())
}

/// Что случилось с туннелем на этом круге супервизора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelState {
    /// Процесс ssh жив с прошлого круга.
    Alive,
    /// Только что подняли — форварду нужно время, а нам рукопожатие.
    Spawned,
    /// Не подняли (нет порта / ssh не запустился).
    Failed,
}

/// Дочерний `ssh -N -L …`: держим живым, порт выбираем свободный при каждом
/// подъёме (старый мог остаться занятым умирающим соединением).
pub struct Tunnel {
    ssh_host: String,
    connection: crate::install::remote::Connection,
    /// Каталог Jarvis на той стороне. `~` разворачивается лениво, при первом
    /// удачном подъёме — до этого момента удалённого $HOME мы не знаем.
    dir: Mutex<String>,
    /// Текущий локальный порт; 0 — туннеля нет.
    port: AtomicU16,
    child: Mutex<Option<Child>>,
    /// Должен ли туннель работать. `stop()` снимает — супервизор не воскрешает.
    active: AtomicBool,
    /// Последняя внятная строка stderr от ssh. Живёт здесь, а не только в логе:
    /// лог выключен по умолчанию, а «почему туннель не поднялся» — это ровно то,
    /// что человек должен увидеть в панели, не включая режим диагностики.
    stderr_tail: Arc<Mutex<String>>,
}

impl Tunnel {
    pub fn new(cfg: &RemoteCfg) -> Self {
        Tunnel {
            ssh_host: cfg.ssh_host.clone(),
            connection: cfg.connection(),
            dir: Mutex::new(cfg.jarvis_dir.clone()),
            port: AtomicU16::new(0),
            child: Mutex::new(None),
            active: AtomicBool::new(true),
            stderr_tail: Arc::new(Mutex::new(String::new())),
        }
    }

    /// Последняя жалоба ssh — пустая строка, если он молчал.
    pub fn last_stderr(&self) -> String {
        self.stderr_tail.lock().unwrap().clone()
    }

    /// Забыть прошлую жалобу: связь наладилась, старая причина только путает.
    pub fn clear_stderr(&self) {
        self.stderr_tail.lock().unwrap().clear();
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }

    /// База HTTP поверх туннеля.
    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port())
    }

    /// Дождаться, пока форвард начнёт принимать соединения.
    ///
    /// ssh открывает локальный порт только ПОСЛЕ аутентификации, поэтому
    /// успешный connect — точный признак готовности, а не догадка о таймингах.
    /// Блокирующая: вызывать из `spawn_blocking`.
    pub fn wait_ready(&self, timeout: Duration) -> bool {
        let port = self.port();
        if port == 0 {
            return false;
        }
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if !self.is_up() {
                return false; // ssh умер (не пустили, форвард запрещён) — ждать нечего
            }
            if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(120));
        }
    }

    /// Жив ли процесс ssh (для бейджа и гейта запросов).
    pub fn is_up(&self) -> bool {
        let mut g = self.child.lock().unwrap();
        g.as_mut()
            .map(|c| matches!(c.try_wait(), Ok(None)))
            .unwrap_or(false)
    }

    /// Абсолютный каталог узла: пока тильда не развёрнута — пробуем развернуть.
    /// Неудача не фатальна: отдаём как есть, ssh отругается в лог.
    fn remote_dir(&self) -> String {
        let cur = self.dir.lock().unwrap().clone();
        if !cur.starts_with('~') {
            return cur;
        }
        match resolve_home(&self.connection, &cur) {
            Some(abs) => {
                *self.dir.lock().unwrap() = abs.clone();
                abs
            }
            None => cur,
        }
    }

    /// Поднять туннель, если он не жив. Не ждёт готовности форварда — это
    /// делает рукопожатие поллера (`/hello`).
    /// Записать причину, по которой туннель не поднялся, — её покажет панель.
    fn note(&self, why: &str) {
        crate::log::line(&format!("[remote] {}: {why}", self.ssh_host));
        *self.stderr_tail.lock().unwrap() = why.to_string();
    }

    pub fn ensure_started(&self) -> TunnelState {
        // A queued spawn_blocking may begin after its poll task was aborted.
        // Only a new Tunnel object can re-arm an explicitly stopped node.
        if !self.active.load(Ordering::SeqCst) {
            return TunnelState::Failed;
        }
        if self.is_up() {
            return TunnelState::Alive;
        }
        // Мёртвого дожинаем, иначе останется зомби. Забрать — под замком,
        // дожать — без него: см. `kick`.
        let dead = self.child.lock().unwrap().take();
        if let Some(mut c) = dead {
            let _ = c.wait();
        }
        self.port.store(0, Ordering::SeqCst);

        let dir = self.remote_dir(); // может сходить по ssh — до захвата лока
        // `~` в `-L` не раскрывает никто: ни ssh, ни sshd на той стороне. Такой
        // туннель поднимется и будет молча отдавать «connection reset» на каждый
        // запрос — худший вид поломки. Лучше не поднимать вовсе и сказать почему.
        if dir.starts_with('~') {
            self.note(
                "не смог узнать $HOME на той машине (ssh не ответил на рукопожатие) — \
                 проверь `ssh <хост> true` или впиши абсолютный каталог узла вместо ~",
            );
            return TunnelState::Failed;
        }
        let Some(port) = free_port() else {
            self.note("не нашёл свободный порт на этой машине");
            return TunnelState::Failed;
        };
        let mut command = match self.connection.tunnel(port, &node_sock(&dir)) {
            Ok(command) => command,
            Err(error) => { self.note(&error); return TunnelState::Failed; }
        };

        let mut g = self.child.lock().unwrap();
        match command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // stderr → лог: «Permission denied», «no such file» и прочие причины
            // молчания узла должны быть видны в ~/.jarvis/jarvis.log, а не теряться.
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut c) => {
                // Пока мы поднимались (а `remote_dir` мог ходить по ssh секундами),
                // узел могли остановить. `stop()` снимает active ДО того, как
                // возьмёт этот же лок, — поэтому проверка под локом не даёт
                // осиротевшему ssh пережить остановку.
                if !self.active.load(Ordering::SeqCst) {
                    let _ = c.kill();
                    let _ = c.wait();
                    return TunnelState::Failed;
                }
                if let Some(err) = c.stderr.take() {
                    let host = self.ssh_host.clone();
                    let tail = self.stderr_tail.clone();
                    std::thread::spawn(move || {
                        use std::io::{BufRead, BufReader};
                        for line in BufReader::new(err).lines().map_while(Result::ok) {
                            let line = line.trim().to_string();
                            if line.is_empty() || is_ssh_noise(&line) {
                                continue;
                            }
                            crate::log::line(&format!("[remote] ssh {host}: {line}"));
                            *tail.lock().unwrap() = line;
                        }
                    });
                }
                *g = Some(c);
                self.port.store(port, Ordering::SeqCst);
                crate::log::line(&format!(
                    "[remote] туннель {} → 127.0.0.1:{port}",
                    self.ssh_host
                ));
                TunnelState::Spawned
            }
            Err(e) => {
                self.note(&format!("ssh не запустился: {e} (он вообще установлен?)"));
                TunnelState::Failed
            }
        }
    }

    /// Уронить туннель, оставив намерение работать: следующий круг поднимет его
    /// заново и на новом порту. Нужно, когда HTTP не отвечает при живом ssh —
    /// значит подвис узел или сам форвард, и лечится только переподъёмом.
    pub fn kick(&self) {
        // Ребёнка забираем ПОД замком, а хороним уже без него. Ожидание смерти
        // ssh — блокирующее и на мёртвой сети затягивается; под замком оно
        // останавливало всех, кому этот замок нужен, включая главный поток.
        let child = self.child.lock().unwrap().take();
        if let Some(mut c) = child {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.port.store(0, Ordering::SeqCst);
    }

    /// Остановить и снять намерение работать (смена настроек/выход).
    ///
    /// Намерение снимаем здесь и сейчас — это атомик, он мгновенный и важен:
    /// поллер не должен поднять туннель обратно. А похороны процесса уходят в
    /// отдельный поток: `wait` блокирующий, на мёртвой сети ssh умирает не
    /// мгновенно, и вызывающим (в том числе главному потоку через синхронные
    /// команды настроек) ждать этого незачем.
    pub fn stop(&self) {
        let Some(mut c) = self.disarm() else { return };
        std::thread::spawn(move || {
            let _ = c.kill();
            let _ = c.wait();
        });
    }

    /// То же, но дождавшись смерти прямо здесь.
    ///
    /// Нужно ровно на выходе из приложения: там отложенные похороны не
    /// состоятся — процесс закончится раньше, и ssh переживёт своего хозяина,
    /// продолжая держать порт до конца сессии терминала.
    pub fn stop_now(&self) {
        let Some(mut c) = self.disarm() else { return };
        let _ = c.kill();
        let _ = c.wait();
    }

    /// Снять намерение работать и забрать процесс. Только атомики и короткий
    /// замок — ничего блокирующего.
    fn disarm(&self) -> Option<Child> {
        self.active.store(false, Ordering::SeqCst);
        let child = self.child.lock().unwrap().take();
        self.port.store(0, Ordering::SeqCst);
        child
    }
}

impl Drop for Tunnel {
    /// Дочерний ssh не должен пережить владельца: иначе после смены настроек
    /// (или закрытия приложения) на машине копятся осиротевшие туннели.
    fn drop(&mut self) {
        self.stop_now();
    }
}

/* ======================= 3. HTTP-клиент узла ======================= */

/// Ответ `GET /hello` — проверка связи, версия узла и состояние его буфера.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Hello {
    pub version: String,
    pub host: String,
    pub uptime_ms: u64,
    /// Курсор ленты узла — по нему видно, насколько мы отстали.
    pub cursor: u64,
    pub buffered: u64,
    pub capacity: u64,
    pub protocol: u32,
    pub capabilities: Vec<String>,
    pub sources: Vec<Value>,
}

/// Событие из буфера узла: конверт хука + метки узла.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Recorded {
    pub cursor: u64,
    /// Когда узел ПРИНЯЛ событие. Часы VPS и ноута расходятся, поэтому метка
    /// вспомогательная — порядок задаёт курсор, а не время.
    pub at: i64,
    /// Конверт от jarvis-hook как есть: ровно то, что ест редьюсер демона.
    pub envelope: Value,
}

/// Страница `GET /events?since=`. В ответе-дырке узел присылает только
/// `{gap, cursor}` — отсутствующие поля добираются дефолтами.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EventsPage {
    /// Identifies one node process; numeric cursors restart with the process.
    pub instance: String,
    /// Курсор ПОСЛЕ последнего события страницы — с него идёт следующий запрос.
    pub cursor: u64,
    /// Кольцевой буфер узла переполнился: часть событий потеряна безвозвратно.
    pub gap: bool,
    pub events: Vec<Recorded>,
}

/// Кусок транскрипта `GET /file`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileChunk {
    pub path: String,
    /// Смещение, с которого реально отдан кусок.
    pub from: u64,
    /// Смещение конца — курсор для следующего чтения.
    pub next: u64,
    pub size: u64,
    pub eof: bool,
    /// Текст куска (у узла ключ `data`).
    pub data: String,
}

impl FileChunk {
    /// Файл переписали с нуля: узел отдал кусок раньше запрошенного места.
    /// Читатель обязан начать транскрипт заново, а не дописывать хвост.
    pub fn rewound(&self, asked_from: u64) -> bool {
        self.from < asked_from
    }
}

/// Обрезать кусок до целых строк и посчитать, где он на самом деле кончился.
///
/// Читая с середины файла (`start > 0`), первая строка почти наверняка
/// оборвана — её отбрасываем. Последняя может быть недописана прямо сейчас:
/// агент пишет транскрипт, пока мы его читаем. Возвращаем только целые строки
/// и смещение сразу за ними, чтобы недописанную дочитал следующий запрос
/// целиком, а не двумя половинками.
fn cut_to_lines(text: &str, start: u64) -> (String, u64) {
    let body = if start > 0 {
        match text.find('\n') {
            Some(i) => &text[i + 1..],
            // весь кусок — нутро одной строки: целых записей тут нет
            None => "",
        }
    } else {
        text
    };
    let head = text.len() - body.len(); // байты, съеденные обрезкой первой строки
    let cut = body.rfind('\n').map_or(0, |i| i + 1);
    (body[..cut].to_string(), start + (head + cut) as u64)
}

/// Живая пана узла. Имена полей — как отдаёт `node::tmux::Pane`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RemotePane {
    pub pane: String,
    pub session: String,
    pub pid: i64,
    pub cwd: String,
}

/// Ответ `GET /panes`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PanesReply {
    pub panes: Vec<RemotePane>,
    /// tmux не установлен или сервер не поднят: это состояние машины, а не
    /// поломка узла — поэтому приезжает в теле, а не кодом ошибки.
    pub error: String,
}

/// Живой агент на той машине: то, что узел нашёл в таблице процессов.
///
/// Нужен для двух вещей сверки, и обе — про «а он вообще жив»: pid отвечает на
/// это честнее паны (пана бывает из ЧУЖОГО tmux-сервера), а `session_id` даёт
/// подобрать сессию, о которой хуков не приходило вовсе.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RemoteAgent {
    pub pid: i64,
    /// "claude" | "codex".
    pub agent: String,
    pub cwd: String,
    /// Пана `-L jarvis`, если агент растёт из неё; пусто — запущен мимо.
    pub pane: String,
    /// Имя tmux-сессии той паны.
    pub session: String,
    /// Идентификатор сессии агента (имя файла транскрипта). Пусто — узел не
    /// смог связать процесс с транскриптом, и подбирать нечего.
    pub session_id: String,
    pub transcript: String,
}

/// Ответ `GET /agents?pids=` — один снимок «кто там сейчас работает».
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AgentsReply {
    pub agents: Vec<RemoteAgent>,
    /// Паны `-L jarvis` — инвариант «одна пана — одна сессия».
    pub panes: Vec<String>,
    /// Какие из спрошенных pid живы.
    pub alive: Vec<i64>,
    /// tmux не установлен или сервер не поднят.
    pub error: String,
}

/// HTTP-клиент к одному узлу через его туннель.
///
/// Два клиента, а не один: long-poll `/events` живёт до 25с, а ответ/пульт
/// должны падать быстро — общий 35-секундный таймаут превращал бы недоступный
/// узел в подвисший UI.
pub struct NodeClient {
    base: String,
    http: reqwest::Client,
    poll: reqwest::Client,
}

impl NodeClient {
    pub fn new(base: impl Into<String>) -> Result<Self, String> {
        Ok(NodeClient {
            base: base.into(),
            http: Self::client(Duration::from_secs(HTTP_TIMEOUT_SECS))?,
            poll: Self::client(Duration::from_secs(POLL_TIMEOUT_SECS))?,
        })
    }

    fn client(timeout: Duration) -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .timeout(timeout)
            // туннель — 127.0.0.1; системный HTTP_PROXY его не касается
            .no_proxy()
            .build()
            .map_err(|e| format!("http client: {e}"))
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        client: &reqwest::Client,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, String> {
        let resp = client
            .get(format!("{}{path}", self.base))
            .query(query)
            .send()
            .await
            .map_err(|e| format!("узел недоступен: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("узел rc={} на {path}", resp.status()));
        }
        resp.json::<T>()
            .await
            .map_err(|e| format!("разбор ответа {path}: {e}"))
    }

    async fn post(&self, path: &str, body: &Value) -> Result<(), String> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await
            .map_err(|e| format!("узел недоступен: {e}"))?;
        let status = resp.status();
        let reply: Value = resp.json().await.unwrap_or(Value::Null);
        parse_command_reply(path, status.as_u16(), &reply)
    }

    /// Версия узла и его uptime — рукопожатие после подъёма туннеля.
    pub async fn hello(&self) -> Result<Hello, String> {
        self.get_json(&self.http, "/hello", &[]).await
    }

    /// События с курсора. Запрос долгий: узел держит его до 25с, если событий
    /// нет — так поллер не опрашивает VPS вхолостую каждую секунду.
    pub async fn events(&self, since: u64, instance: &str) -> Result<EventsPage, String> {
        self.get_json(&self.poll, "/events", &[("since", since.to_string()), ("instance", instance.to_string())])
            .await
    }

    /// Кусок транскрипта. Корни файлов проверяет узел — ноут не решает, что
    /// той стороне можно отдавать. `Ok(None)` = транскрипта ещё нет: свежая
    /// сессия до первого промпта, это ожидание, а не ошибка.
    pub async fn file(&self, path: &str, from: u64) -> Result<Option<FileChunk>, String> {
        let resp = self
            .http
            .get(format!("{}/file", self.base))
            .query(&[("path", path.to_string()), ("from", from.to_string())])
            .send()
            .await
            .map_err(|e| format!("узел недоступен: {e}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(format!("узел rc={} на /file", resp.status()));
        }
        resp.json::<FileChunk>()
            .await
            .map(Some)
            .map_err(|e| format!("разбор ответа /file: {e}"))
    }

    /// Хвост транскрипта одним куском — то, с чего открывается чат.
    ///
    /// Размер узнаём тем же `/file`: узел зажимает `from` к размеру файла,
    /// поэтому запрос за концом — дешёвое «сколько там байт» без отдельного
    /// эндпоинта. Дальше дочитываем кусками по 512К (потолок узла).
    ///
    /// Возвращает текст, начатый и оборванный по границе строк, и смещение,
    /// с которого продолжит живой хвост. Обрезанные половинки строк не отдаём:
    /// их дочитает следующий запрос, а склеить половинку с продолжением может
    /// только тот, кто держит смещение. `Ok(None)` — транскрипта ещё нет.
    pub async fn tail_text(
        &self,
        path: &str,
        max_bytes: u64,
    ) -> Result<Option<(String, u64)>, String> {
        let Some(head) = self.file(path, u64::MAX).await? else {
            return Ok(None);
        };
        let size = head.size;
        let start = size.saturating_sub(max_bytes);
        let mut text = String::new();
        let mut at = start;
        while at < size {
            let Some(chunk) = self.file(path, at).await? else {
                return Ok(None); // файл исчез между запросами — считаем, что его нет
            };
            if chunk.next <= at {
                break; // узел не двигается вперёд — цикл крутить незачем
            }
            at = chunk.next;
            text.push_str(&chunk.data);
        }
        Ok(Some(cut_to_lines(&text, start)))
    }

    /// Ответ в пану удалённой сессии (там же `tmux -L jarvis`, что и локально).
    pub async fn reply(&self, pane: &str, text: &str) -> Result<(), String> {
        self.post("/reply", &serde_json::json!({ "pane": pane, "text": text }))
            .await
    }

    /// Слэш-команда пульта (модель/effort) в пану.
    pub async fn control(&self, pane: &str, cmd: &str) -> Result<(), String> {
        self.post("/control", &serde_json::json!({ "pane": pane, "cmd": cmd }))
            .await
    }

    /// План клавиш в пикер вопроса. Что нажимать — решает ноут (раскладка
    /// пикеров Claude/Codex — его знание), узел только проигрывает.
    pub async fn keys(&self, pane: &str, plan: Vec<Value>) -> Result<(), String> {
        self.post("/keys", &serde_json::json!({ "pane": pane, "keys": plan }))
            .await
    }

    /// Visible terminal text. Older nodes return a plain-text health response
    /// for unknown GET paths, so require the exact screen response shape.
    pub async fn screen(&self, pane: &str) -> Result<String, String> {
        let reply: Value = self.get_json(&self.http, "/screen", &[("pane", pane.to_string())]).await?;
        parse_screen_reply(pane, &reply)
    }

    /// Streaming terminal endpoints require structured acknowledgements. Old
    /// nodes accept unknown POST paths as hooks with HTTP 204; that must never
    /// look like successful input or an empty terminal poll.
    pub async fn terminal_action(&self, action: &str, payload: &Value) -> Value {
        if !matches!(action, "open" | "poll" | "input" | "resize" | "close" | "history") {
            return serde_json::json!({"ok": false, "error": "Неизвестное действие терминала"});
        }
        let client = if action == "history" || (action == "open" && payload["historyLines"].as_u64().is_some_and(|n| n > 2000))
            || (action == "input" && payload["paste"] == true) { &self.poll } else { &self.http };
        let mut response = match client.post(format!("{}/terminal/{action}", self.base)).json(payload).send().await {
            Ok(response) => response,
            Err(error) => return serde_json::json!({"ok": false, "error": format!("Узел недоступен: {error}")}),
        };
        let status = response.status().as_u16();
        let mut body = Vec::new();
        // Byte arrays expand in JSON; allow a bounded history/initial snapshot
        // while refusing an unbounded or malformed response from a peer.
        const MAX_REPLY: usize = 20 * 1024 * 1024;
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) if body.len().saturating_add(chunk.len()) <= MAX_REPLY => body.extend_from_slice(&chunk),
                Ok(Some(_)) => return serde_json::json!({"ok": false, "error": "Ответ терминала слишком большой"}),
                Ok(None) => break,
                Err(error) => return serde_json::json!({"ok": false, "error": format!("Чтение терминала: {error}")}),
            }
        }
        let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        parse_terminal_reply(action, status, &value)
    }

    pub async fn sources(&self) -> Result<Value, String> {
        let value: Value = self.get_json(&self.http, "/sources", &[]).await?;
        value.get("sources").filter(|v| v.is_array()).cloned()
            .ok_or_else(|| "Узел не поддерживает источники агентов — обнови jarvis-node".into())
    }

    pub async fn repair_source(&self, source_id: &str) -> Result<(), String> {
        let response = Self::client(Duration::from_secs(100))?.post(format!("{}/sources/repair",self.base))
            .json(&serde_json::json!({"sourceId":source_id})).send().await.map_err(|e| format!("Настройка хуков: {e}"))?;
        let status = response.status().as_u16();
        let value: Value = response.json().await.map_err(|e| format!("Ответ настройки хуков: {e}"))?;
        parse_command_reply("/sources/repair",status,&value)
    }

    /// Bounded metadata only. Content is fetched through the existing checked /file endpoint.
    pub async fn sessions(&self) -> Result<Value, String> {
        let value: Value = self.get_json(&self.http, "/sessions", &[]).await?;
        if value.get("sessions").is_some_and(Value::is_array) { Ok(value) }
        else { Err("Узел не поддерживает каталог сессий — обнови jarvis-node".into()) }
    }

    /// Снимок живого: агенты, паны и живость спрошенных pid — одним запросом.
    ///
    /// Узел до этой ручки не знает и на неизвестный GET отвечает строкой
    /// «jarvis-node ok» — разбор её не примет, и ошибка честно скажет, что
    /// узел пора переустановить.
    pub async fn agents(&self, pids: &[i64]) -> Result<AgentsReply, String> {
        let list = pids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        self.get_json(&self.http, "/agents", &[("pids", list)]).await
    }

    /// Оглавление проектов машины: где там работали. Узел отдаёт только
    /// найденное на диске — что из этого показать, решает панель.
    pub async fn projects(&self) -> Result<Value, String> {
        self.get_json::<Value>(&self.http, "/projects", &[])
            .await
            .map(|v| v.get("projects").cloned().unwrap_or(Value::Array(Vec::new())))
    }

    /// Поднять сессию агента на той машине: каталог создаётся рекурсивно,
    /// команду собирает ноут (агент и флаги — его настройки).
    pub async fn launch(&self, cwd: &str, cmd: &str, name: &str) -> Result<(), String> {
        self.launch_pane(cwd, cmd, name).await.map(|_| ())
    }

    /// То же, но с паной в ответе. Связке она нужна: через пану уходят задача,
    /// сообщение о конфликте и прерывание — без неё рука неуправляема.
    pub async fn launch_pane(&self, cwd: &str, cmd: &str, name: &str) -> Result<(String, String), String> {
        self.launch_pane_source(cwd, cmd, name, None).await
    }
    pub async fn launch_pane_source(&self, cwd: &str, cmd: &str, name: &str, source_id: Option<&str>) -> Result<(String, String), String> {
        let resp = self
            .http
            .post(format!("{}/launch", self.base))
            .json(&serde_json::json!({ "cwd": cwd, "cmd": cmd, "name": name, "sourceId": source_id }))
            .send()
            .await
            .map_err(|e| format!("узел недоступен: {e}"))?;
        let status = resp.status();
        let body: serde_json::Value =
            resp.json().await.map_err(|e| format!("узел ответил не-JSON: {e}"))?;
        if !status.is_success() {
            return Err(body
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("узел отказал")
                .to_string());
        }
        let take = |k: &str| body.get(k).and_then(serde_json::Value::as_str).unwrap_or_default().to_string();
        Ok((take("session"), take("pane")))
    }

    /// Лимиты аккаунта с узла: текст `claude /usage` той машины как есть.
    ///
    /// Нужен, когда локально авторизации нет: человек работает на узле, и
    /// правда о лимитах живёт там же. Узел кэширует ответ на пять минут.
    pub async fn usage_text(&self, fresh: bool) -> Result<String, String> { self.usage_text_for(fresh, None).await }
    pub async fn usage_text_for(&self, fresh: bool, source_id: Option<&str>) -> Result<String, String> {
        let v: serde_json::Value = self
            .get_json(&self.http, "/usage", &[("fresh", if fresh { "1" } else { "0" }.to_string()), ("sourceId", source_id.unwrap_or_default().to_string())])
            .await?;
        let text = v.get("text").and_then(serde_json::Value::as_str).unwrap_or_default();
        if text.trim().is_empty() {
            Err(v.get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("узел вернул пустой /usage")
                .to_string())
        } else {
            Ok(text.to_string())
        }
    }

    /// Закрыть пану на узле: сессия там завершается вместе с ней.
    ///
    /// Успеха мало — нужен ОТВЕТ С ТЕЛОМ. Узел старой версии про `/kill` не
    /// знает, а POST на неизвестный путь у него означает «конверт от хука»:
    /// он молча кладёт тело в буфер и отвечает 204. Принять это за успех
    /// значило бы сказать «сессия завершена» живому агенту в лицо.
    pub async fn kill(&self, pane: &str) -> Result<(), String> {
        let resp = self
            .http
            .post(format!("{}/kill", self.base))
            .json(&serde_json::json!({ "pane": pane }))
            .send()
            .await
            .map_err(|e| format!("узел недоступен: {e}"))?;
        let status = resp.status();
        if status.as_u16() == 204 {
            return Err("узел не умеет завершать сессии — обнови jarvis-node".into());
        }
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(body
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("узел отказал")
                .to_string());
        }
        Ok(())
    }

    /// Живые паны узла — по ним видно, что удалённая сессия ещё жива.
    pub async fn panes(&self) -> Result<PanesReply, String> {
        self.get_json(&self.http, "/panes", &[]).await
    }
}

fn parse_screen_reply(pane: &str, reply: &Value) -> Result<String, String> {
    if let Some(error) = reply.get("error").and_then(Value::as_str).filter(|error| !error.is_empty()) {
        return Err(error.to_string());
    }
    if reply.get("pane").and_then(Value::as_str) != Some(pane) {
        return Err("Узел вернул экран другой паны — обнови jarvis-node".into());
    }
    reply.get("screen").and_then(Value::as_str).map(str::to_string)
        .ok_or_else(|| "Узел не вернул экран терминала — обнови jarvis-node".into())
}

fn terminal_needs_update() -> Value {
    serde_json::json!({"ok": false, "needsUpdate": true, "error": "Узел не поддерживает живой терминал — обнови jarvis-node"})
}

fn parse_terminal_reply(action: &str, status: u16, reply: &Value) -> Value {
    if matches!(status, 204 | 404 | 405) { return terminal_needs_update(); }
    if !(200..300).contains(&status) {
        return serde_json::json!({"ok": false, "error": reply["error"].as_str().map(str::to_string)
            .unwrap_or_else(|| format!("Узел не подтвердил терминал (HTTP {status})"))});
    }
    match reply["ok"].as_bool() {
        Some(false) => return reply.clone(),
        Some(true) => {},
        None => return terminal_needs_update(),
    }
    let bytes = |value: &Value| value.as_array().is_some_and(|array| array.iter().all(|v| v.as_u64().is_some_and(|b| b <= 255)));
    let valid = match action {
        "open" => reply["streamId"].as_str().is_some_and(|id| !id.is_empty() && id.len() <= 160)
            && reply["cursor"].is_u64() && reply["cols"].is_u64() && reply["rows"].is_u64()
            && bytes(&reply["initial"]) && reply["historyTruncated"].is_boolean(),
        "poll" => reply["cursor"].is_u64() && reply["closed"].is_boolean() && reply["gap"].is_boolean()
            && reply["chunks"].as_array().is_some_and(|chunks| chunks.iter().all(|chunk| chunk["seq"].is_u64() && bytes(&chunk["data"]))),
        "history" => reply["text"].is_string() && reply["truncated"].is_boolean(),
        "input" | "resize" | "close" => true,
        _ => false,
    };
    if valid { reply.clone() } else { terminal_needs_update() }
}

fn parse_command_reply(path: &str, status: u16, reply: &Value) -> Result<(), String> {
    // An old node records unknown POSTs as hooks and returns 204. That is
    // delivery to its event buffer, not acknowledgement of terminal input.
    if status == 204 {
        return Err(format!("Узел не подтвердил {path} — обнови jarvis-node"));
    }
    if let Some(error) = reply.get("error").and_then(Value::as_str).filter(|error| !error.is_empty()) {
        return Err(error.to_string());
    }
    if (200..300).contains(&status) && reply.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(())
    } else {
        Err(format!("Узел не подтвердил {path} (HTTP {status})"))
    }
}

#[cfg(test)]
mod terminal_screen_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn terminal_open_checks_capability_before_posting_to_a_legacy_node() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 1024];
                let count = socket.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&chunk[..count]);
                if request.windows(4).any(|part| part == b"\r\n\r\n") { break; }
                assert!(request.len() <= 8192);
            }
            let body = r#"{"version":"0.3.3","capabilities":["screen"]}"#;
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap().lines().next().unwrap().to_string()
        });
        let node = Node::new(RemoteCfg { name: "legacy-terminal-test".into(), ssh_host: "example.invalid".into(), ..Default::default() });
        node.tunnel.port.store(port, Ordering::SeqCst);
        let reply = node.terminal_action("open", &json!({"pane":"%4"})).await;
        assert_eq!(reply["needsUpdate"], true);
        assert_eq!(server.await.unwrap(), "GET /hello HTTP/1.1");
    }

    #[test]
    fn stream_protocol_rejects_old_node_fallbacks_and_malformed_successes() {
        for action in ["open", "poll", "input", "resize", "close", "history"] {
            for (status, reply) in [(204, Value::Null), (200, json!({})), (404, json!({"ok":true}))] {
                assert_eq!(parse_terminal_reply(action, status, &reply)["needsUpdate"], true);
            }
        }
        let open = json!({"ok":true,"streamId":"node-1","cursor":0,"cols":80,"rows":24,"initial":[27,91,72],"historyTruncated":false});
        assert_eq!(parse_terminal_reply("open", 200, &open), open);
        let mut invalid = open.clone(); invalid["initial"] = json!([256]);
        assert_eq!(parse_terminal_reply("open", 200, &invalid)["needsUpdate"], true);
        let poll = json!({"ok":true,"cursor":1,"chunks":[{"seq":1,"data":[0,255]}],"closed":false,"gap":false});
        assert_eq!(parse_terminal_reply("poll", 200, &poll), poll);
        assert_eq!(parse_terminal_reply("poll", 200, &json!({"ok":true}))["ok"], false);
        assert_eq!(parse_terminal_reply("history", 200, &json!({"ok":true,"text":"full output","truncated":false}))["ok"], true);
        assert_eq!(parse_terminal_reply("input", 200, &json!({"ok":true}))["ok"], true);
        let failure = json!({"ok":false,"closed":true,"error":"stream expired"});
        assert_eq!(parse_terminal_reply("poll", 200, &failure), failure);
        assert_eq!(parse_terminal_reply("input", 502, &json!({"ok":true}))["ok"], false);
    }

    #[test]
    fn terminal_input_requires_real_acknowledgement_not_legacy_hook_acceptance() {
        assert!(parse_command_reply("/keys", 200, &json!({"ok":true})).is_ok());
        for (status, reply) in [(204, Value::Null), (200, json!({})), (200, json!({"ok":false})), (502, json!({"ok":true}))] {
            assert!(parse_command_reply("/keys", status, &reply).is_err());
        }
        assert_eq!(parse_command_reply("/reply", 502, &json!({"error":"can't find pane: %4"})).unwrap_err(), "can't find pane: %4");
    }

    #[test]
    fn terminal_screen_preserves_unicode_and_accepts_an_empty_live_screen() {
        for screen in ["", "Привет 👋\n❯ готов"] {
            assert_eq!(parse_screen_reply("%7", &json!({"pane":"%7","screen":screen})).unwrap(), screen);
        }
    }

    #[test]
    fn terminal_screen_cannot_masquerade_as_healthy_when_node_reports_an_error() {
        let missing = json!({"pane":"%7","screen":"","error":"can't find pane: %7"});
        assert_eq!(parse_screen_reply("%7", &missing).unwrap_err(), "can't find pane: %7");
        for reply in [json!({}), json!("jarvis-node ok"), json!({"pane":"%8","screen":"other session"}), json!({"pane":"%7"})] {
            assert!(parse_screen_reply("%7", &reply).is_err());
        }
    }
}

/* ======================= 4. курсор и поллер ======================= */

/// Партия событий узла наружу.
#[derive(Debug, Clone, Default)]
pub struct Batch {
    /// Имя узла.
    pub remote: String,
    /// Конверты хуков в порядке возникновения, уже помеченные узлом (см.
    /// [`stamp`]): их можно скармливать редьюсеру как локальные.
    pub events: Vec<Value>,
    /// Курсор после этой партии — с него пойдёт следующий запрос.
    pub cursor: u64,
    /// Была потеря. Ноут обязан перечитать транскрипты целиком: делать вид,
    /// что ничего не потерялось, — худшее из возможных поведений.
    pub gap: bool,
}

/// Куда отдавать события узлов. Демон подставляет сюда свой приём событий.
pub type Sink = Arc<dyn Fn(Batch) + Send + Sync + 'static>;

/// Пометить конверт узлом: имя в `remote` и префикс `<узел>:` у `session_id`.
///
/// Идентификаторы сессий пространство имён не делят: агенты на разных машинах
/// спокойно получают одинаковый id, и без префикса они слились бы в одну строку
/// реестра. Идемпотентна — второй проход по уже помеченному конверту ничего не
/// меняет (иначе двойная пометка на стыке с демоном давала бы `vps:vps:id`).
pub fn stamp(remote: &str, mut envelope: Value) -> Value {
    let Some(obj) = envelope.as_object_mut() else {
        return envelope;
    };
    obj.insert("remote".to_string(), Value::from(remote));
    let prefix = format!("{remote}:");
    if let Some(sid) = obj
        .get_mut("payload")
        .and_then(Value::as_object_mut)
        .and_then(|p| p.get_mut("session_id"))
    {
        let cur = sid.as_str().unwrap_or_default().to_string();
        if !cur.is_empty() && !cur.starts_with(&prefix) {
            *sid = Value::from(format!("{prefix}{cur}"));
        }
    }
    envelope
}

/// Свести страницу узла к партии наружу — вся логика курсора и потерь.
///
/// Потеря объявляется в двух случаях: узел честно сказал `gap:true` (буфер
/// переполнился, пока ноут спал) ИЛИ курсор поехал назад — узел перезапустился
/// и начал нумерацию заново. Второе снаружи выглядит как «новых событий нет» и
/// молча съело бы всё, что произошло после рестарта узла.
pub fn fold_page(remote: &str, prev: u64, page: EventsPage) -> Batch {
    let rewound = page.cursor < prev;
    let mut records = page.events;
    // Reconnect может повторить хвост страницы. Сначала упорядочиваем его
    // по серверному курсору, затем убираем уже принятые записи и дубли.
    // При рестарте узла прежние курсоры больше не сравнимы — принимаем новый
    // диапазон и сообщаем gap для восстановления состояния.
    records.sort_by_key(|record| record.cursor);
    records.dedup_by_key(|record| record.cursor);
    // Ring::since uses [since, next): cursor zero is the first real event.
    records.retain(|record| record.cursor < page.cursor
        && (rewound || record.cursor >= prev)
        && !record.envelope.is_null());
    Batch {
        remote: remote.to_string(),
        // конверт достаём из обёртки узла: `at` и покусочный курсор нужны были
        // ленте, а редьюсеру демона — ровно тот же конверт, что от локального хука
        events: records
            .into_iter()
            .map(|r| {
                let mut envelope = stamp(remote, r.envelope);
                if let Some(obj) = envelope.as_object_mut() {
                    obj.insert("remoteAt".into(), Value::from(r.at));
                    obj.insert("remoteCursor".into(), Value::from(r.cursor));
                }
                envelope
            })
            .collect(),
        // курсор не откатываем назад без причины: страница без событий и с
        // нулевым cursor не должна заставлять перечитывать буфер заново
        cursor: if rewound { page.cursor } else { page.cursor.max(prev) },
        gap: page.gap || rewound,
    }
}

/// Имя узла попадает в путь файла курсора, а приходит оно из настроек — то есть
/// из рук человека. Всё, кроме `[A-Za-z0-9._-]`, заменяем: `../` в имени не
/// должен уводить запись за пределы каталога.
pub fn safe_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() || s.chars().all(|c| c == '.') {
        "_".to_string() // "." и ".." — не имена файлов
    } else {
        s
    }
}

/// Файл курсора узла: `<jarvis_dir>/remotes/<name>.cursor`.
pub fn cursor_path(name: &str) -> PathBuf {
    jarvis_dir()
        .join("remotes")
        .join(format!("{}.cursor", safe_name(name)))
}

/// Курсор с диска. Нет файла или он битый → 0: узел отдаст всё, что есть в
/// буфере, и сам объявит gap, если начало уже вытеснено.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedCursor {
    cursor: u64,
    instance: String,
}

fn parse_cursor(text: &str) -> SavedCursor {
    serde_json::from_str(text).unwrap_or_else(|_| SavedCursor {
        cursor: text.trim().parse().unwrap_or(0),
        instance: String::new(),
    })
}

fn read_cursor(name: &str) -> SavedCursor {
    std::fs::read_to_string(cursor_path(name)).map(|s| parse_cursor(&s)).unwrap_or_default()
}

/// Курсор на диск: перезапуск ноута не должен ни перечитывать буфер заново
/// (дубли событий), ни терять хвост. Права 0600 — как у всего в каталоге
/// Jarvis, хотя секретов в файле нет.
fn write_cursor(name: &str, cursor: u64, instance: &str) {
    let path = cursor_path(name);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let saved = serde_json::json!({"cursor": cursor, "instance": instance}).to_string();
    let _ = crate::stt::transcripts::write_private_atomic(&path, saved.as_bytes());
}

fn align_instance(previous: &str, since: u64, page: &mut EventsPage) {
    if !page.instance.is_empty() && page.instance != previous
        && (!previous.is_empty() || since > 0) {
        // Re-fetch this incarnation from zero after telling the reducer to
        // refresh transcripts. Filtering the page at an old offset loses data.
        page.cursor = 0;
        page.events.clear();
        page.gap = true;
    }
}

/// Состояние узла для панели/диагностики. Форма — та, что читает вкладка
/// «Удалённые» (`ui/settings2.js`): конфиг узла ⊕ живость, чтобы IPC-слою
/// осталось просто отдать этот список.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStatus {
    pub name: String,
    pub ssh_host: String,
    pub jarvis_dir: String,
    pub ssh_config_file: Option<String>,
    pub transport: String,
    pub teleport_proxy: Option<String>,
    pub teleport_cluster: Option<String>,
    pub node_tcp_port: Option<u16>,
    pub run_as_user: Option<String>,
    pub protocol: u32,
    pub capabilities: Vec<String>,
    pub sources: Vec<Value>,
    /// Последний круг поллера удался (туннель жив и узел ответил).
    pub connected: bool,
    /// Локальный порт туннеля; 0 — туннеля нет.
    pub port: u16,
    pub cursor: u64,
    /// Версия узла с последнего рукопожатия. Пусто — ещё не здоровались.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// Узел старее приложения. Он ставится приложением и живёт на чужой машине
    /// месяцами: без явной отметки об этом человек не узнает, что половина
    /// починенного до него просто не доехала.
    pub outdated: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// Один удалённый узел: туннель, курсор и счётчик неудач для backoff.
pub struct Node {
    pub cfg: RemoteCfg,
    pub tunnel: Tunnel,
    cursor: AtomicU64,
    instance: Mutex<String>,
    /// Клиент кэшируется по порту: пересобирать пул на каждый запрос незачем,
    /// но при переподъёме туннеля порт меняется и клиент устаревает.
    client: Mutex<Option<(u16, Arc<NodeClient>)>>,
    online: AtomicBool,
    /// Подряд неудачных кругов поллера — множитель backoff.
    fails: AtomicU64,
    last_error: Mutex<String>,
    /// Версия узла с последнего рукопожатия.
    version: Mutex<String>,
    hello: Mutex<Hello>,
}

impl Node {
    pub fn new(cfg: RemoteCfg) -> Self {
        let cursor = read_cursor(&cfg.name);
        Node {
            tunnel: Tunnel::new(&cfg),
            cursor: AtomicU64::new(cursor.cursor),
            instance: Mutex::new(cursor.instance),
            client: Mutex::new(None),
            online: AtomicBool::new(false),
            fails: AtomicU64::new(0),
            last_error: Mutex::new(String::new()),
            version: Mutex::new(String::new()),
            hello: Mutex::new(Hello::default()),
            cfg,
        }
    }

    pub fn cursor(&self) -> u64 {
        self.cursor.load(Ordering::SeqCst)
    }

    /// HTTP-клиент к текущему порту туннеля.
    pub fn client(&self) -> Result<Arc<NodeClient>, String> {
        let port = self.tunnel.port();
        if port == 0 {
            return Err("туннель не поднят".to_string());
        }
        let mut g = self.client.lock().unwrap();
        if let Some((p, c)) = g.as_ref() {
            if *p == port {
                return Ok(c.clone());
            }
        }
        let c = Arc::new(NodeClient::new(self.tunnel.base())?);
        *g = Some((port, c.clone()));
        Ok(c)
    }

    pub async fn terminal_action(&self, action: &str, payload: &Value) -> Value {
        let client = match self.client() {
            Ok(client) => client,
            Err(error) => return serde_json::json!({"ok": false, "error": error}),
        };
        if action == "open" {
            // Refresh after an in-place node update; cached version strings do
            // not prove that a particular terminal protocol is implemented.
            match client.hello().await {
                Ok(hello) => {
                    let supported = hello.capabilities.iter().any(|cap| cap == "terminal.stream.v1");
                    *self.hello.lock().unwrap_or_else(|e| e.into_inner()) = hello;
                    if !supported { return terminal_needs_update(); }
                }
                Err(error) => return serde_json::json!({"ok": false, "error": error}),
            }
        } else if !self.hello.lock().unwrap_or_else(|e| e.into_inner()).capabilities.iter().any(|cap| cap == "terminal.stream.v1") {
            return terminal_needs_update();
        }
        client.terminal_action(action, payload).await
    }

    pub fn status(&self) -> RemoteStatus {
        // Версию берём ОДИН раз в переменную — и только потом собираем ответ.
        //
        // Раньше она читалась прямо в поле структуры, а соседнее поле звало
        // `outdated()`, который лезет за тем же замком. Временный страж живёт
        // до конца всего выражения, то есть до конца литерала структуры, —
        // значит второй захват происходил при живом первом. Обычный мьютекс не
        // реентерабелен: поток вставал навсегда, и вместе с ним весь список
        // машин. Отсюда и белый экран «Проектов», и вечные скелетоны в
        // настройках удалённых.
        let version = self.version.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let hello = self.hello.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let outdated = !version.is_empty()
            && (version != env!("CARGO_PKG_VERSION") || hello.protocol < 2);
        RemoteStatus {
            name: self.cfg.name.clone(),
            ssh_host: self.cfg.ssh_host.clone(),
            jarvis_dir: self.cfg.jarvis_dir.clone(),
            ssh_config_file: self.cfg.ssh_config_file.clone(),
            transport: self.cfg.transport.clone(),
            teleport_proxy: self.cfg.teleport_proxy.clone(),
            teleport_cluster: self.cfg.teleport_cluster.clone(),
            node_tcp_port: self.cfg.node_tcp_port,
            run_as_user: self.cfg.run_as_user.clone(),
            protocol: hello.protocol,
            capabilities: hello.capabilities,
            sources: hello.sources,
            connected: self.online.load(Ordering::SeqCst),
            port: self.tunnel.port(),
            cursor: self.cursor(),
            version,
            outdated,
            error: self.why(),
        }
    }

    /// Человеческая причина «почему не работает». Причина поллера отвечает
    /// ЧТО не вышло («туннель не поднялся»), жалоба ssh — ПОЧЕМУ («Permission
    /// denied», «administratively prohibited»). По отдельности каждая половина
    /// бесполезна, поэтому отдаём обе.
    /// Отстал ли узел от приложения. Сравниваем строки как есть: версии у них
    /// общие по построению (выпускаются вместе), и «не равно» здесь честнее
    /// любого разбора на компоненты — расхождение в любую сторону означает,
    /// что узел ставился другой сборкой.
    pub fn outdated(&self) -> bool {
        let v = self.version.lock().unwrap_or_else(|e| e.into_inner());
        !v.is_empty() && (*v != env!("CARGO_PKG_VERSION")
            || self.hello.lock().unwrap_or_else(|e| e.into_inner()).protocol < 2)
    }

    /// Запомнить версию с рукопожатия.
    pub fn saw_version(&self, v: &str) {
        if !v.is_empty() {
            *self.version.lock().unwrap() = v.to_string();
        }
    }

    pub fn why(&self) -> String {
        let own = self.last_error.lock().unwrap().clone();
        let ssh = self.tunnel.last_stderr();
        match (own.is_empty(), ssh.is_empty()) {
            (true, true) => String::new(),
            (true, false) => ssh,
            (false, true) => own,
            (false, false) => format!("{own} · ssh: {ssh}"),
        }
    }

    /// Круг удался: связь есть, backoff сбрасываем.
    fn ok(&self) {
        self.fails.store(0, Ordering::SeqCst);
        self.last_error.lock().unwrap().clear();
        self.tunnel.clear_stderr(); // связь есть — прошлая жалоба только путает
        if !self.online.swap(true, Ordering::SeqCst) {
            crate::log::line(&format!("[remote] {}: связь есть", self.cfg.name));
        }
    }

    /// Круг не удался: запоминаем причину и возвращаем паузу до следующего.
    /// Логируем только смену состояния — иначе лежащий VPS зальёт лог.
    fn fail(&self, why: &str) -> Duration {
        let n = self.fails.fetch_add(1, Ordering::SeqCst);
        *self.last_error.lock().unwrap() = why.to_string();
        if self.online.swap(false, Ordering::SeqCst) || n == 0 {
            crate::log::line(&format!("[remote] {}: связи нет — {why}", self.cfg.name));
        }
        Duration::from_secs(backoff_secs(n))
    }

    /// Продвинуть курсор и сохранить его. Пишем только на изменении: холостые
    /// long-poll-круги не должны трогать диск.
    fn advance(&self, cursor: u64, incoming_instance: &str) {
        let mut instance = self.instance.lock().unwrap();
        let instance_changed = !incoming_instance.is_empty() && *instance != incoming_instance;
        if instance_changed {
            *instance = incoming_instance.to_string();
        }
        if self.cursor.swap(cursor, Ordering::SeqCst) != cursor || instance_changed {
            write_cursor(&self.cfg.name, cursor, &instance);
        }
    }
}

/// Цикл одного узла: держим туннель, тянем `/events` с курсора, отдаём наружу.
///
/// Ошибки не фатальны и не выходят из цикла: VPS уходит в перезагрузку, ноут —
/// в сон, сеть меняется. Всё это лечится ожиданием с backoff, а не остановкой
/// поллера — иначе «проснулся, а событий нет» стало бы нормой.
async fn poll_loop(node: Arc<Node>, sink: Sink) {
    let mut replay_before: u64 = 0;
    loop {
        // 1. Туннель. Без него HTTP смысла не имеет.
        let n = node.clone();
        let state = tokio::task::spawn_blocking(move || n.tunnel.ensure_started())
            .await
            .unwrap_or(TunnelState::Failed);
        if state == TunnelState::Failed {
            tokio::time::sleep(node.fail("туннель не поднялся")).await;
            continue;
        }

        // 2. Рукопожатие после свежего подъёма: форвард открывается не мгновенно,
        // да и версия узла в логе экономит час разбирательств при рассинхроне.
        if state == TunnelState::Spawned {
            let ready_secs = if node.cfg.transport == "teleport" { TELEPORT_READY_SECS } else { TUNNEL_READY_SECS };
            let n = node.clone();
            let ready = tokio::task::spawn_blocking(move || {
                n.tunnel.wait_ready(Duration::from_secs(ready_secs))
            })
            .await
            .unwrap_or(false);
            if !ready {
                // Форвард так и не открылся: ssh либо не пустили, либо форвард
                // запрещён на той стороне. Причину, если ssh её назвал, уже
                // держит туннель — она уедет в панель вместе с этой.
                let transport = if node.cfg.transport == "teleport" { "Teleport" } else { "SSH" };
                let pause = node.fail(&format!("{transport} не открыл туннель за {ready_secs} секунд"));
                let n = node.clone();
                let _ = tokio::task::spawn_blocking(move || n.tunnel.kick()).await;
                tokio::time::sleep(pause).await;
                continue;
            }
            let hello = match node.client() {
                Ok(c) => c.hello().await,
                Err(e) => Err(e),
            };
            match hello {
                Ok(h) => {
                    replay_before = h.cursor;
                    *node.hello.lock().unwrap_or_else(|e| e.into_inner()) = h.clone();
                    node.saw_version(&h.version);
                    if node.outdated() {
                        crate::log::line(&format!(
                            "[remote] {}: узел v{} старее приложения v{} — переустанови его",
                            node.cfg.name,
                            h.version,
                            env!("CARGO_PKG_VERSION"),
                        ));
                    }
                    crate::log::line(&format!(
                        "[remote] {}: узел {} v{} (uptime {}с, в буфере {})",
                        node.cfg.name,
                        h.host,
                        h.version,
                        h.uptime_ms / 1000,
                        h.buffered
                    ));
                }
                Err(e) => {
                    let pause = node.fail(&e);
                    let n = node.clone();
                    let _ = tokio::task::spawn_blocking(move || n.tunnel.kick()).await;
                    tokio::time::sleep(pause).await;
                    continue;
                }
            }
        }

        // 3. Одна страница событий (long-poll на той стороне).
        let since = node.cursor();
        let instance = node.instance.lock().unwrap().clone();
        let page = match node.client() {
            Ok(c) => c.events(since, &instance).await,
            Err(e) => Err(e),
        };
        match page {
            Ok(mut page) => {
                node.ok();
                align_instance(&instance, since, &mut page);
                let incoming_instance = page.instance.clone();
                let mut batch = fold_page(&node.cfg.name, since, page);
                for event in &mut batch.events {
                    let replay = event.get("remoteCursor").and_then(Value::as_u64).is_some_and(|cursor| cursor < replay_before);
                    if let Some(obj) = event.as_object_mut() { obj.insert("remoteReplay".into(), Value::Bool(replay)); }
                }
                if batch.gap {
                    crate::log::line(&format!(
                        "[remote] {}: потеря событий (gap), курсор {since} → {}",
                        node.cfg.name, batch.cursor
                    ));
                }
                let (cursor, empty) = (batch.cursor, batch.events.is_empty());
                // Пустая страница без потери — нормальный исход long-poll, будить
                // ей демона незачем.
                if !empty || batch.gap {
                    (sink.as_ref())(batch);
                }
                // Курсор двигаем ПОСЛЕ выдачи наружу: падение между ответом узла и
                // разбором должно приводить к повтору партии, а не к её потере —
                // дубль событий редьюсер переживает, пропажу нет.
                node.advance(cursor, &incoming_instance);
                if empty {
                    tokio::time::sleep(Duration::from_millis(POLL_FLOOR_MS)).await;
                }
            }
            Err(e) => {
                let pause = node.fail(&e);
                // HTTP не отвечает при живом ssh — подвис узел или форвард.
                // Лечится только переподъёмом, поэтому роняем туннель.
                let n = node.clone();
                let _ = tokio::task::spawn_blocking(move || n.tunnel.kick()).await;
                tokio::time::sleep(pause).await;
            }
        }
    }
}

/// Реестр удалённых узлов: по задаче-поллеру на каждый.
pub struct Remotes {
    nodes: Mutex<Vec<Arc<Node>>>,
    tasks: Mutex<Vec<tauri::async_runtime::JoinHandle<()>>>,
}

impl Remotes {
    pub fn new() -> Self {
        Remotes {
            nodes: Mutex::new(Vec::new()),
            tasks: Mutex::new(Vec::new()),
        }
    }

    /// Поднять узлы из настроек. Повторный вызов (настройки изменились) сначала
    /// гасит прежние: список узлов описывается целиком, точечный дифф тут только
    /// добавил бы состояний, в которых туннель живёт от старого конфига.
    pub fn start(&self, cfgs: Vec<RemoteCfg>, sink: Sink) {
        self.stop_all();
        // Сначала собираем всё, что нужно, и только потом трогаем замки: под
        // ними не должно происходить ничего, кроме записи.
        let fresh: Vec<Arc<Node>> = cfgs.into_iter().map(|cfg| Arc::new(Node::new(cfg))).collect();
        let spawned: Vec<_> = fresh
            .iter()
            .map(|n| tauri::async_runtime::spawn(poll_loop(n.clone(), sink.clone())))
            .collect();
        let count = fresh.len();
        self.nodes.lock().unwrap().extend(fresh);
        self.tasks.lock().unwrap().extend(spawned);
        if count > 0 {
            crate::log::line(&format!("[remote] узлов в работе: {count}"));
        }
    }

    /// Погасить всё: сначала задачи, потом туннели — иначе поллер успел бы
    /// поднять убитый ssh заново.
    pub fn stop_all(&self) {
        let tasks: Vec<_> = self.tasks.lock().unwrap().drain(..).collect();
        for t in tasks {
            t.abort();
        }
        // Список забираем и отпускаем замок СРАЗУ. Раньше он держался всё
        // время, пока умирали ssh-процессы, — а список читает `machines_list`,
        // синхронная команда, то есть главный поток. Медленно умирающий
        // туннель вешал этим всё окно; ровно это и происходило на вкладке
        // «Проекты», которая с этого списка и начинается.
        let nodes: Vec<_> = self.nodes.lock().unwrap().drain(..).collect();
        for n in nodes {
            n.tunnel.stop();
        }
    }

    /// Погасить всё и дождаться. Только для выхода из приложения — см.
    /// `Tunnel::stop_now`.
    pub fn stop_all_now(&self) {
        for t in self.tasks.lock().unwrap().drain(..).collect::<Vec<_>>() {
            t.abort();
        }
        for n in self.nodes.lock().unwrap().drain(..).collect::<Vec<_>>() {
            n.tunnel.stop_now();
        }
    }

    /// Узел по имени — точка входа для маршрутизации действий (ответ, пульт,
    /// чтение транскрипта) в нужную машину.
    pub fn node(&self, name: &str) -> Option<Arc<Node>> {
        self.nodes
            .lock()
            .unwrap()
            .iter()
            .find(|n| n.cfg.name == name)
            .cloned()
    }

    /// Все узлы разом — для обходов вроде сверки живости сессий.
    pub fn all(&self) -> Vec<Arc<Node>> {
        self.nodes.lock().unwrap().clone()
    }

    /// Состояние всех узлов — для панели и диагностики.
    pub fn list(&self) -> Vec<RemoteStatus> {
        // Отравленный замок — это чужая паника в прошлом, а не повод уронить
        // ещё и эту команду: данные под ним целы, читаем их как есть.
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        nodes.iter().map(|n| n.status()).collect()
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    /// Список узлов читает `machines_list` — СИНХРОННАЯ команда, то есть
    /// главный поток. Если гашение туннелей держит тот же замок, пока умирает
    /// ssh, окно встаёт целиком. Ровно это и происходило на вкладке
    /// «Проекты», которая с этого списка начинается.
    #[test]
    fn stopping_does_not_hold_the_node_list() {
        let remotes = Remotes::new();
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let d = done.clone();
        // Пока «гасим», кто-то читает список — и обязан пройти насквозь.
        let reader = std::thread::spawn(move || {
            for _ in 0..100 {
                let _ = remotes.list();
            }
            d.store(true, Ordering::SeqCst);
        });
        reader.join().unwrap();
        assert!(done.load(Ordering::SeqCst), "чтение списка не должно упираться в гашение");
    }

    /// Снятие статуса не должно вставать на собственном замке.
    ///
    /// Так уже было: версия читалась прямо в поле структуры, а соседнее поле
    /// звало `outdated()` за тем же замком. Временный страж живёт до конца
    /// всего выражения, поэтому второй захват приходился на живой первый —
    /// и поток вставал навсегда вместе со всем списком машин.
    ///
    /// Проверяем в отдельном потоке с ожиданием: тест обязан ПАДАТЬ, а не
    /// висеть, иначе он утащит за собой весь прогон.
    #[test]
    fn status_does_not_deadlock_on_its_own_lock() {
        let node = std::sync::Arc::new(Node::new(RemoteCfg {
            name: "t".into(),
            ssh_host: "nowhere.invalid".into(),
            jarvis_dir: "/tmp".into(),
            ..RemoteCfg::default()
        }));
        node.saw_version("0.0.1"); // непустая версия — путь, на котором и вставало
        let (tx, rx) = std::sync::mpsc::channel();
        let n = node.clone();
        std::thread::spawn(move || {
            let st = n.status();
            let _ = tx.send(st.version);
        });
        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("status() встал на собственном замке");
        assert_eq!(got, "0.0.1");
    }

    /// Мёртвый туннель хоронится, но не на плечах вызывающего.
    #[test]
    fn stop_returns_without_waiting_for_the_child() {
        let cfg = RemoteCfg {
            name: "t".into(),
            ssh_host: "nowhere.invalid".into(),
            jarvis_dir: "/tmp".into(),
            ..RemoteCfg::default()
        };
        let t = Tunnel::new(&cfg);
        let at = std::time::Instant::now();
        t.stop();
        assert!(at.elapsed() < Duration::from_millis(200), "stop обязан возвращаться сразу");
        assert_eq!(t.port(), 0);
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_or_empty_key_gives_no_remotes() {
        assert!(parse_remotes(&json!({})).is_empty());
        assert!(parse_remotes(&json!({ "remotes": [] })).is_empty());
        // не массив — настройки правили руками, но падать не за что
        assert!(parse_remotes(&json!({ "remotes": "vps" })).is_empty());
    }

    #[test]
    fn parses_full_entry() {
        let v = json!({ "remotes": [
            { "name": "vps", "sshHost": "vps.example", "jarvisDir": "/home/bob/.jarvis" }
        ]});
        assert_eq!(
            parse_remotes(&v),
            vec![RemoteCfg {
                name: "vps".into(),
                ssh_host: "vps.example".into(),
                jarvis_dir: "/home/bob/.jarvis".into(),
            ..RemoteCfg::default()
            }]
        );
    }

    #[test]
    fn trims_and_defaults_dir() {
        let v = json!({ "remotes": [{ "name": " vps ", "sshHost": " vps.example " }] });
        let out = parse_remotes(&v);
        assert_eq!(out[0].name, "vps");
        assert_eq!(out[0].ssh_host, "vps.example");
        assert_eq!(out[0].jarvis_dir, DEFAULT_REMOTE_DIR, "каталог по умолчанию");
        // хвостовой слэш снимаем — иначе путь к сокету станет `…//node.sock`
        let v = json!({ "remotes": [
            { "name": "a", "sshHost": "h", "jarvisDir": "/srv/jarvis/" }
        ]});
        assert_eq!(parse_remotes(&v)[0].jarvis_dir, "/srv/jarvis");
    }

    #[test]
    fn skips_broken_entries_and_duplicate_names() {
        let v = json!({ "remotes": [
            { "sshHost": "no-name.example" },              // без имени
            { "name": "no-host" },                          // без хоста
            "строка вместо объекта",
            { "name": "vps", "sshHost": "first.example" },
            { "name": "vps", "sshHost": "second.example" }, // дубль имени
            { "name": "box", "sshHost": "box.example" },
        ]});
        let out = parse_remotes(&v);
        assert_eq!(out.len(), 2, "остались только целые и уникальные");
        assert_eq!(out[0].ssh_host, "first.example", "побеждает первый");
        assert_eq!(out[1].name, "box");
    }
}

#[cfg(test)]
mod ssh_tests {
    use super::*;

    #[test]
    fn builds_expected_argv() {
        let args = ssh_args("vps.example", 45123, "/home/bob/.jarvis/node.sock");
        assert_eq!(
            args,
            vec![
                "-N",
                "-o",
                "BatchMode=yes",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-L",
                "127.0.0.1:45123:/home/bob/.jarvis/node.sock",
                "vps.example",
            ]
        );
    }

    #[test]
    fn host_is_last_and_never_an_option() {
        // хост идёт последним аргументом: ssh не примет опции после него
        let args = ssh_args("user@vps", 1, "/s.sock");
        assert_eq!(args.last().unwrap(), "user@vps");
        assert_eq!(args.iter().filter(|a| *a == "-L").count(), 1);
    }

    #[test]
    fn node_sock_joins_without_double_slash() {
        assert_eq!(node_sock("/home/bob/.jarvis"), "/home/bob/.jarvis/node.sock");
        assert_eq!(node_sock("/home/bob/.jarvis/"), "/home/bob/.jarvis/node.sock");
    }

    #[test]
    fn backoff_grows_and_saturates() {
        assert_eq!(backoff_secs(0), 1);
        assert_eq!(backoff_secs(1), 2);
        assert_eq!(backoff_secs(4), 16);
        // потолок: и на пятой, и на сотой неудаче ждём не больше MAX_BACKOFF_SECS
        assert_eq!(backoff_secs(5), MAX_BACKOFF_SECS);
        assert_eq!(backoff_secs(100), MAX_BACKOFF_SECS);
    }
}

#[cfg(test)]
mod gap_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn queued_tunnel_start_cannot_rearm_a_removed_remote() {
        let tunnel = Tunnel::new(&RemoteCfg::default());
        tunnel.stop();
        assert_eq!(tunnel.ensure_started(), TunnelState::Failed);
        assert_eq!(tunnel.port(), 0);
        assert!(!tunnel.is_up());
    }

    fn page(cursor: u64, gap: bool, n: usize) -> EventsPage {
        EventsPage {
            instance: String::new(),
            cursor,
            gap,
            events: (0..n)
                .map(|i| Recorded {
                    cursor: cursor - (n - i) as u64,
                    at: 1_700_000_000_000 + i as i64,
                    envelope: json!({
                        "event": "stop",
                        "payload": { "session_id": format!("s{i}") },
                    }),
                })
                .collect(),
        }
    }

    #[test]
    fn normal_page_advances_cursor_without_gap() {
        let b = fold_page("vps", 10, page(14, false, 4));
        assert_eq!(b.remote, "vps");
        assert_eq!(b.cursor, 14);
        assert_eq!(b.events.len(), 4);
        assert!(!b.gap);
    }

    #[test]
    fn empty_page_keeps_cursor() {
        let b = fold_page("vps", 10, page(10, false, 0));
        assert_eq!(b.cursor, 10);
        assert!(b.events.is_empty());
        assert!(!b.gap);
    }

    #[test]
    fn real_node_first_event_is_not_dropped_at_the_exclusive_page_boundary() {
        // Shape observed through the live SSH fixture: event 0, next cursor 1.
        let page: EventsPage = serde_json::from_str(r#"{"cursor":1,"instance":"node-a","events":[{"cursor":0,"at":1788562492808,"envelope":{"agent":"claude","event":"prompt","payload":{"session_id":"qa-synthetic","prompt":"Привет 👋"}}}]}"#).unwrap();
        let batch = fold_page("qa", 0, page);
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0]["payload"]["session_id"], "qa:qa-synthetic");
        assert_eq!(batch.cursor, 1);
    }

    #[test]
    fn changed_instance_rewinds_even_when_new_node_passed_previous_cursor() {
        let mut fresh = page(20, false, 3);
        fresh.instance = "node-b".into();
        align_instance("node-a", 10, &mut fresh);
        assert!(fresh.gap);
        assert_eq!(fresh.cursor, 0);
        assert!(fresh.events.is_empty());
        let mut next = page(4, false, 4);
        next.instance = "node-b".into();
        align_instance("node-b", 0, &mut next);
        assert_eq!(fold_page("qa", 0, next).events.len(), 4);
    }

    #[test]
    fn legacy_cursor_migration_is_safe_and_old_nodes_remain_supported() {
        let old = parse_cursor("42\n");
        assert_eq!(old.cursor, 42);
        assert!(old.instance.is_empty());
        let new = parse_cursor(r#"{"cursor":42,"instance":"node-a"}"#);
        assert_eq!((new.cursor, new.instance.as_str()), (42, "node-a"));
        let mut upgraded = page(45, false, 3);
        upgraded.instance = "node-a".into();
        align_instance(&old.instance, old.cursor, &mut upgraded);
        assert!(upgraded.gap);
        assert_eq!(upgraded.cursor, 0);
        let mut legacy = page(45, false, 3);
        align_instance("node-a", 42, &mut legacy);
        assert!(!legacy.gap);
        assert_eq!(legacy.cursor, 45);
    }

    #[test]
    fn reconnect_page_is_sorted_and_does_not_replay_old_or_duplicate_events() {
        let record = |cursor| Recorded { cursor, at: 0,
            envelope: json!({"event":"stop","payload":{"session_id":format!("s{cursor}")}}) };
        let b = fold_page("vps", 10, EventsPage { cursor: 13, gap: false, instance: String::new(),
            events: vec![record(13), record(9), record(12), record(12), record(11), record(10), record(14)] });
        let ids: Vec<_> = b.events.iter().map(|event| event["payload"]["session_id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["vps:s10", "vps:s11", "vps:s12"]);
        assert_eq!(b.cursor, 13);
        assert!(!b.gap);
    }

    #[test]
    fn node_reported_gap_is_propagated_with_events() {
        // буфер переполнился, пока ноут спал: события отдаём, но честно говорим,
        // что часть потеряна — ноут перечитает транскрипты
        let b = fold_page("vps", 10, page(900, true, 3));
        assert!(b.gap);
        assert_eq!(b.cursor, 900);
        assert_eq!(b.events.len(), 3, "уцелевшие события не выбрасываем");
    }

    #[test]
    fn rewound_cursor_counts_as_gap() {
        // узел перезапустился и начал нумерацию заново: снаружи это выглядит
        // как «новых событий нет» и молча съело бы весь хвост
        let b = fold_page("vps", 900, page(5, false, 2));
        assert!(b.gap, "откат курсора — тоже потеря");
        assert_eq!(b.cursor, 5, "идём за узлом, а не держим мёртвый курсор");
    }

    #[test]
    fn envelope_is_unwrapped_and_stamped() {
        // редьюсеру демона нужен конверт хука, а не обёртка ленты узла
        let b = fold_page("vps", 0, page(2, false, 2));
        assert_eq!(b.events.len(), 2);
        assert_eq!(b.events[0]["event"], json!("stop"), "конверт достали из обёртки");
        assert_eq!(b.events[0]["remote"], json!("vps"));
        // id пространство имён не делит: без префикса сессии разных машин слиплись бы
        assert_eq!(b.events[0]["payload"]["session_id"], json!("vps:s0"));
        assert_eq!(b.events[1]["payload"]["session_id"], json!("vps:s1"));
    }

    #[test]
    fn stamping_twice_does_not_double_the_prefix() {
        let once = stamp("vps", json!({ "payload": { "session_id": "abc" } }));
        let twice = stamp("vps", once);
        assert_eq!(twice["payload"]["session_id"], json!("vps:abc"));
        assert_eq!(twice["remote"], json!("vps"));
    }

    #[test]
    fn stamp_survives_envelopes_without_payload() {
        // конверт без payload/session_id (или вовсе не объект) не должен паниковать
        let e = stamp("vps", json!({ "event": "ping" }));
        assert_eq!(e["remote"], json!("vps"));
        assert_eq!(stamp("vps", json!("строка")), json!("строка"));
    }

    #[test]
    fn zero_cursor_from_fresh_node_is_a_gap_too() {
        // узел поднялся с пустым буфером и отдал cursor=0: молча оставить свой
        // курсор 42 значило бы навсегда ослепнуть на этом узле
        let b = fold_page("vps", 42, page(0, false, 0));
        assert!(b.gap, "нулевой курсор при живом prev — откат, значит потеря");
        assert_eq!(b.cursor, 0);
    }
}

#[cfg(test)]
mod cursor_path_tests {
    use super::*;

    #[test]
    fn safe_name_neutralizes_traversal() {
        assert_eq!(safe_name("vps"), "vps");
        assert_eq!(safe_name("my-box_1.2"), "my-box_1.2");
        assert_eq!(safe_name("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(safe_name(".."), "_");
        assert_eq!(safe_name(""), "_");
        assert_eq!(safe_name("a/b"), "a_b");
    }

    #[test]
    fn cursor_file_stays_inside_remotes_dir() {
        let p = cursor_path("../evil");
        assert_eq!(p.parent().unwrap(), jarvis_dir().join("remotes"));
        assert_eq!(p.file_name().unwrap(), ".._evil.cursor");
    }
}

#[cfg(test)]
mod link_tests {
    use super::*;

    #[test]
    fn home_survives_a_chatty_shell() {
        // чужой ~/.bashrc печатает в stdout — раньше это молча убивало туннель
        let out = "Добро пожаловать!\nОбновлений: 3\nJARVIS_HOME=/home/bob\n";
        assert_eq!(parse_home(out).as_deref(), Some("/home/bob"));
        assert_eq!(parse_home("JARVIS_HOME=/srv/x/\n").as_deref(), Some("/srv/x/"));
        assert_eq!(parse_home("JARVIS_HOME=/\n").as_deref(), Some("/"), "root тоже человек");
    }

    #[test]
    fn home_without_the_marker_is_not_a_home() {
        assert_eq!(parse_home("/home/bob\n"), None, "голый вывод больше не принимаем");
        assert_eq!(parse_home("JARVIS_HOME=relative/path"), None);
        assert_eq!(parse_home(""), None);
    }

    #[test]
    fn ssh_chatter_is_not_a_failure() {
        // из-за этих строк человек искал бы поломку там, где её нет
        assert!(is_ssh_noise("Warning: Permanently added '1.2.3.4' (ED25519) to the list of known hosts."));
        assert!(is_ssh_noise("bash: warning: setlocale: LC_ALL: cannot change locale (en_GB.UTF-8)"));
        assert!(!is_ssh_noise("Permission denied (publickey)."));
        assert!(!is_ssh_noise("channel 2: open failed: administratively prohibited"));
    }
}

#[cfg(test)]
mod chunk_tests {
    use super::*;

    #[test]
    fn from_file_start_keeps_first_line() {
        let (text, next) = cut_to_lines("{\"a\":1}\n{\"b\":2}\n", 0);
        assert_eq!(text, "{\"a\":1}\n{\"b\":2}\n");
        assert_eq!(next, 16);
    }

    #[test]
    fn mid_file_drops_torn_first_line() {
        // читали с 100-го байта: начало первой строки осталось позади
        let (text, next) = cut_to_lines("ся\n{\"b\":2}\n", 100);
        assert_eq!(text, "{\"b\":2}\n");
        assert_eq!(next, 100 + 5 + 8, "обрезка тоже съедает байты");
    }

    #[test]
    fn unfinished_last_line_waits_for_next_read() {
        // агент дописывает строку прямо сейчас — половинку не отдаём
        let (text, next) = cut_to_lines("{\"a\":1}\n{\"b\"", 0);
        assert_eq!(text, "{\"a\":1}\n");
        assert_eq!(next, 8, "продолжим ровно с недописанной строки");
    }

    #[test]
    fn no_complete_line_yields_nothing() {
        let (text, next) = cut_to_lines("{\"a\"", 0);
        assert!(text.is_empty());
        assert_eq!(next, 0, "не сдвигаемся: строка ещё не дописана");
    }

    #[test]
    fn rewound_detects_truncated_file() {
        let c = FileChunk { from: 0, next: 10, size: 10, ..Default::default() };
        assert!(c.rewound(500), "узел отдал раньше запрошенного — файл переписали");
        assert!(!c.rewound(0));
    }
}
