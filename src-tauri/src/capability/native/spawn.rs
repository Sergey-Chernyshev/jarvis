//! Поднять и погасить сессию CLI — штатным инструментом, а не привычкой звать
//! tmux руками. `sessions.spawn` / `sessions.close`.
//!
//! Запуск идёт ТЕМ ЖЕ путём, что ручной из панели, — `ipc::launch_core`
//! (терминал/прокси/шаблон из настроек, `isolate` под worktree). Второй
//! реализации запуска в проекте нет и не будет.
//!
//! Главная особенность: сессия заводится ТОЛЬКО хуком CLI (`daemon::reduce`), а
//! хук приходит через секунды после того, как терминал открылся. Ждать его в
//! хендлере нельзя — ровно за блокирующее ожидание откатывали `sessions.wait`
//! (ход агента не завершён → он недоступен человеку). Поэтому `spawn` заводит
//! СВОЙ идентификатор (талон `spawn-…`) и возвращает его сразу, а связывание с
//! настоящей сессией доделывает фоновый ожидатель запуска (`ipc::deliver_task`):
//! он и так сторожит появление сессии, чтобы отдать ей первый промпт.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{json, Value};

use crate::backend::Agent;
use crate::capability::confirm_panel::gen_nonce;
use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::{Daemon, MAX_CHAT_NAME};
use crate::util::{now_ms, one_line};

use super::arg_str;

/* ================= учёт: кто поднял, зачем, когда ================= */

/// Запись о запуске. `session_id` пуст, пока CLI не прислал первый хук, — и это
/// не сбой, а нормальное состояние первых секунд жизни.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Spawn {
    pub ticket: String,
    /// Потребитель гейта, поднявший сессию (`agent`, `plugin:x`). По нему и
    /// только по нему решается, чья сессия своя.
    pub by: String,
    /// Откуда поднято: чат/сессия родителя. Человеку нужно дерево, а не россыпь.
    pub parent: Option<String>,
    pub name: String,
    pub agent: String,
    pub cwd: String,
    pub task: String,
    pub at: i64,
    pub session_id: Option<String>,
    pub closed: bool,
}

/// Куда бить `sessions.close`: сессия уже есть или ещё только едет.
#[derive(Debug, PartialEq)]
pub enum Target {
    /// Сессия в реестре — гасим её по-настоящему.
    Session(String),
    /// Хук ещё не пришёл: гасить нечего, снимаем талон.
    Pending,
}

/// Реестр запусков. Отдельная структура, а не поле в сессии: сессия исчезает
/// по session-end, а «кто её поднял» переживает её — по нему решается, чью
/// сессию агент вправе гасить, и кого перечислить в «продолжают работу».
#[derive(Default)]
pub struct Spawns {
    list: Mutex<Vec<Spawn>>,
}

impl Spawns {
    pub fn new() -> Self {
        Self::default()
    }

    /// Завести талон. Возвращает его id — это и есть тот id, который `spawn`
    /// отдаёт вызывающему, не дожидаясь ни хука, ни первого ответа.
    pub fn open(&self, by: &str, parent: Option<String>, plan: &Plan, now: i64) -> String {
        let ticket = format!("spawn-{}", &gen_nonce()[..12]);
        self.list.lock().unwrap().push(Spawn {
            ticket: ticket.clone(),
            by: by.to_string(),
            parent,
            name: plan.name.clone(),
            agent: plan.agent.label().to_string(),
            cwd: plan.cwd.clone(),
            task: plan.task.clone(),
            at: now,
            session_id: None,
            closed: false,
        });
        ticket
    }

    /// Талон дождался своей сессии.
    pub fn bind(&self, ticket: &str, sid: &str) -> bool {
        let mut list = self.list.lock().unwrap();
        match list.iter_mut().find(|s| s.ticket == ticket) {
            Some(s) => {
                s.session_id = Some(sid.to_string());
                true
            }
            None => false,
        }
    }

