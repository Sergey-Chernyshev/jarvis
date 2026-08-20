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

/// Сколько сессий агенту позволено держать одновременно, если в настройках
/// ничего не сказано. Бесконечности быть не должно: каждая сессия — деньги.
pub const DEFAULT_MAX: usize = 4;
/// Ключ настроек с потолком. В `SETTINGS_ALLOWLIST` его НЕТ намеренно: агент,
/// который вправе поднять себе потолок, потолка не имеет.
pub const MAX_KEY: &str = "sessionsSpawnMax";

/// Сколько талон ждёт свою сессию. Больше, чем ожидатель запуска (90 с): талон
/// обязан пережить его, иначе слот освободится раньше, чем станет ясен исход.
const TICKET_TTL_MS: i64 = 3 * 60 * 1000;

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
    /// Хук ещё не пришёл: гасить нечего, снимаем талон (и освобождаем слот).
    Pending,
}

/// Реестр запусков. Отдельная структура, а не поле в сессии: сессия исчезает
/// по session-end, а «кто её поднял» переживает её и нужен для потолка.
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

    /// Агент так и не встал — талон снимаем, слот освобождаем.
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

    /// Сколько сессий этот потребитель держит прямо сейчас. Живость сессии знает
    /// только реестр демона, поэтому спрашиваем его через `alive`.
    pub fn live(&self, by: &str, now: i64, alive: impl Fn(&str) -> bool) -> usize {
        self.list
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.by == by && !s.closed)
            .filter(|s| match &s.session_id {
                Some(sid) => alive(sid),
                // талон без сессии считаем занятым слотом, но не вечно: агент
                // мог не встать вовсе, и тогда слот держать не за что.
                None => now - s.at < TICKET_TTL_MS,
            })
            .count()
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
    /// Сколько сессий уже поднято этим потребителем и сколько ему позволено.
    pub live: usize,
    pub max: usize,
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

    if f.live >= f.max {
        return Err(format!(
            "поднято {} из {} — закрой лишние через sessions.close или подними потолок «{MAX_KEY}» в настройках",
            f.live, f.max
        ));
    }

    Ok(Plan {
        agent,
        name,
        task,
        cwd,
        model: w.model.as_deref().map(str::trim).filter(|m| !m.is_empty()).map(str::to_string),
    })
}

/// Потолок из настроек. Мусор и ноль — дефолт: «ноль сессий» никто не имеет в
/// виду, а тихо запретить запуск целиком хуже, чем взять разумное значение.
pub fn max_from_settings(root: &Value) -> usize {
    root.get(MAX_KEY)
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_MAX)
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
живая сессия жжёт деньги и занимает место под потолком одновременных.",
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
    let max = max_from_settings(&d.settings.load());
    let facts = Facts {
        cli_found: crate::backend::backend(Agent::from_label(w.agent.trim()))
            .cli_found(),
        tmux_ok: crate::tmux::reachable().await,
        dir_exists: std::path::Path::new(w.cwd.trim()).is_dir(),
        live: d.spawns.live(&by, now, |sid| d.session(sid).is_some()),
        max,
    };
    let plan = preflight(&w, &facts)?;

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
        // Терминал не открылся — талон не должен занимать слот до истечения TTL.
        d.spawns.give_up(&ticket);
        return Ok(res);
    }
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
        "limit": { "live": facts.live + 1, "max": max },
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
        Facts { cli_found: true, tmux_ok: true, dir_exists: true, live: 0, max: 4 }
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

    /// Потолок называет числа: «поднято N из N», а не молчит.
    #[test]
    fn the_ceiling_refuses_with_numbers() {
        let f = Facts { live: 3, max: 3, ..facts() };
        let e = preflight(&wanted(), &f).unwrap_err();
        assert!(e.contains("поднято 3 из 3"), "{e}");
        assert!(e.contains("sessions.close"), "отказ обязан сказать, что делать: {e}");
        assert!(e.contains(MAX_KEY), "и где поднять потолок: {e}");
        // на единицу ниже потолка запуск ещё проходит
        assert!(preflight(&wanted(), &Facts { live: 2, max: 3, ..facts() }).is_ok());
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

    /// Учёт потолка: мёртвые сессии слот не держат, протухший талон — тоже.
    #[test]
    fn the_ceiling_counts_only_living_children() {
        let s = Spawns::new();
        let a = s.open("agent", None, &plan(), 0);
        s.bind(&a, "sid-a");
        let b = s.open("agent", None, &plan(), 0);
        s.bind(&b, "sid-b");
        assert_eq!(s.live("agent", 0, |_| true), 2);
        assert_eq!(s.live("agent", 0, |sid| sid == "sid-a"), 1, "мёртвая слот не держит");
        s.mark_closed(&a);
        assert_eq!(s.live("agent", 0, |_| true), 1);
        // чужие запуски в наш потолок не входят
        let c = s.open("plugin:x", None, &plan(), 0);
        s.bind(&c, "sid-c");
        assert_eq!(s.live("agent", 0, |_| true), 1);
        // талон без сессии занимает слот, но не дольше TTL
        let d = s.open("agent", None, &plan(), 0);
        assert_eq!(s.live("agent", 0, |_| true), 2);
        assert_eq!(s.live("agent", TICKET_TTL_MS + 1, |_| true), 1);
        let _ = d;
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

    #[test]
    fn the_ceiling_comes_from_settings_with_a_sane_default() {
        assert_eq!(max_from_settings(&json!({})), DEFAULT_MAX);
        assert_eq!(max_from_settings(&json!({ MAX_KEY: 7 })), 7);
        // ноль и мусор — не «запретить всё молча»
        assert_eq!(max_from_settings(&json!({ MAX_KEY: 0 })), DEFAULT_MAX);
        assert_eq!(max_from_settings(&json!({ MAX_KEY: "много" })), DEFAULT_MAX);
    }
}
