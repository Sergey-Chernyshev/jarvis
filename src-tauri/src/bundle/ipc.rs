//! Команды панели и такт связки.
//!
//! Такт — сердце режима: раз в несколько секунд он смотрит на каждую руку
//! глазами git и её же сессии (хуки приносят cwd — по нему рука и узнаётся),
//! двигает состояния и готовит очередь. Все правки состояния — точечные, через
//! `store.with`: такт работает долго, а панель в это время может добавить руку
//! или нажать паузу, и батч-запись затёрла бы её действия.

use super::host::Host;
use super::{git, launch, Bundle, Hand, HandState};
use crate::daemon::Daemon;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;

/* ================= снимок для панели ================= */

/// Где исполняется связка. Узел ищется в настройках удалённых машин — его
/// ssh-хост и есть транспорт для git и гейтов.
fn host_for(d: &Arc<Daemon>, b: &Bundle) -> Result<Host, String> {
    let m = b.machine.trim();
    if m.is_empty() || m == "local" {
        return Ok(Host::Local);
    }
    match d.remotes.node(m) {
        Some(node) => Ok(Host::Ssh { machine: m.to_string(), host: node.cfg.ssh_host.clone() }),
        None => Err(format!("узел «{m}» не найден в настройках удалённых машин")),
    }
}

/// Сессия руки: локальная ищется без пометки узла, удалённая — с ней.
/// cwd — единственная ниточка между рукой и её сессией.
fn session_of(d: &Arc<Daemon>, machine: &str, worktree: &str) -> Option<crate::model::Session> {
    let local = machine.is_empty() || machine == "local";
    let sessions = d.sessions.lock().unwrap_or_else(|e| e.into_inner());
    sessions
        .values()
        .find(|s| {
            let same_host = if local { s.remote.is_none() } else { s.remote.as_deref() == Some(machine) };
            same_host && s.cwd.as_deref() == Some(worktree)
        })
        .cloned()
}

/// Сообщение в чат руки — локально через tmux, на узле через его /reply.
async fn send_to_hand(d: &Arc<Daemon>, b: &Bundle, pane: &str, text: &str) -> Result<(), String> {
    match host_for(d, b)? {
        Host::Local => crate::tmux::reply(pane, text).await,
        Host::Ssh { machine, .. } => {
            let node = d.remotes.node(&machine).ok_or("узел пропал из настроек")?;
            node.client()?.reply(pane, text).await
        }
    }
}

pub fn snapshot(d: &Arc<Daemon>) -> Value {
    let bundles = d.bundles.store.all();
    let items: Vec<Value> = bundles.iter().map(|b| bundle_view(d, b)).collect();
    json!({ "ok": true, "bundles": items })
}

fn bundle_view(d: &Arc<Daemon>, b: &Bundle) -> Value {
    let queue: Vec<&str> = b.queue().iter().map(|h| h.id.as_str()).collect();
    let hands: Vec<Value> = b
        .hands
        .iter()
        .map(|h| {
            let sess = session_of(d, &b.machine, &h.worktree);
            let (sid, status, detail) = match &sess {
                Some(s) => (
                    Some(s.id.clone()),
                    serde_json::to_value(s.status).unwrap_or(Value::Null),
                    s.detail.clone(),
                ),
                None => (None, Value::Null, String::new()),
            };
            let tokens = sid
                .as_ref()
                .and_then(|id| d.usage.for_session(id))
                .and_then(|u| u.get("tok").and_then(Value::as_f64))
                .unwrap_or(0.0);
            let pos = queue.iter().position(|id| *id == h.id).map(|p| p + 1);
            json!({
                "id": h.id,
                "name": h.name,
                "task": h.task,
                "branch": h.branch,
                "worktree": h.worktree,
                "state": h.state,
                "queuePos": pos,
                "attempt": h.attempt,
                "conflictFiles": h.conflict_files,
                "gatesOk": h.gates_ok,
                "sessionId": sid,
                "status": status,
                "detail": detail,
                "tokens": tokens,
                // Влить можно только голову очереди, и только когда всё зелёное.
                "canMerge": pos == Some(1) && h.gates_ok,
            })
        })
        .collect();
    json!({
        "id": b.id,
        "name": b.name,
        "machine": if b.machine.trim().is_empty() { "local" } else { b.machine.trim() },
        "dir": b.dir,
        "base": b.base,
        "gates": b.gates,
        "budgetTokens": b.budget_tokens,
        "paused": b.paused,
        "createdAt": b.created_at,
        "lastMergeAt": b.last_merge_at,
        "active": b.active(),
        "problems": b.problems(),
        "hands": hands,
        "events": b.events,
        "hotFiles": hot_files(b),
    })
}