    /// Агент так и не встал — талон снимаем: иначе он навсегда останется
    /// «поднимающейся» сессией и в `sessions.get`, и в списке дочерних.
    pub fn give_up(&self, ticket: &str) {
        self.list.lock().unwrap().retain(|s| s.ticket != ticket);
    }

    /// Запись по талону ИЛИ по id сессии: снаружи это один и тот же запуск.
    pub fn find(&self, id: &str) -> Option<Spawn> {
        self.list
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.ticket == id || s.session_id.as_deref() == Some(id))
            .cloned()
    }

    pub fn mark_closed(&self, ticket: &str) {
        if let Some(s) = self.list.lock().unwrap().iter_mut().find(|s| s.ticket == ticket) {
            s.closed = true;
        }
    }

    /// Кого гасить по этому id — и вправе ли `by` это делать.
    ///
    /// Единственное правило: своё — это то, что этот же потребитель поднял через
    /// `spawn`. Сессия человека и сессия соседа в реестре запусков не значатся,
    /// поэтому отказ приходит сам собой и с причиной.
    pub fn child(&self, id: &str, by: &str) -> Result<(String, Target), String> {
        let Some(s) = self.find(id) else {
            return Err(format!(
                "«{id}» не поднималась через sessions.spawn — это чужая или ручная сессия, \
                 закрывать её нельзя. Свои запуски видны в поле spawnedBy у sessions.get"
            ));
        };
        if s.by != by {
            return Err(format!(
                "сессию «{}» поднял {} — закрывать можно только свои дочерние",
                s.name, s.by
            ));
        }
        if s.closed {
            return Err(format!("сессия «{}» уже закрыта", s.name));
        }
        let target = match &s.session_id {
            Some(sid) => Target::Session(sid.clone()),
            None => Target::Pending,
        };
        Ok((s.ticket, target))
    }

    /// Срез реестра. Копия, а не лок наружу: решать, кого показать человеку, —
    /// не дело учёта запусков, и тронуть записи по этому срезу нельзя.
    pub fn snapshot(&self) -> Vec<Spawn> {
        self.list.lock().unwrap().clone()
    }
}

/* ================= проверки до запуска ================= */

/// Чего просят. Сырьё из аргументов капабилити, ещё не проверенное.
#[derive(Debug, Default)]
pub struct Wanted {
    pub agent: String,
    pub model: Option<String>,
    pub cwd: String,
    pub name: String,
    pub task: String,
}

/// Факты о мире, которых чистая проверка сама знать не может.
#[derive(Debug)]
pub struct Facts {
    pub cli_found: bool,
    pub tmux_ok: bool,
    pub dir_exists: bool,
}

/// Проверенный запуск: дальше идёт уже без «а вдруг».
#[derive(Debug, Clone)]
pub struct Plan {
    pub agent: Agent,
    pub name: String,
    pub task: String,
    pub cwd: String,
    pub model: Option<String>,
}

