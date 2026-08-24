//! Гейт безопасности (§7) — единственная точка, через которую проходит каждый
//! вызов любой капабилити, кем бы ни инициирован. Живёт в слое истины, не в
//! транспорте, поэтому необходим всем проекциям (MCP-сервер, in-process).
//!
//! Порядок проверок: реестр (notfound) → грант по классу (+ поимённый denylist)
//! → запрет самоэскалации (класс Settings: security-ключи всем + allowlist для
//! agent/plugin) → подтверждение side-effect, кроме поимённо авто-одобренных
//! пользователем капабилити (без дедлайна: ждём человека) → исполнение
//! (дедлайн 30с) → аудит каждого исхода.
//!
//! Аудит вопроса пишется ДВУМЯ строками: `asked` перед ожиданием и исход после.
//! Одной строки по завершении мало — вопрос, убитый перезапуском демона, не
//! оставлял следа вовсе.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::audit::{AuditEntry, AuditSink};
use super::confirm::Confirmer;
use super::contract::{CallOutput, GateError, RiskClass};
use super::grant::{Consumer, SettingsWrite, SECURITY_KEYS, SETTINGS_ALLOWLIST};
use super::registry::Registry;

/// Дедлайны гейта (R3). Default — боевые; тесты подставляют короткие.
/// Дедлайн тут ровно один — на ИСПОЛНЕНИЕ. Ожидание человека дедлайна не имеет
/// и настройкой не задаётся: поле, которое можно выставить, рано или поздно
/// выставят, а истёкшая карточка — худший из исходов (см. шаг 3).
#[derive(Clone, Copy, Debug)]
pub struct GateConfig {
    pub handler_timeout: Duration,
}

impl Default for GateConfig {
    fn default() -> Self {
        GateConfig {
            handler_timeout: Duration::from_secs(30),
        }
    }
}

/// Имя вопроса: миллисекунды плюс счётчик процесса. Не секрет и не nonce
/// подтверждения (тот одноразовый и живёт в `PendingConfirms`) — только ключ,
/// которым в журнале сходятся «спросили» и «чем кончилось». Время в имени
/// нужно, чтобы строки переживших перезапуск процессов не сливались в одну пару.
fn next_ask_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("{ms:x}-{:x}", SEQ.fetch_add(1, Ordering::Relaxed))
}

/// Принимать ли решение по карточке подтверждения.
///
/// Асимметрия намеренная и в ней весь смысл: признаков присутствия человека
/// требует только СОГЛАСИЕ. Отказ проходит всегда — заперев «Отклонить», мы
/// оставили бы человека наедине с карточкой, которую нечем закрыть, а
/// подброшенный отказ стоит одного хода агента и ничего необратимого не делает.
///
/// `None` (старое окно, не приславшее признак) считается отсутствием признаков:
/// умолчание у проверки безопасности бывает только строгим.
pub fn decision_allowed(approved: bool, armed: Option<bool>) -> bool {
    !approved || armed == Some(true)
}

/// Строка журнала о согласии, у которого нет признаков человека за клавишами
/// (`ipc::agent_confirm`, аргумент `armed`).
///
/// Заводится отдельной записью, а не парой к `asked`, честно: имя вопроса
/// (`ask`) знает гейт, а UI знает только nonce карточки — сшить их нечем.
/// Поэтому nonce лежит в аргументах: по нему видно, к какой карточке относилась
/// попытка, и не выдумывается пара, которой нет.
pub fn unarmed_entry(nonce: &str) -> AuditEntry {
    AuditEntry {
        consumer: "panel".into(),
        id: "capability.confirm".into(),
        class: RiskClass::Admin.as_str(),
        args: serde_json::json!({ "nonce": nonce }),
        provenance: "trusted",
        outcome: "denied:not-armed".into(),
        ms: 0,
        ask: None,
    }
}

/// Записать такую попытку. Тихо отклонить согласие нельзя: для человека это
/// «нажал и ничего», для агента — молчание, а для нас — потерянный след ровно
/// того события, ради которого проверка и заведена.
pub fn note_unarmed(sink: &dyn AuditSink, nonce: &str) {
    sink.record(&unarmed_entry(nonce));
    crate::log::line("[gate] согласие отклонено: нет признаков, что нажимал человек");
}