/// Горячие файлы: те, которых коснулись две руки и больше. Прямой прогноз
/// конфликта — и первое, на что стоит смотреть на пульте.
fn hot_files(b: &Bundle) -> Vec<Value> {
    use std::collections::BTreeMap;
    let mut by_file: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for h in &b.hands {
        if h.state == HandState::Merged {
            continue;
        }
        for f in &h.conflict_files {
            by_file.entry(f).or_default().push(&h.name);
        }
        for f in &h.touched {
            by_file.entry(f).or_default().push(&h.name);
        }
    }
    let mut out: Vec<Value> = by_file
        .into_iter()
        .filter(|(_, hands)| {
            let mut names = hands.clone();
            names.dedup();
            names.len() >= 2
        })
        .map(|(file, mut hands)| {
            hands.dedup();
            json!({ "file": file, "hands": hands })
        })
        .collect();
    out.truncate(8);
    out
}

pub fn push(d: &Arc<Daemon>) {
    crate::windows::emit_to_panel(&d.app, "bundle-state", &snapshot(d));
}

/* ================= помощники состояния ================= */

fn set_hand(d: &Arc<Daemon>, bid: &str, hid: &str, f: impl FnOnce(&mut Hand)) {
    d.bundles.store.with(bid, |b| {
        if let Some(h) = b.hands.iter_mut().find(|h| h.id == hid) {
            f(h);
        }
    });
}

fn add_event(d: &Arc<Daemon>, bid: &str, text: String) {
    d.bundles.store.with(bid, |b| b.event(text));
}

/* ================= команды панели ================= */

#[tauri::command]
pub fn bundle_get(app: AppHandle) -> Value {
    snapshot(&Daemon::get(&app))
}

/// Заготовка новой связки: гейты по умолчанию, три пустые руки.
/// Ничего не сохраняет — черновик живёт в панели до «Запустить».
#[tauri::command]
pub fn bundle_draft() -> Value {
    json!({
        "ok": true,
        "item": {
            "id": "",
            "name": "",
            "machine": "local",
            "dir": "",
            "base": "",
            "gates": [
                { "name": "тесты", "command": "cargo test" },
                { "name": "clippy", "command": "cargo clippy --all-targets -- -D warnings" },
            ],
            "budgetTokens": 60_000,
            "paused": false,
            "hands": [ { "task": "" }, { "task": "" }, { "task": "" } ],
            "events": [],
        },
    })
}