/// Все отказы `spawn` — в одном месте и словами, которые говорят, что делать.
/// Чистая: ни демона, ни диска, поэтому проверяется тестами целиком.
pub fn preflight(w: &Wanted, f: &Facts) -> Result<Plan, String> {
    let label = w.agent.trim().to_lowercase();
    let agent = Agent::all()
        .iter()
        .copied()
        .find(|a| a.label() == label)
        .ok_or_else(|| {
            format!(
                "не знаю агента «{}» — доступны claude, kimi, codex",
                w.agent.trim()
            )
        })?;

    // Имя обязательно на входе: безымянная сессия заводится за секунду, а имя
    // ей потом проставляет человек руками — ровно та кустарщина, ради которой
    // инструмент и появился.
    let name = one_line(
        &w.name
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>(),
    );
    if name.is_empty() {
        return Err(
            "сессии нужно имя: без него список зарастает автозаголовками. \
             Передай короткое и говорящее, вроде «Сайдбар·JRV·O5»"
                .into(),
        );
    }
    let len = name.chars().count();
    if len > MAX_CHAT_NAME {
        return Err(format!(
            "имя длиннее {MAX_CHAT_NAME} символов ({len}) — сократи: длиннее в строку списка не влезет"
        ));
    }

    // Пустая сессия — это сессия, которую кто-то должен догадаться пнуть.
    let task = w.task.trim().to_string();
    if task.is_empty() {
        return Err(
            "нужен 'task' — текст первого промпта. Сессия поднимается под работу, \
             а пустую потом некому пнуть"
                .into(),
        );
    }

    if let Some(m) = w.model.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        crate::backend::backend(agent).validate_model(m)?;
    }

    let cwd = w.cwd.trim().trim_end_matches('/').to_string();
    if cwd.is_empty() {
        return Err("не указан 'cwd' — рабочий каталог обязателен: сессия без проекта бессмысленна".into());
    }
    if !f.dir_exists {
        return Err(format!(
            "каталога {cwd} нет — создай его или укажи существующий (путь абсолютный)"
        ));
    }

    if !f.cli_found {
        return Err(format!(
            "{} не найден в PATH — поставь его CLI или выбери другого агента (claude, kimi, codex)",
            agent.label()
        ));
    }
    if !f.tmux_ok {
        return Err(
            "tmux недоступен — без него сессия поднимется вне учёта Jarvis: \
             поставь tmux (brew install tmux / apt install tmux) и повтори"
                .into(),
        );
    }

    // Потолка на ЧИСЛО одновременных сессий здесь нет и не будет: сколько нужно
    // задаче, столько и поднимается. Ограничитель один — расход, и он стоит
    // ниже, в `spawn_handler` (`budget_gate` перед самым подъёмом).
    Ok(Plan {
        agent,
        name,
        task,
        cwd,
        model: w.model.as_deref().map(str::trim).filter(|m| !m.is_empty()).map(str::to_string),
    })
}

/* ================= капабилити ================= */

pub fn register(reg: &mut DaemonRegistry) {
    // Класс Control, а не Settings: гейт читает аргументы settings-капабилити
    // как патч конфига и отклонил бы 'agent'/'cwd' по SETTINGS_ALLOWLIST — та же
    // ловушка, что у sessions.rename. Карточка подтверждения при этом остаётся:
    // запуск тратит деньги и плодит процессы. Снимается она только настройкой
    // человека — grants.agent.autoApprove, тем же механизмом, что у
    // sessions.reply; константы «агенту можно» в коде нет.
    reg.register(
        CapabilityMeta {
            id: "sessions.spawn",
            class: RiskClass::Control,
            provenance: Provenance::Trusted,
            description: "Поднять новую сессию CLI-агента (claude/kimi/codex) под задачу и сразу отдать ей первый промпт. \
Зови вместо того, чтобы просить человека открыть терминал. ОБЯЗАТЕЛЬНО задай 'name' — короткое имя чата \
(вроде «Сайдбар·JRV·O5»): безымянную сессию инструмент не создаёт. 'task' уходит первым промптом, \
'parent' — id чата/сессии, откуда поднимаешь (человек должен видеть дерево). 'isolate' поднимает задачу \
в отдельном git-worktree рядом с проектом — бери его, когда сессия будет ПИСАТЬ код. \
Возвращает id СРАЗУ, не дожидаясь ответа сессии: это талон вида 'spawn-…', по нему работают \
sessions.get и sessions.close, а настоящий id сессии появится в нём через несколько секунд.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "enum": ["claude", "kimi", "codex"], "description": "какой CLI поднять" },
                    "name":  { "type": "string", "description": "имя чата, обязательно; до 60 символов" },
                    "cwd":   { "type": "string", "description": "абсолютный путь рабочего каталога (должен существовать)" },
                    "task":  { "type": "string", "description": "первый промпт — что сессии делать" },
                    "parent": { "type": "string", "description": "id чата/сессии, откуда поднимаешь" },
                    "model": { "type": "string", "description": "модель, напр. opus / sonnet (необязательно)" },
                    "mode":  { "type": "string", "enum": ["ask", "plan", "yolo"], "description": "сколько позволено без вопросов" },
                    "isolate": { "type": "boolean", "description": "поднять в отдельном git-worktree" }
                },
                "required": ["agent", "name", "cwd", "task"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move { spawn_handler(d, args).await }),
    );

    reg.register(
        CapabilityMeta {
            id: "sessions.close",
            class: RiskClass::Control,
            provenance: Provenance::Trusted,
            description: "Погасить сессию, которую ты сам поднял через sessions.spawn (по талону 'spawn-…' или по id сессии). \
Чужую сессию и сессию человека закрыть нельзя — придёт отказ. Зови, когда дочерняя работа закончена: \
живая сессия жжёт деньги из общего недельного лимита.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "талон 'spawn-…' или id сессии" }
                },
                "required": ["id"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move { close_handler(d, args).await }),
    );
}