/// Прогнать вызов капабилити через все проверки и (при успехе) исполнить.
#[allow(clippy::too_many_arguments)]
pub async fn invoke<C>(
    reg: &Registry<C>,
    ctx: C,
    consumer: &Consumer,
    id: &str,
    args: Value,
    confirmer: &dyn Confirmer,
    audit: &dyn AuditSink,
    cfg: GateConfig,
) -> Result<CallOutput, GateError> {
    let t0 = Instant::now();

    let Some(entry) = reg.get(id) else {
        audit.record(&AuditEntry {
            consumer: consumer.id.clone(),
            id: id.to_string(),
            class: "?",
            args,
            provenance: "?",
            outcome: "notfound".into(),
            ms: t0.elapsed().as_millis(),
            ask: None,
        });
        return Err(GateError::NotFound(id.to_string()));
    };
    let meta = &entry.meta;

    // Имя вопроса — только у тех вызовов, где вопрос действительно задавали: им
    // сшиваются строка «спросили» и строка исхода. Отклонённым раньше и
    // авто-одобренным сшивать нечего, и ключа в журнале у них нет.
    let mut ask: Option<String> = None;

    // фабрика записи аудита с уже известными meta. Аргументы снимаем до инъекции
    // _consumer: в аудите потребитель и так пишется отдельным полем.
    let audit_args = args.clone();
    let entry_for = |outcome: String, ms: u128| AuditEntry {
        consumer: consumer.id.clone(),
        id: meta.id.to_string(),
        class: meta.class.as_str(),
        args: audit_args.clone(),
        provenance: meta.provenance.as_str(),
        outcome,
        ms,
        ask: None,
    };

    // 1. Грант по классу (+ поимённый denylist, напр. audit.query агенту).
    if !consumer.grant.allows_id(meta.id, meta.class) {
        audit.record(&entry_for("denied:class".into(), t0.elapsed().as_millis()));
        return Err(GateError::Denied(format!(
            "грант '{}' не разрешает {} ({})",
            consumer.id, meta.id, meta.class.as_str()
        )));
    }

    // 2. Самоэскалация (R7): для класса Settings — security-ключи запрещены ВСЕМ;
    //    agent/plugin (SettingsWrite::Allowlist) — только ключи из allowlist.
    if meta.class == RiskClass::Settings {
        if let Some(key) = touched_key(&args, |k| SECURITY_KEYS.contains(&k)) {
            audit.record(&entry_for("denied:security-key".into(), t0.elapsed().as_millis()));
            return Err(GateError::Denied(format!(
                "ключ '{key}' защищён — меняется только пользователем через UI"
            )));
        }
        if consumer.grant.write == SettingsWrite::Allowlist {
            if let Some(key) = touched_key(&args, |k| !SETTINGS_ALLOWLIST.contains(&k)) {
                audit.record(&entry_for("denied:settings-key".into(), t0.elapsed().as_millis()));
                return Err(GateError::Denied(format!(
                    "ключ '{key}' не в allowlist — агент/плагин не вправе его менять"
                )));
            }
        }
    }

    // 2б. Личность вызывающего для consumer-aware капабилити (entities.publish):
    // ключ служебный, перезаписывается всегда — подделать нельзя. Инъекция после
    // проверки самоэскалации, чтобы _consumer не считался «изменяемым ключом».
    let mut args = args;
    if let Value::Object(ref mut m) = args {
        m.insert("_consumer".into(), Value::String(consumer.id.clone()));
    }

    // 3. Подтверждение side-effect — БЕЗ дедлайна: ждём человека столько, сколько
    //    он идёт к экрану. Дедлайн здесь давал худший из исходов — работа не
    //    сделана, агент получил невнятный отказ, человек не узнал, что спрашивали.
    //    Кто и когда закроет вопрос — забота confirmer'а (он же и отвечает Expired,
    //    если ответа не будет никогда). Молча пропускаем только то, что пользователь
    //    сам внёс в авто-одобрение гранта — поимённо, см. Grant::needs_confirm.
    if consumer.grant.needs_confirm(meta.id, meta.class) {
        ask = Some(next_ask_id());
        let paired = |outcome: String, ms: u128| AuditEntry { ask: ask.clone(), ..entry_for(outcome, ms) };
        // Строка «спросили» — ДО ожидания, а не после. Аудит писался по
        // завершении вызова, и вопрос, убитый перезапуском демона, не оставлял
        // НИ ОДНОЙ строки: агент получал внятное «демон недоступен», человек —
        // ничего. Теперь после перезапуска видно и что спрашивали, и что
        // именно (аргументы тут же), а `asked` без парной строки исхода — это
        // и есть «спросили и не дождались» (`audit::unanswered`).
        audit.record(&paired("asked".into(), t0.elapsed().as_millis()));
        let outcome = confirmer.confirm(meta, &args).await;
        if !outcome.allows() {
            audit.record(&paired(outcome.as_str().into(), t0.elapsed().as_millis()));
            return Err(outcome.gate_error());
        }
    }
    // Дальше исход — вторая строка той же пары, если вопрос задавали.
    let entry_for = |outcome: String, ms: u128| AuditEntry { ask: ask.clone(), ..entry_for(outcome, ms) };

    // 4. Исполнение — с дедлайном (R3, fail-safe liveness; эффект at-least-once).
    //    Дедлайн общий, кроме тех капабилити, что назвали свой при регистрации
    //    (`register_slow`). Общий здесь врал: `sessions.resume` ждёт хука
    //    оживлённой сессии дольше 30 с, и обрубленный вызов отдавал
    //    `failed:timeout` про сессию, которая как раз встала.
    let deadline = entry.deadline.unwrap_or(cfg.handler_timeout);
    match tokio::time::timeout(deadline, (entry.handler)(ctx, args.clone())).await {
        Err(_) => {
            audit.record(&entry_for("failed:timeout".into(), t0.elapsed().as_millis()));
            Err(GateError::Failed("timeout".into()))
        }
        Ok(Ok(value)) => {
            // Хендлер вернул Ok — но это лишь «дошли до конца», а не «сделали».
            // Мягкий отказ ({ok:false}: мёртвая пана, сессия не найдена) обязан
            // лечь в аудит отказом: журнал существует ровно ради вопроса «ты
            // правда отправил?», и «ok» за непроизошедшее — та самая ложь,
            // которую он должен исключать. Значение отдаём нетронутым — форма
            // {needsTmux, resumeCmd} нужна панели.
            audit.record(&entry_for(outcome_of(&value), t0.elapsed().as_millis()));
            Ok(CallOutput { value, provenance: meta.provenance })
        }
        Ok(Err(e)) => {
            audit.record(&entry_for(format!("failed:{e}"), t0.elapsed().as_millis()));
            Err(GateError::Failed(e))
        }
    }
}