/// Точки входа обзора: дом машины и её известные проекты.
///
/// Известные — те же, что видит вкладка «Проекты»: локально из истории
/// транскриптов, на узле из его оглавления. Один клик вместо набора пути по
/// памяти — ради этого обзор и существует.
#[tauri::command]
pub async fn bundle_places(app: AppHandle, machine: String) -> Value {
    let d = Daemon::get(&app);
    let probe = Bundle { machine: machine.clone(), ..Default::default() };
    let host = match host_for(&d, &probe) {
        Ok(h) => h,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let home = match host.home().await {
        Ok(h) => h,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let known = known_dirs(&d, &machine).await;
    json!({ "ok": true, "home": home, "known": known })
}

/// Известные проекты машины — их каталоги.
async fn known_dirs(d: &Arc<Daemon>, machine: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if machine.is_empty() || machine == "local" {
        let projects = d.history.projects(&d.usage);
        if let Some(arr) = projects.as_array() {
            for g in arr {
                if let Some(cwd) = g.get("cwd").and_then(Value::as_str) {
                    if !cwd.is_empty() {
                        out.push(cwd.to_string());
                    }
                }
            }
        }
    } else if let Some(node) = d.remotes.node(machine) {
        if let Ok(client) = node.client() {
            if let Ok(list) = client.projects().await {
                if let Some(arr) = list.as_array() {
                    for p in arr {
                        if let Some(cwd) = p.get("cwd").and_then(Value::as_str) {
                            if !cwd.is_empty() {
                                out.push(cwd.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    out.truncate(24);
    out
}

/// Подкаталоги пути — шаг обзора.
#[tauri::command]
pub async fn bundle_browse(app: AppHandle, machine: String, path: String) -> Value {
    let d = Daemon::get(&app);
    let probe = Bundle { machine, ..Default::default() };
    let host = match host_for(&d, &probe) {
        Ok(h) => h,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let path = if path.trim().is_empty() {
        match host.home().await {
            Ok(h) => h,
            Err(e) => return json!({ "ok": false, "error": e }),
        }
    } else {
        path.trim().trim_end_matches('/').to_string()
    };
    match host.list_dirs(&path).await {
        Ok(dirs) => json!({
            "ok": true,
            "path": path,
            "parent": super::host::parent_of(&path),
            "dirs": dirs,
        }),
        Err(e) => json!({ "ok": false, "error": e, "path": path }),
    }
}

/// Сохранить конфигурацию. Руки нормализуются: пустые задачи выбрасываются,
/// имена достраиваются из задач.
#[tauri::command]
pub fn bundle_save(app: AppHandle, item: Value) -> Value {
    let d = Daemon::get(&app);
    let mut b: Bundle = match serde_json::from_value(item) {
        Ok(b) => b,
        Err(e) => return json!({ "ok": false, "error": format!("не разобрал связку: {e}") }),
    };
    if b.id.is_empty() {
        b.id = format!("bundle-{}", crate::util::now_ms());
        b.created_at = crate::util::now_ms();
    }
    b.hands.retain(|h| !h.task.trim().is_empty() || h.state != HandState::New);
    for (i, h) in b.hands.iter_mut().enumerate() {
        if h.id.is_empty() {
            h.id = format!("hand-{}-{i}", crate::util::now_ms());
        }
        if h.name.trim().is_empty() {
            h.name = super::hand_name(&h.task);
        }
    }
    // Прежние события и время создания переживают правку формы.
    if let Some(old) = d.bundles.store.get(&b.id) {
        if b.events.is_empty() {
            b.events = old.events;
        }
        if b.created_at == 0 {
            b.created_at = old.created_at;
        }
        if b.last_merge_at == 0 {
            b.last_merge_at = old.last_merge_at;
        }
    }
    let problems = b.problems();
    let id = b.id.clone();
    d.bundles.store.save(b);
    push(&d);
    json!({ "ok": true, "id": id, "problems": problems })
}

/// Запустить связку: поднять все руки, у которых есть задача и нет агента.
#[tauri::command]
pub async fn bundle_start(app: AppHandle, id: String) -> Value {
    let d = Daemon::get(&app);
    let Some(b) = d.bundles.store.get(&id) else {
        return json!({ "ok": false, "error": "связка не найдена" });
    };
    let problems = b.problems();
    if !problems.is_empty() {
        return json!({ "ok": false, "error": problems.join("; ") });
    }
    let host = match host_for(&d, &b) {
        Ok(h) => h,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let dir = b.dir.trim().to_string();
    if matches!(host, Host::Ssh { .. }) && !dir.starts_with('/') {
        return json!({ "ok": false, "error": "на узле нужен абсолютный путь — ~ раскрывать некому" });
    }
    // Директория — как в «Проектах»: нет каталога — создадим, нет git —
    // инициализируем, нет коммитов — закоммитим лежащее. База определяется
    // здесь же и запоминается: на неё смотрят очередь и авторебейз.
    let base = if b.base.trim().is_empty() {
        match git::ensure_repo(&host, &dir).await {
            Ok(x) => x,
            Err(e) => return json!({ "ok": false, "error": e }),
        }
    } else {
        if let Err(e) = git::ensure_repo(&host, &dir).await {
            return json!({ "ok": false, "error": e });
        }
        b.base.trim().to_string()
    };
    d.bundles.store.with(&id, |b| b.base = base.clone());

    let launching: Vec<Hand> =
        b.hands.iter().filter(|h| h.state == HandState::New && !h.task.trim().is_empty()).cloned().collect();
    if launching.is_empty() {
        return json!({ "ok": false, "error": "нет рук к запуску — все уже подняты" });
    }
    let dd = d.clone();
    tauri::async_runtime::spawn(async move {
        for hand in launching {
            launch_hand(&dd, &id, &hand).await;
            push(&dd);
        }
    });
    json!({ "ok": true })
}

/// Поднять одну руку: worktree → ветка → tmux → первое сообщение.
async fn launch_hand(d: &Arc<Daemon>, bid: &str, hand: &Hand) {
    let Some(b) = d.bundles.store.get(bid) else { return };
    let host = match host_for(d, &b) {
        Ok(h) => h,
        Err(e) => {
            set_hand(d, bid, &hand.id, |h| h.state = HandState::Failed);
            add_event(d, bid, format!("{}: не поднялась — {e}", hand.name));
            return;
        }
    };
    let dir = b.dir.trim().to_string();
    let slug = unique_slug(&b, &hand.name, &hand.task);
    let branch = format!("team/{slug}");
    // Worktree — сосед директории, как в дизайне: ../wt-<имя>. На виду, а не
    // в недрах ~/.jarvis: человек в него заглядывает.
    let wt = format!("{}/wt-{slug}", super::host::parent_of(&dir));

    let fail = |d: &Arc<Daemon>, why: String| {
        set_hand(d, bid, &hand.id, |h| h.state = HandState::Failed);
        add_event(d, bid, format!("{}: не поднялась — {}", hand.name, why));
    };

    if let Err(e) = git::add_worktree(&host, &dir, &wt, &branch, &b.base).await {
        return fail(d, e);
    }
    // Канонический путь нужен для сверки с cwd из хуков; удалённый путь и так
    // абсолютный, а Path этой машины про чужую ФС ничего не знает.
    let wt_canon = match &host {
        Host::Local => std::path::Path::new(&wt)
            .canonicalize()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or(wt.clone()),
        Host::Ssh { .. } => wt.clone(),
    };
    // Задача + правила руки. Коммиты — не пожелание: очередь слияний видит
    // только закоммиченное, рука без коммитов не станет готовой никогда.
    let brief = format!(
        "{task}\n\nТы — рука связки «{name}» в отдельном worktree ({wt}), ветка {branch}. \
         Работай только в этом каталоге. Закончив, закоммить всё осмысленными коммитами — \
         несделанный коммит для очереди слияний не существует. Проверки перед готовностью: {gates}.",
        task = hand.task.trim(),
        name = b.name,
        wt = wt_canon,
        branch = branch,
        gates = if b.gates.is_empty() {
            "нет".to_string()
        } else {
            b.gates.iter().map(|g| g.command.as_str()).collect::<Vec<_>>().join("; ")
        },
    );
    let dangerous = d.settings.bool("launchDangerous");
    let pane = match &host {
        Host::Local => {
            let cmd = match launch::hand_command(dangerous) {
                Ok(c) => c,
                Err(e) => return fail(d, e),
            };
            match launch::spawn(std::path::Path::new(&wt_canon), &slug, &cmd).await {
                Ok(p) => p,
                Err(e) => return fail(d, e),
            }
        }
        Host::Ssh { machine, .. } => {
            // На узле бинарь агента резолвит сам узел (он добавляет PATH), а
            // вопрос доверия к папке подтверждает его launch — как в «Проектах».
            let cmd = crate::launch::agent_command("claude", None, dangerous);
            let node = match d.remotes.node(machine) {
                Some(n) => n,
                None => return fail(d, "узел пропал из настроек".into()),
            };
            let client = match node.client() {
                Ok(c) => c,
                Err(e) => return fail(d, e),
            };
            match client.launch_pane(&wt_canon, &cmd, &slug).await {
                Ok((_session, pane)) => pane,
                Err(e) => return fail(d, e),
            }
        }
    };
    if let Err(e) = send_to_hand(d, &b, &pane, &brief).await {
        return fail(d, format!("сессия поднялась, но задача не доехала: {e}"));
    }
    set_hand(d, bid, &hand.id, |h| {
        h.branch = branch.clone();
        h.worktree = wt_canon.clone();
        h.pane = pane.clone();
        h.state = HandState::Working;
    });
    add_event(d, bid, format!("{}: рука запущена · {branch}", hand.name));
}

/// Слаг руки, не совпадающий с уже занятыми ветками связки.
fn unique_slug(b: &Bundle, name: &str, task: &str) -> String {
    let base = crate::loops::model::slug(if name.trim().is_empty() { task } else { name });
    let taken: Vec<&str> = b.hands.iter().map(|h| h.branch.as_str()).collect();
    if !taken.contains(&format!("team/{base}").as_str()) {
        return base;
    }
    for n in 2..20 {
        let cand = format!("{base}-{n}");
        if !taken.contains(&format!("team/{cand}").as_str()) {
            return cand;
        }
    }
    format!("{base}-{}", crate::util::now_ms() % 1000)
}

/// Добавить руку в живую связку — и сразу поднять.
#[tauri::command]
pub async fn bundle_add_hand(app: AppHandle, id: String, task: String, name: Option<String>) -> Value {
    let d = Daemon::get(&app);
    let task = task.trim().to_string();
    if task.is_empty() {
        return json!({ "ok": false, "error": "пустая задача" });
    }
    let hand = Hand {
        id: format!("hand-{}", crate::util::now_ms()),
        name: name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| super::hand_name(&task)),
        task,
        ..Default::default()
    };
    let hand_for_launch = hand.clone();
    if d.bundles.store.with(&id, |b| b.hands.push(hand)).is_none() {
        return json!({ "ok": false, "error": "связка не найдена" });
    }
    let dd = d.clone();
    let bid = id.clone();
    tauri::async_runtime::spawn(async move {
        launch_hand(&dd, &bid, &hand_for_launch).await;
        push(&dd);
    });
    json!({ "ok": true })
}

/// Пауза всем: остановить автоматику и прервать работающих агентов.
#[tauri::command]
pub async fn bundle_pause(app: AppHandle, id: String, on: bool) -> Value {
    let d = Daemon::get(&app);
    let Some(b) = d.bundles.store.with(&id, |b| {
        b.paused = on;
        b.event(if on { "пауза всем".into() } else { "связка продолжает".to_string() });
    }) else {
        return json!({ "ok": false, "error": "связка не найдена" });
    };
    if on {
        for h in b.hands.iter().filter(|h| h.state == HandState::Working && !h.pane.is_empty()) {
            let working = session_of(&d, &b.machine, &h.worktree)
                .is_some_and(|s| s.status == crate::model::Status::Working);
            if !working {
                continue;
            }
            match host_for(&d, &b) {
                Ok(Host::Local) | Err(_) => launch::interrupt(&h.pane).await,
                Ok(Host::Ssh { machine, .. }) => {
                    if let Some(node) = d.remotes.node(&machine) {
                        if let Ok(c) = node.client() {
                            let _ = c.keys(&h.pane, vec![json!({ "key": "Escape" })]).await;
                        }
                    }
                }
            }
        }
    }
    push(&d);
    json!({ "ok": true })
}

/// Влить голову очереди. Кнопка — только у человека; здесь лишь проверяем,
/// что нажата она на том, на чём можно.
#[tauri::command]
pub async fn bundle_merge(app: AppHandle, id: String, hand: String) -> Value {
    let d = Daemon::get(&app);
    let Some(b) = d.bundles.store.get(&id) else {
        return json!({ "ok": false, "error": "связка не найдена" });
    };
    let queue = b.queue();
    let Some(head) = queue.first() else {
        return json!({ "ok": false, "error": "очередь пуста" });
    };
    if head.id != hand {
        return json!({ "ok": false, "error": "вливается только голова очереди — по одному, с гейтами между" });
    }
    if !head.gates_ok {
        return json!({ "ok": false, "error": "гейты не зелёные — вливать рано" });
    }
    let host = match host_for(&d, &b) {
        Ok(h) => h,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    if let Err(e) = git::ff_advance(&host, &b.dir, &b.base, &head.branch).await {
        add_event(&d, &id, format!("{}: вливание не прошло — {e}", head.name));
        push(&d);
        return json!({ "ok": false, "error": e });
    }
    let head_id = head.id.clone();
    let branch = head.branch.clone();
    d.bundles.store.with(&id, |b| {
        if let Some(h) = b.hands.iter_mut().find(|h| h.id == head_id) {
            h.state = HandState::Merged;
            h.merged_at = crate::util::now_ms();
        }
        b.last_merge_at = crate::util::now_ms();
        b.event(format!("ты влил {branch} → {} · хвост переребейзится сам", b.base));
    });
    push(&d);
    json!({ "ok": true })
}

/// Убрать связку. Ветки остаются — в них работа; worktree влитых и упавших
/// рук прибираются.
#[tauri::command]
pub async fn bundle_remove(app: AppHandle, id: String) -> Value {
    let d = Daemon::get(&app);
    let Some(b) = d.bundles.store.get(&id) else {
        return json!({ "ok": false, "error": "связка не найдена" });
    };
    if let Ok(host) = host_for(&d, &b) {
        for h in &b.hands {
            if h.worktree.is_empty() {
                continue;
            }
            let _ = host.git(&b.dir, &["worktree", "remove", "--force", &h.worktree]).await;
        }
    }
    d.bundles.store.remove(&id);
    push(&d);
    json!({ "ok": true })
}

/* ================= такт ================= */

/// Один проход по всем живым связкам. Зовётся таймером; сам себя не дублирует.
pub async fn tick(d: &Arc<Daemon>) {
    if !d.bundles.claim_tick() {
        return;
    }
    let bundles = d.bundles.store.all();
    for b in bundles.iter().filter(|b| b.active() && !b.paused) {
        tick_bundle(d, b).await;
    }
    d.bundles.release_tick();
}

async fn tick_bundle(d: &Arc<Daemon>, b: &Bundle) {
    let host = match host_for(d, b) {
        Ok(h) => h,
        Err(_) => return, // узел пропал из настроек: молча ждём его возвращения
    };
    let mut changed = false;
    for h in &b.hands {
        let moved = match h.state {
            HandState::Working => tick_working(d, b, h, &host).await,
            HandState::Ready => tick_ready(d, b, h, &host).await,
            HandState::Conflict => tick_conflict(d, b, h, &host).await,
            _ => false,
        };
        changed = changed || moved;
    }
    if changed {
        push(d);
    }
}

/// Агент руки сейчас занят? Занятость — по её сессии; без сессии судим по
/// живости паны: агент мог ещё не прислать ни одного хука.
async fn busy(d: &Arc<Daemon>, b: &Bundle, h: &Hand, host: &Host) -> bool {
    match session_of(d, &b.machine, &h.worktree) {
        Some(s) => matches!(s.status, crate::model::Status::Working | crate::model::Status::Waiting),
        None => match host {
            Host::Local => crate::tmux::pane_alive(&h.pane).await,
            Host::Ssh { machine, .. } => match d.remotes.node(machine).and_then(|n| n.client().ok()) {
                Some(c) => c
                    .panes()
                    .await
                    .map(|r| r.panes.iter().any(|p| p.pane == h.pane))
                    .unwrap_or(false),
                None => false,
            },
        },
    }
}

/// Рабочая рука: ждём, пока агент закончит и закоммитит, — тогда ребейз,
/// гейты и очередь.
async fn tick_working(d: &Arc<Daemon>, b: &Bundle, h: &Hand, host: &Host) -> bool {
    if busy(d, b, h, host).await {
        return false;
    }
    if git::dirty(host, &h.worktree).await {
        return false; // агент замолчал, не закоммитив, — не наша очередь решать
    }
    if git::ahead(host, &b.dir, &b.base, &h.branch).await == 0 {
        return false; // коммитов нет — готовности нет
    }
    if !git::rebased(host, &b.dir, &b.base, &h.branch).await {
        match git::try_rebase(host, &h.worktree, &b.base).await {
            Ok(git::Rebase::Clean) => {}
            Ok(git::Rebase::Conflict(files)) => {
                to_conflict(d, b, h, files).await;
                return true;
            }
            Err(e) => {
                add_event(d, &b.id, format!("{}: ребейз не удался — {e}", h.name));
                return true;
            }
        }
    }
    run_gates_and_queue(d, b, h, host).await
}

/// Готовая рука: база могла уехать после чужого вливания — переребейз и гейты
/// заново; агент мог дописать — тогда она снова рабочая.
async fn tick_ready(d: &Arc<Daemon>, b: &Bundle, h: &Hand, host: &Host) -> bool {
    if busy(d, b, h, host).await {
        set_hand(d, &b.id, &h.id, |h| {
            h.state = HandState::Working;
            h.gates_ok = false;
        });
        add_event(d, &b.id, format!("{}: агент снова работает — вышла из очереди", h.name));
        return true;
    }
    if git::rebased(host, &b.dir, &b.base, &h.branch).await {
        let sha = git::head_sha(host, &h.worktree).await.unwrap_or_default();
        if sha != h.checked_sha {
            // Дописала коммит, стоя в очереди, — гейты пересдать.
            return run_gates_and_queue(d, b, h, host).await;
        }
        return false;
    }
    match git::try_rebase(host, &h.worktree, &b.base).await {
        Ok(git::Rebase::Clean) => {
            add_event(d, &b.id, format!("{}: авторебейз на свежий {} — гейты заново", h.name, b.base));
            run_gates_and_queue(d, b, h, host).await;
            true
        }
        Ok(git::Rebase::Conflict(files)) => {
            to_conflict(d, b, h, files).await;
            true
        }
        Err(e) => {
            add_event(d, &b.id, format!("{}: ребейз не удался — {e}", h.name));
            true
        }
    }
}

/// Конфликтная рука: агент чинит у себя; как только ветка снова на базе и
/// дерево чисто — обратно в строй через гейты.
async fn tick_conflict(d: &Arc<Daemon>, b: &Bundle, h: &Hand, host: &Host) -> bool {
    if busy(d, b, h, host).await {
        return false;
    }
    if git::rebased(host, &b.dir, &b.base, &h.branch).await && !git::dirty(host, &h.worktree).await {
        add_event(d, &b.id, format!("{}: конфликт решён — гейты и обратно в очередь", h.name));
        return run_gates_and_queue(d, b, h, host).await;
    }
    false
}

/// Выпадение в конфликт: откат уже сделан, теперь — сообщение агенту руки.
/// «Чинит сам» из дизайна — ровно этот путь.
async fn to_conflict(d: &Arc<Daemon>, b: &Bundle, h: &Hand, files: Vec<String>) {
    let attempt = h.attempt + 1;
    set_hand(d, &b.id, &h.id, |h| {
        h.state = HandState::Conflict;
        h.gates_ok = false;
        h.attempt = attempt;
        h.conflict_files = files.clone();
    });
    add_event(
        d,
        &b.id,
        format!("{}: конфликт при ребейзе — {} · чинит сам, попытка {attempt}", h.name, files.join(", ")),
    );
    let msg = format!(
        "Конфликт при ребейзе на {base}: {files}. Сделай `git rebase {base}` в своём worktree, \
         реши конфликты по смыслу обеих сторон, заверши ребейз (`git rebase --continue`) и \
         добейся зелёных проверок. Очередь слияний ждёт этот фикс.",
        base = b.base,
        files = files.join(", "),
    );
    if let Err(e) = send_to_hand(d, b, &h.pane, &msg).await {
        add_event(d, &b.id, format!("{}: не смог передать конфликт агенту — {e}", h.name));
    }
}

/// Гейты на текущей голове; зелёные — в очередь (или подтверждение готовности).
async fn run_gates_and_queue(d: &Arc<Daemon>, b: &Bundle, h: &Hand, host: &Host) -> bool {
    let sha = git::head_sha(host, &h.worktree).await.unwrap_or_default();
    let runs = run_gates(host, &b.gates, &h.worktree).await;
    let ok = runs.iter().all(|g| g.ok);
    let files = git::changed_files(host, &b.dir, &b.base, &h.branch).await;
    let was = h.state;
    set_hand(d, &b.id, &h.id, |h| {
        h.checked_sha = sha.clone();
        h.gates_ok = ok;
        h.touched = files.clone();
        h.conflict_files.clear();
        if ok {
            if h.state != HandState::Ready {
                h.ready_at = crate::util::now_ms();
            }
            h.state = HandState::Ready;
        } else {
            h.state = HandState::Working;
        }
    });
    if ok {
        if was != HandState::Ready {
            add_event(d, &b.id, format!("{}: гейты зелёные → в очередь", h.name));
        }
    } else {
        let red = runs.iter().find(|g| !g.ok);
        let name = red.map(|g| g.name.as_str()).unwrap_or("гейт");
        add_event(d, &b.id, format!("{}: красный гейт «{name}» — вернул агенту", h.name));
        let msg = format!(
            "Гейт «{name}» красный:\n{}\nПочини и закоммить — без зелёных проверок рука не встанет в очередь.",
            crate::loops::runner::tail(&red.map(|g| g.output.clone()).unwrap_or_default(), 25),
        );
        let _ = send_to_hand(d, b, &h.pane, &msg).await;
    }
    true
}

/// Прогнать гейты по порядку — там, где живёт связка. Первый красный
/// останавливает: гонять остальные нечего.
async fn run_gates(host: &Host, gates: &[crate::loops::model::Gate], cwd: &str) -> Vec<crate::loops::model::GateRun> {
    let mut out = Vec::new();
    for g in gates {
        let (code, text) = host.sh(cwd, &g.command, Duration::from_secs(1800)).await;
        let ok = code == 0;
        out.push(crate::loops::model::GateRun {
            name: g.name.clone(),
            ok,
            output: crate::loops::runner::tail(&text, 40),
        });
        if !ok {
            break;
        }
    }
    out
}