/// Кто зовёт. Гейт инжектит `_consumer` после проверок и всегда перезаписывает —
/// подделать нельзя (см. `gate.rs`, шаг 2б).
fn consumer_of(args: &Value) -> String {
    args.get("_consumer")
        .and_then(Value::as_str)
        .unwrap_or("agent")
        .to_string()
}

async fn spawn_handler(d: Arc<Daemon>, args: Value) -> Result<Value, String> {
    let by = consumer_of(&args);
    let w = Wanted {
        agent: arg_str(&args, "agent")?,
        model: args.get("model").and_then(Value::as_str).map(str::to_string),
        cwd: arg_str(&args, "cwd")?,
        name: arg_str(&args, "name")?,
        task: arg_str(&args, "task")?,
    };
    let parent = args
        .get("parent")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string);

    let now = now_ms();
    let facts = Facts {
        cli_found: crate::backend::backend(Agent::from_label(w.agent.trim()))
            .cli_found(),
        tmux_ok: crate::tmux::reachable().await,
        dir_exists: std::path::Path::new(w.cwd.trim()).is_dir(),
    };
    let plan = preflight(&w, &facts)?;

    // Дорогая работа: параллельная сессия — это новый расход, и числа перед ней
    // обязаны быть свежими, а не пятиминутными из кэша. Сессий может быть
    // сколько угодно — единственная стена здесь эта, и она про деньги: ступень
    // «стоп» (в том числе от ночного потолка расхода) отказывает вот тут.
    //
    // Гейт списывает ОЖИДАЕМУЮ стоимость сразу: расход поднятой сессии доедет до
    // чисел провайдера через минуты, а залп из двадцати запусков случается за
    // секунду. Модель называем — opus стоит впятеро против sonnet, и бронь
    // обязана это знать. Вернуть бронь — ниже, если сессия не поднимется.
    let hold = crate::ipc::budget_reserve(
        &d,
        plan.agent.label(),
        plan.model.as_deref(),
        false,
        "запуск сессии sessions.spawn",
    )
    .await?;

    // Талон заводим ДО запуска: он и есть тот id, который вернётся вызывающему.
    let ticket = d.spawns.open(&by, parent.clone(), &plan, now);
    let bind = Bind {
        ticket: ticket.clone(),
        name: plan.name.clone(),
        model: plan.model.clone(),
        by: by.clone(),
        parent: parent.clone(),
        task: plan.task.clone(),
        at: now,
    };

    let res = crate::ipc::launch_core(
        &d,
        crate::ipc::LaunchReq {
            cwd: Some(plan.cwd.clone()),
            agent: plan.agent.label().to_string(),
            task: Some(plan.task.clone()),
            isolate: args.get("isolate").and_then(Value::as_bool),
            mode: args.get("mode").and_then(Value::as_str).map(str::to_string),
            bind: Some(bind),
            ..Default::default()
        },
    )
    .await;

    if res.get("ok").and_then(Value::as_bool) != Some(true) {
        // Терминал не открылся — талона не за что держать: сессии не будет.
        // И бронь возвращаем тут же: расхода, под который её списали, не
        // случится, а невозвращённая бронь врёт вниз ничуть не лучше, чем
        // отсутствие брони врало вверх.
        d.spawns.give_up(&ticket);
        hold.release();
        return Ok(res);
    }
    hold.in_flight(); // сессия пошла — бронь доживает лизу и уступает место факту
    crate::log::line(&format!(
        "[spawn] {by} поднял «{}» ({}) в {} — талон {ticket}",
        plan.name,
        plan.agent.label(),
        plan.cwd
    ));
    Ok(json!({
        "ok": true,
        "id": ticket,
        "state": "launching",
        "name": plan.name,
        "agent": plan.agent.label(),
        "cwd": plan.cwd,
        "parent": parent,
        "note": "id выдан сразу; сессия появится через несколько секунд — спрашивай sessions.get(id)"
    }))
}