/// Исход по значению капабилити. `{ok:false,…}` — мягкий отказ бизнес-логики
/// (панельная форма ответа, см. `ipc::reply_core`), всё прочее — успех.
/// Причину берём из `error`, а для формы `{needsTmux}` называем её сами:
/// строка в журнале должна говорить, почему не сделано.
fn outcome_of(value: &Value) -> String {
    if value.get("ok").and_then(Value::as_bool) != Some(false) {
        return "ok".into();
    }
    let why = value
        .get("error")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            value
                .get("needsTmux")
                .and_then(Value::as_bool)
                .filter(|v| *v)
                .map(|_| "сессия вне tmux".to_string())
        })
        .unwrap_or_else(|| "не выполнено".to_string());
    format!("failed:{why}")
}

/// Первый ключ patch (или корня), удовлетворяющий предикату. Принимаем обе формы:
/// `{patch:{...}}` и `{...}` напрямую.
fn touched_key(args: &Value, pred: impl Fn(&str) -> bool) -> Option<String> {
    let obj = args
        .get("patch")
        .and_then(|p| p.as_object())
        .or_else(|| args.as_object())?;
    obj.keys().find(|k| pred(k.as_str())).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::audit::MemAudit;
    use crate::capability::confirm::AutoApprove;
    use crate::capability::contract::{CapabilityMeta, Provenance};
    use crate::capability::grant::ConfirmPolicy;
    use crate::capability::registry::{make_handler, Registry};
    use serde_json::json;

    /// Согласие без признаков человека не проходит, а отказ проходит всегда.
    /// Дыра была живой: проверяющий CLI слал синтетические клики в окно, где
    /// работал человек, и такой клик способен нажать «Разрешить» — то есть
    /// согласиться за него в том самом гейте, который спрашивает разрешение.
    #[test]
    fn consent_needs_signs_of_a_human_but_refusal_never_does() {
        assert!(decision_allowed(true, Some(true)), "человеческое согласие не прошло");
        assert!(!decision_allowed(true, Some(false)), "согласие прошло без признаков");
        assert!(!decision_allowed(true, None), "умолчание оказалось нестрогим");
        // Заперев отказ, мы оставили бы человека наедине с карточкой.
        for armed in [Some(true), Some(false), None] {
            assert!(decision_allowed(false, armed), "отказ не прошёл при armed={armed:?}");
        }
    }

    /// Отклонённое согласие обязано оставить след: тихий отказ выглядит как
    /// «нажал и ничего» — ровно тот класс вранья, который мы весь день чиним.
    #[test]
    fn a_refused_consent_is_written_down_with_the_card_it_belongs_to() {
        let audit = MemAudit::new();
        note_unarmed(&audit, "nonce-42");
        let e = audit.last().expect("след не записан");
        assert_eq!(e.outcome, "denied:not-armed");
        assert_eq!(e.args["nonce"], "nonce-42", "по строке не найти карточку");
        // Пары с «asked» тут нет и быть не может — UI знает nonce, а не имя
        // вопроса. Выдуманная пара сломала бы поиск неотвеченных вопросов.
        assert!(e.ask.is_none(), "выдумана пара к вопросу");
    }

    /// Реестр с одной Read-капабилити, возвращающей свои args как есть.
    fn echo_registry() -> Registry<()> {
        let mut reg = Registry::new();
        reg.register(
            CapabilityMeta {
                id: "test.echo",
                class: RiskClass::Read,
                provenance: Provenance::Trusted,
                description: "эхо аргументов (тест)",
                input_schema: json!({ "type": "object" }),
            },
            make_handler(|_: (), args| async move { Ok(args) }),
        );
        reg
    }

    #[tokio::test]
    async fn injects_consumer_identity_into_args() {
        let reg = echo_registry();
        let c = Consumer::custom("plugin:test", &[RiskClass::Read], ConfirmPolicy::Never);
        let out = invoke(
            &reg, (), &c, "test.echo", json!({ "x": 1 }),
            &AutoApprove, &MemAudit::new(), GateConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.value["_consumer"], "plugin:test");
        assert_eq!(out.value["x"], 1, "остальные args не тронуты");
    }

    /// Реестр с капабилити, которая «дошла до конца», но ничего не сделала —
    /// ровно как `sessions.reply` в мёртвую пану.
    fn soft_failure_registry(value: Value) -> Registry<()> {
        let mut reg = Registry::new();
        reg.register(
            CapabilityMeta {
                id: "sessions.reply",
                class: RiskClass::Read, // класс тут ни при чём — проверяем аудит
                provenance: Provenance::Trusted,
                description: "мягкий отказ (тест)",
                input_schema: json!({ "type": "object" }),
            },
            make_handler(move |_: (), _args| {
                let value = value.clone();
                async move { Ok(value) }
            }),
        );
        reg
    }

    /// Позже человек спросит «ты правда отправил?» — и единственный артефакт,
    /// существующий ради этого вопроса, обязан ответить честно.
    #[tokio::test]
    async fn a_soft_failure_is_audited_as_a_failure() {
        let reg = soft_failure_registry(json!({ "ok": false, "error": "Сессия не найдена" }));
        let audit = MemAudit::new();
        let c = Consumer::custom("agent", &[RiskClass::Read], ConfirmPolicy::Never);
        let out = invoke(
            &reg, (), &c, "sessions.reply", json!({ "session_id": "s1" }),
            &AutoApprove, &audit, GateConfig::default(),
        )
        .await
        .expect("мягкий отказ — не ошибка гейта: форма ответа нужна панели");
        assert_eq!(out.value["ok"], false, "значение отдано как есть");
        assert_eq!(audit.last().unwrap().outcome, "failed:Сессия не найдена");
    }

    /// Форма {ok:false, needsTmux} причины в себе не несёт — называем её сами.
    #[tokio::test]
    async fn needs_tmux_is_audited_with_a_reason() {
        let reg = soft_failure_registry(json!({ "ok": false, "needsTmux": true, "resumeCmd": "claude --resume x" }));
        let audit = MemAudit::new();
        let c = Consumer::custom("agent", &[RiskClass::Read], ConfirmPolicy::Never);
        invoke(
            &reg, (), &c, "sessions.reply", json!({ "session_id": "s1" }),
            &AutoApprove, &audit, GateConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(audit.last().unwrap().outcome, "failed:сессия вне tmux");
    }

    /// А успех остаётся успехом — в том числе у капабилити без поля `ok`.
    #[tokio::test]
    async fn a_plain_value_is_still_ok() {
        let audit = MemAudit::new();
        let c = Consumer::custom("plugin:test", &[RiskClass::Read], ConfirmPolicy::Never);
        invoke(
            &echo_registry(), (), &c, "test.echo", json!({ "x": 1 }),
            &AutoApprove, &audit, GateConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(audit.last().unwrap().outcome, "ok");
        let reg = soft_failure_registry(json!({ "ok": true, "channel": "tmux" }));
        invoke(
            &reg, (), &c, "sessions.reply", json!({}),
            &AutoApprove, &audit, GateConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(audit.last().unwrap().outcome, "ok");
    }

    /// Confirmer, который ждёт снаружи: пока ответа нет, вызов висит на вопросе —
    /// ровно то состояние, в котором демона и перезапускают.
    struct Waiter(std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<bool>>>);
    impl crate::capability::confirm::Confirmer for Waiter {
        fn confirm<'a>(
            &'a self,
            _m: &'a CapabilityMeta,
            _a: &'a Value,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = super::super::confirm::Outcome> + Send + 'a>>
        {
            // приёмник забираем ДО await: замок через ожидание не тащим
            let rx = self.0.lock().unwrap().take();
            Box::pin(async move {
                match rx {
                    Some(rx) => super::super::confirm::Outcome::decide(rx.await.ok(), true),
                    None => super::super::confirm::Outcome::Expired,
                }
            })
        }
    }

    fn confirmed_registry() -> Registry<()> {
        let mut reg = Registry::new();
        reg.register(
            CapabilityMeta {
                id: "sessions.spawn",
                class: RiskClass::Control,
                provenance: Provenance::Trusted,
                description: "side-effect, спрашивает человека (тест)",
                input_schema: json!({ "type": "object" }),
            },
            make_handler(|_: (), args| async move { Ok(args) }),
        );
        reg
    }

    /// Аудит писался по ЗАВЕРШЕНИИ вызова — и вопрос, убитый перезапуском
    /// демона, не оставлял ни строки: агент получал внятное «демон недоступен»,
    /// человек не получал ничего. Строка «спросили» обязана лечь в журнал ДО
    /// ожидания и пережить смерть ожидания.
    #[tokio::test]
    async fn the_question_is_written_down_before_the_answer_and_outlives_the_wait() {
        let reg = confirmed_registry();
        let audit = MemAudit::new();
        let c = Consumer::agent();
        let (_tx, rx) = tokio::sync::oneshot::channel::<bool>();
        let waiter = Waiter(std::sync::Mutex::new(Some(rx)));
        let args = json!({ "agent": "claude", "name": "Сайдбар", "cwd": "/p", "task": "работай" });

        {
            let fut = invoke(
                &reg, (), &c, "sessions.spawn", args,
                &waiter, &audit, GateConfig::default(),
            );
            tokio::pin!(fut);
            // прокручиваем до места, где вызов повис на вопросе
            tokio::select! {
                _ = &mut fut => panic!("вызов не имел права закончиться: ответа не было"),
                _ = tokio::time::sleep(Duration::from_millis(30)) => {}
            }
            let asked = audit.last().expect("вопрос задан, а в журнале пусто");
            assert_eq!(asked.outcome, "asked");
            assert_eq!(asked.id, "sessions.spawn");
            assert_eq!(asked.args["task"], "работай", "видно, что именно спрашивали");
            assert!(asked.ask.is_some(), "вопросу нужно имя: по нему сходится пара");
            // и вот тут демон умирает — будущее гейта дропается на ожидании
        }
        assert_eq!(audit.len(), 1, "смерть ожидания стёрла строку вопроса");
        assert_eq!(audit.last().unwrap().outcome, "asked", "след остался, и он говорит «спросили»");
    }

    /// Ответ дошёл — в журнале пара: «спросили» и «чем кончилось», сшитые одним
    /// именем. Одна строка без другой ничего не доказывает.
    #[tokio::test]
    async fn an_answered_question_leaves_a_matching_pair() {
        let reg = confirmed_registry();
        let audit = MemAudit::new();
        let out = invoke(
            &reg, (), &Consumer::agent(), "sessions.spawn", json!({ "task": "работай" }),
            &AutoApprove, &audit, GateConfig::default(),
        )
        .await
        .expect("подтверждённый вызов обязан исполниться");
        assert_eq!(out.value["task"], "работай");
        let rows = audit.entries.lock().unwrap().clone();
        assert_eq!(rows.len(), 2, "пара строк: спросили и чем кончилось");
        assert_eq!(rows[0].outcome, "asked");
        assert_eq!(rows[1].outcome, "ok");
        assert_eq!(rows[0].ask, rows[1].ask, "пара не сходится по имени вопроса");
        assert!(rows[0].ask.is_some());
        assert_ne!(next_ask_id(), next_ask_id(), "имена вопросов не повторяются");

        // «спросили и не дождались» отличимо машиной, а не только глазами
        let json_rows: Vec<Value> = rows.iter().map(|r| r.to_json()).collect();
        assert!(crate::capability::audit::unanswered(&json_rows).is_empty(), "вопрос ответили");
        assert_eq!(
            crate::capability::audit::unanswered(&json_rows[..1]).len(),
            1,
            "убитое ожидание обязано находиться"
        );
    }

    /// Вызов, который человека не спрашивает, лишней строки не пишет: `asked` —
    /// про вопрос, а не про каждый вызов.
    #[tokio::test]
    async fn a_call_without_a_question_writes_one_line() {
        let audit = MemAudit::new();
        invoke(
            &echo_registry(), (), &Consumer::custom("plugin:test", &[RiskClass::Read], ConfirmPolicy::Never),
            "test.echo", json!({ "x": 1 }), &AutoApprove, &audit, GateConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit.last().unwrap().outcome, "ok");
        assert!(audit.last().unwrap().ask.is_none(), "вопроса не было — имени тоже");
    }

    /// Капабилити, назвавшая свой дедлайн, живёт по нему, а соседи — по общему.
    ///
    /// Дефект был живым: `sessions.resume` ждёт хука оживлённой сессии дольше
    /// общих тридцати секунд, и гейт обрубал вызов на полпути — агент получал
    /// `failed:timeout` про сессию, которая как раз встала. Своё число обязано
    /// действовать, но только на того, кто его назвал: «поднять общий дедлайн,
    /// раз одному мало» сняло бы защиту со всех остальных.
    #[tokio::test]
    async fn a_capability_may_name_its_own_deadline_and_only_its_own() {
        fn slow_meta(id: &'static str) -> CapabilityMeta {
            CapabilityMeta {
                id,
                class: RiskClass::Read,
                provenance: Provenance::Trusted,
                description: "ждёт дольше общего дедлайна (тест)",
                input_schema: json!({ "type": "object" }),
            }
        }
        let handler = || {
            make_handler(|_: (), _args| async move {
                tokio::time::sleep(Duration::from_millis(60)).await;
                Ok(json!({ "ok": true }))
            })
        };
        let mut reg = Registry::new();
        reg.register_slow(slow_meta("slow.own"), handler(), Duration::from_millis(500));
        reg.register(slow_meta("slow.common"), handler());

        // общий дедлайн заведомо короче того, сколько работает хендлер
        let cfg = GateConfig { handler_timeout: Duration::from_millis(10) };
        let c = Consumer::custom("panel", &[RiskClass::Read], ConfirmPolicy::Never);

        let audit = MemAudit::new();
        let out = invoke(&reg, (), &c, "slow.own", json!({}), &AutoApprove, &audit, cfg)
            .await
            .expect("свой дедлайн не сработал — вызов обрубили общим");
        assert_eq!(out.value["ok"], true);
        assert_eq!(audit.last().unwrap().outcome, "ok");

        // а сосед по реестру по-прежнему под общим: исключение не расползлось
        let err = invoke(&reg, (), &c, "slow.common", json!({}), &AutoApprove, &audit, cfg)
            .await
            .expect_err("общий дедлайн перестал действовать на остальных");
        assert!(matches!(err, GateError::Failed(ref e) if e == "timeout"), "{err:?}");
        assert_eq!(audit.last().unwrap().outcome, "failed:timeout");
    }

    #[tokio::test]
    async fn overwrites_spoofed_consumer() {
        let reg = echo_registry();
        let c = Consumer::custom("plugin:test", &[RiskClass::Read], ConfirmPolicy::Never);
        let out = invoke(
            &reg, (), &c, "test.echo", json!({ "_consumer": "panel" }),
            &AutoApprove, &MemAudit::new(), GateConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.value["_consumer"], "plugin:test", "подделка перезаписана");
    }
}