async fn close_handler(d: Arc<Daemon>, args: Value) -> Result<Value, String> {
    let by = consumer_of(&args);
    let id = arg_str(&args, "id")?;
    let (ticket, target) = d.spawns.child(&id, &by)?;
    match target {
        Target::Session(sid) => {
            let res = crate::ipc::kill_core(&d, &sid).await;
            if res.get("ok").and_then(Value::as_bool) == Some(true) {
                d.spawns.mark_closed(&ticket);
            }
            crate::log::line(&format!("[spawn] {by} закрыл дочернюю {sid} (талон {ticket})"));
            Ok(res)
        }
        Target::Pending => {
            d.spawns.give_up(&ticket);
            crate::log::line(&format!("[spawn] {by} снял талон {ticket} — сессия ещё не встала"));
            Ok(json!({ "ok": true, "state": "cancelled",
                       "note": "сессия ещё не появлялась — снят талон запуска" }))
        }
    }
}

/* ================= связывание талона с сессией ================= */

/// Что доделать, когда поднятая сессия наконец появится в реестре.
///
/// Все три вещи — имя, модель, родитель — только ПОСЛЕ первого хука: до него
/// сессии просто нет. Ожидатель у нас уже есть (`ipc::deliver_task` сторожит
/// появление сессии, чтобы отдать первый промпт), поэтому второй сторож не
/// заводится — довешиваемся к нему.
#[derive(Clone, Debug)]
pub struct Bind {
    pub ticket: String,
    pub name: String,
    pub model: Option<String>,
    pub by: String,
    pub parent: Option<String>,
    pub task: String,
    pub at: i64,
}

/// Сессия нашлась: записать родителя, поставить имя, выставить модель.
/// Зовётся из ожидателя запуска ДО отправки первого промпта.
pub async fn on_bound(d: &Arc<Daemon>, b: &Bind, sid: &str) {
    d.spawns.bind(&b.ticket, sid);
    let origin = crate::model::SpawnOrigin {
        by: b.by.clone(),
        parent: b.parent.clone(),
        task: crate::util::ellipsize(&b.task, 160),
        at: b.at,
    };
    d.with_session(sid, |s| s.spawned_by = Some(origin.clone()));
    if let Err(e) = d.rename_chat(sid, &b.name) {
        crate::log::line(&format!("[spawn] имя «{}» не легло на {sid}: {e}", b.name));
    }
    if let Some(m) = &b.model {
        let res = crate::ipc::set_model_core(d, sid, m).await;
        if res.get("ok").and_then(Value::as_bool) != Some(true) {
            crate::log::line(&format!("[spawn] модель «{m}» не встала на {sid}"));
        }
        // Дать пикеру модели закрыться: промпт, вставленный в открытый пикер,
        // уехал бы не в чат, а в поле поиска модели.
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    }
    d.push();
    crate::log::line(&format!("[spawn] талон {} → сессия {sid} («{}»)", b.ticket, b.name));
}

/// Состояние запуска для `sessions.get`, пока настоящей сессии ещё нет.
pub fn pending_json(s: &Spawn) -> Value {
    json!({
        "id": s.ticket,
        "state": "launching",
        "pending": true,
        "name": s.name,
        "agent": s.agent,
        "cwd": s.cwd,
        "parent": s.parent,
        "spawnedBy": s.by,
        "at": s.at,
        "note": "сессия поднимается — хук CLI ещё не пришёл; спроси снова через несколько секунд"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts { cli_found: true, tmux_ok: true, dir_exists: true }
    }

    fn wanted() -> Wanted {
        Wanted {
            agent: "claude".into(),
            model: None,
            cwd: "/tmp/proj".into(),
            name: "Сайдбар·JRV·O5".into(),
            task: "разберись с сайдбаром".into(),
        }
    }

    /// Безымянная сессия — отказ, а не «поднимем, назовёт потом кто-нибудь».
    #[test]
    fn a_nameless_session_is_refused() {
        for empty in ["", "   ", "\n\t"] {
            let w = Wanted { name: empty.into(), ..wanted() };
            let e = preflight(&w, &facts()).unwrap_err();
            assert!(e.contains("имя"), "{e}");
            assert!(e.contains("Сайдбар"), "отказ обязан показать, как выглядит имя: {e}");
        }
        // а нормальное имя проходит и нормализуется в одну строку
        let w = Wanted { name: "  Сайдбар\nJRV  ".into(), ..wanted() };
        assert_eq!(preflight(&w, &facts()).unwrap().name, "Сайдбар JRV");
    }

    #[test]
    fn a_too_long_name_says_how_long() {
        let w = Wanted { name: "и".repeat(MAX_CHAT_NAME + 1), ..wanted() };
        let e = preflight(&w, &facts()).unwrap_err();
        assert!(e.contains(&format!("{MAX_CHAT_NAME}")) && e.contains("61"), "{e}");
    }

    /// Отсутствие CLI названо словами — с именем агента и что делать.
    #[test]
    fn a_missing_cli_is_named() {
        for a in ["claude", "kimi", "codex"] {
            let w = Wanted { agent: a.into(), ..wanted() };
            let e = preflight(&w, &Facts { cli_found: false, ..facts() }).unwrap_err();
            assert!(e.contains(a) && e.contains("PATH"), "{e}");
        }
    }

    #[test]
    fn tmux_and_missing_dir_are_named_too() {
        let e = preflight(&wanted(), &Facts { tmux_ok: false, ..facts() }).unwrap_err();
        assert!(e.contains("tmux") && e.contains("install"), "{e}");
        let e = preflight(&wanted(), &Facts { dir_exists: false, ..facts() }).unwrap_err();
        assert!(e.contains("/tmp/proj"), "отказ обязан назвать каталог: {e}");
    }

    #[test]
    fn unknown_agent_and_empty_task_are_refused() {
        let w = Wanted { agent: "gemini".into(), ..wanted() };
        let e = preflight(&w, &facts()).unwrap_err();
        assert!(e.contains("claude") && e.contains("kimi") && e.contains("codex"), "{e}");
        let w = Wanted { task: "  ".into(), ..wanted() };
        assert!(preflight(&w, &facts()).unwrap_err().contains("task"));
    }

    fn plan() -> Plan {
        Plan {
            agent: Agent::Claude,
            name: "Сайдбар·JRV·O5".into(),
            task: "работай".into(),
            cwd: "/tmp/proj".into(),
            model: None,
        }
    }

    /// Свою дочернюю закрыть можно; чужую и человеческую — нет.
    #[test]
    fn only_own_children_can_be_closed() {
        let s = Spawns::new();
        let mine = s.open("agent", Some("chat-1".into()), &plan(), 0);
        s.bind(&mine, "sid-1");

        // своя — и по талону, и по id сессии
        assert_eq!(s.child(&mine, "agent").unwrap().1, Target::Session("sid-1".into()));
        assert_eq!(s.child("sid-1", "agent").unwrap().1, Target::Session("sid-1".into()));

        // сессия человека в реестре запусков не значится вовсе
        let e = s.child("sid-человека", "agent").unwrap_err();
        assert!(e.contains("не поднималась через sessions.spawn"), "{e}");

        // чужая: поднял другой потребитель
        let theirs = s.open("plugin:x", None, &plan(), 0);
        s.bind(&theirs, "sid-2");
        let e = s.child("sid-2", "agent").unwrap_err();
        assert!(e.contains("plugin:x") && e.contains("только свои"), "{e}");
        // …и наоборот
        assert!(s.child("sid-1", "plugin:x").is_err());
    }

    #[test]
    fn a_ticket_without_a_session_yet_is_cancelled_not_killed() {
        let s = Spawns::new();
        let t = s.open("agent", None, &plan(), 0);
        assert_eq!(s.child(&t, "agent").unwrap().1, Target::Pending);
        s.give_up(&t);
        assert!(s.child(&t, "agent").is_err(), "снятый талон больше не наш");
    }

    #[test]
    fn a_closed_session_is_not_closed_twice() {
        let s = Spawns::new();
        let t = s.open("agent", None, &plan(), 0);
        s.bind(&t, "sid-1");
        s.mark_closed(&t);
        assert!(s.child("sid-1", "agent").unwrap_err().contains("уже закрыта"));
    }

    /// Родитель записан: кто поднял, зачем и когда — иначе человек видит
    /// россыпь окон вместо дерева.
    #[test]
    fn the_parent_is_recorded() {
        let s = Spawns::new();
        let t = s.open("agent", Some("Джарвис·чат-7".into()), &plan(), 1_700_000_000_000);
        let rec = s.find(&t).expect("запись есть сразу");
        assert_eq!(rec.by, "agent");
        assert_eq!(rec.parent.as_deref(), Some("Джарвис·чат-7"));
        assert_eq!(rec.task, "работай");
        assert_eq!(rec.at, 1_700_000_000_000);
        assert_eq!(rec.name, "Сайдбар·JRV·O5");
        // и после связывания находится по обоим идентификаторам
        s.bind(&t, "sid-1");
        assert_eq!(s.find("sid-1").unwrap().parent.as_deref(), Some("Джарвис·чат-7"));
    }

    /// Талон выдаётся ДО того, как появилась сессия, и мгновенно: `spawn` не
    /// имеет права ждать ни хука, ни первого ответа. Ровно за блокирующее
    /// ожидание откатывали `sessions.wait`: пока оно не вернулось, ход агента не
    /// завершён и человеку он недоступен.
    #[tokio::test]
    async fn a_ticket_is_issued_without_waiting_for_the_session() {
        let s = Spawns::new();
        let t0 = std::time::Instant::now();
        // весь путь выдачи id — проверки и талон — под жёстким дедлайном,
        // много меньшим, чем 90 с ожидателя запуска
        let ticket = tokio::time::timeout(std::time::Duration::from_millis(50), async {
            let plan = preflight(&wanted(), &facts()).expect("проверки проходят");
            s.open("agent", Some("chat-1".into()), &plan, now_ms())
        })
        .await
        .expect("выдача id не имеет права блокироваться");
        assert!(t0.elapsed() < std::time::Duration::from_millis(50));
        assert!(ticket.starts_with("spawn-"), "{ticket}");

        // сессии ещё нет — и это нормальное состояние первых секунд; id уже
        // спрашиваем, и он отвечает «поднимается», а не «не найдено»
        let rec = s.find(&ticket).unwrap();
        assert!(rec.session_id.is_none());
        let pending = pending_json(&rec);
        assert_eq!(pending["state"], "launching");
        assert_eq!(pending["pending"], true);
        assert_eq!(pending["name"], "Сайдбар·JRV·O5");
        assert_eq!(pending["parent"], "chat-1");
        assert_ne!(ticket, s.open("agent", None, &plan(), 0), "талоны разные");
    }

    /// Бюджет спрашивается ДО подъёма сессии, а не после: узнать про стену,
    /// когда терминал уже открыт и деньги потрачены, — то же самое, что не
    /// узнать вовсе. Числа отказа проверяются там, где он собирается (`ipc`).
    #[test]
    fn the_budget_is_asked_before_the_session_goes_up() {
        let src = include_str!("spawn.rs");
        let body = src
            .split("async fn spawn_handler")
            .nth(1)
            .and_then(|t| t.split("launch_core").next())
            .expect("хендлер запуска на месте");
        assert!(body.contains("budget_reserve("), "перед запуском бюджет не спрашивается");
        assert!(
            body.find("budget_reserve(").unwrap() > body.find("preflight(").unwrap(),
            "гейт бюджета обязан идти после проверок: агента ещё не знают"
        );
        // Гейт списывает ОЖИДАЕМЫЙ расход вперёд — залп из двадцати запусков
        // иначе читает один и тот же остаток и проходит целиком. Модель ему
        // называют: opus стоит впятеро против sonnet.
        assert!(body.contains("plan.model.as_deref()"), "бронь не знает модель: {body}");
    }

    /// Сессия не поднялась — бронь возвращается тут же. Невозвращённая бронь
    /// врёт вниз ничуть не лучше, чем её отсутствие врало вверх: остановит
    /// работу, которой ничто не мешает.
    #[test]
    fn a_failed_launch_gives_the_reservation_back() {
        let src = include_str!("spawn.rs");
        let tail = src
            .split("async fn spawn_handler")
            .nth(1)
            .and_then(|t| t.split("launch_core").nth(1))
            .expect("хендлер запуска на месте");
        let fail = tail.split("give_up(&ticket);").nth(1).expect("ветка неудачи на месте");
        let fail = fail.split("return Ok(res);").next().unwrap_or_default();
        assert!(fail.contains("hold.release()"), "бронь осталась висеть после неудачи: {fail}");
    }

    /// Потолка на ЧИСЛО одновременных сессий нет ни в одной форме — ни ключом
    /// настроек, ни константой, ни счётом «сколько уже поднято». Решение
    /// владельца: сессий столько, сколько нужно задаче, а сдерживает их расход.
    /// Сторож грепом, потому что потолок легко вернуть «на минуточку» — и он
    /// молча переживёт ревью, спрятавшись за разумно звучащим дефолтом.
    #[test]
    fn no_ceiling_on_the_number_of_sessions_survives_anywhere() {
        // сам сторож называет запретные слова, поэтому себя не читает
        let me = include_str!("spawn.rs");
        let spawn = &me[..me.find("#[cfg(test)]").expect("тесты на месте")];
        // у соседа тоже режем тесты: там запретное слово стоит в утверждении,
        // которое его и запрещает — сторож ловил бы сам себя
        let am = include_str!("../../agent/mod.rs");
        let prompt = &am[..am.find("#[cfg(test)]").expect("тесты агента на месте")];
        // Преамбула агента — тоже источник потолка: фраза про лимит заставила бы
        // Джарвиса отказывать себе самому, ссылаясь на то, чего нет.
        for (what, src) in [
            ("spawn", spawn),
            ("settings", include_str!("../../settings.rs")),
            ("prompt", prompt),
        ] {
            for word in ["sessionsSpawnMax", "DEFAULT_MAX", "max_from_settings", "fn live("] {
                assert!(!src.contains(word), "{what}: потолок сессий вернулся — «{word}»");
            }
        }
        // и отказа «поднято N из M» тоже нет: счёт был нужен только под него
        assert!(!spawn.contains("поднято {} из"), "отказ по числу сессий вернулся");
    }
}
