use super::*;
use serde_json::json;
use std::io::Write;
fn event(at: i64, payload: Value) -> Value {
    json!({"type":"event_msg","timestamp":chrono::DateTime::from_timestamp_millis(at).unwrap().to_rfc3339(),"payload":payload})
}
fn meta() -> Value {
    json!({"type":"session_meta","timestamp":"2026-09-05T00:00:00Z","payload":{"id":"same-sid","cwd":"/fixture","originator":"codex_desktop"}})
}
fn line(v: Value) -> String {
    format!("{v}\n")
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "jarvis-rollout-tests-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn file(&self) -> PathBuf {
        self.0.join("rollout-fixture.jsonl")
    }
    fn append(&self, text: &str) {
        fs::OpenOptions::new()
            .append(true)
            .open(self.file())
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn lifecycle_uses_explicit_provider_boundaries_and_rejects_old_turn_completion() {
    let mut o = Observation::default();
    o.apply(&meta());
    assert_eq!(o.status, Status::Idle);
    o.apply(&event(10, json!({"type":"task_started","turn_id":"new"})));
    o.apply(&event(12, json!({"type":"task_complete","turn_id":"old"})));
    assert_eq!(o.status, Status::Working);
    o.apply(&event(13, json!({"type":"task_complete","turn_id":"new"})));
    assert_eq!(o.status, Status::Done);
    o.apply(&event(11, json!({"type":"task_started","turn_id":"old"})));
    assert_eq!(o.status, Status::Done);
    o.apply(&event(14, json!({"type":"token_count","info":{}})));
    assert_eq!(o.status, Status::Done);
}

#[test]
fn partial_append_and_restart_do_not_lose_or_replay_an_event() {
    let f = Fixture::new();
    fs::write(f.file(), line(meta())).unwrap();
    let mut c = Cursor::default();
    let first = read_increment(&mut c, &f.file()).unwrap();
    assert!(first.2);
    let next = line(event(
        1_800_000_000_000,
        json!({"type":"task_started","turn_id":"turn"}),
    ));
    let split = next.len() / 2;
    f.append(&next[..split]);
    assert!(!read_increment(&mut c, &f.file()).unwrap().1);
    let mut restored: Cursor = serde_json::from_slice(&serde_json::to_vec(&c).unwrap()).unwrap();
    restored.read_offset = restored.committed;
    restored.initialized = false;
    f.append(&next[split..]);
    let result = read_increment(&mut restored, &f.file()).unwrap();
    assert!(result.1 && result.2);
    assert_eq!(restored.observation.status, Status::Working);
    assert_eq!(read_increment(&mut restored, &f.file()).unwrap().0, 0);
    assert!(!read_increment(&mut restored, &f.file()).unwrap().1);
}

#[test]
fn a_large_initial_file_is_silent_until_its_latest_state_is_known() {
    let f = Fixture::new();
    let mut text = line(meta());
    text.push_str(&" ".repeat(READ_BUDGET + 1024));
    text.push('\n');
    text.push_str(&line(event(
        1_800_000_000_000,
        json!({"type":"task_complete","turn_id":"done"}),
    )));
    fs::write(f.file(), text).unwrap();
    let mut c = Cursor::default();
    let first = read_increment(&mut c, &f.file()).unwrap();
    assert!(!first.1 && first.2);
    let second = read_increment(&mut c, &f.file()).unwrap();
    assert!(second.1 && second.2);
    assert_eq!(c.observation.status, Status::Done);
}

#[test]
fn rotation_replaces_old_lifecycle_and_is_bootstrap_not_a_new_notification() {
    let f = Fixture::new();
    fs::write(
        f.file(),
        line(meta()) + &line(event(1_800_000_000_000, json!({"type":"task_started"}))),
    )
    .unwrap();
    let mut c = Cursor::default();
    read_increment(&mut c, &f.file()).unwrap();
    let replacement = f.0.join("replacement");
    fs::write(&replacement, line(meta())).unwrap();
    fs::rename(replacement, f.file()).unwrap();
    let result = read_increment(&mut c, &f.file()).unwrap();
    assert!(result.2);
    assert_eq!(c.observation.status, Status::Idle);
}

#[test]
fn external_questions_clear_only_for_the_matching_provider_call() {
    let mut o = Observation::default();
    o.apply(&meta());
    o.apply(&event(10, json!({"type":"task_started","turn_id":"turn"})));
    o.apply(&json!({"type":"response_item","timestamp":"2026-09-05T01:00:00Z","payload":{
        "type":"function_call","name":"request_user_input","call_id":"call-a","arguments":json!({"questions":[{"id":"q1","header":"Scope","question":"Which scope?","options":[{"label":"One","description":""},{"label":"Two","description":""}]}]}).to_string()}}));
    assert_eq!(o.status, Status::Waiting);
    assert_eq!(o.question.as_ref().unwrap().transport, "external");
    o.apply(&json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"another"}}));
    assert!(o.question.is_some());
    o.apply(&json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"call-a"}}));
    assert!(o.question.is_none());
    assert_eq!(o.status, Status::Working);
}

fn question(at: i64, call: &str, turn: &str) -> Value {
    json!({"type":"response_item","timestamp":chrono::DateTime::from_timestamp_millis(at).unwrap().to_rfc3339(),"payload":{
        "type":"function_call","name":"request_user_input","call_id":call,
        "internal_chat_message_metadata_passthrough":{"turn_id":turn},
        "arguments":json!({"questions":[{"id":"scope","question":"Which scope?","options":[{"label":"Local","description":""},{"label":"VM","description":""}]}]}).to_string()}})
}

#[test]
fn late_questions_and_messages_cannot_reopen_or_rewrite_a_newer_turn() {
    let mut o = Observation::default();
    o.apply(&meta());
    o.apply(&event(100, json!({"type":"task_started","turn_id":"new"})));
    o.apply(&event(
        110,
        json!({"type":"user_message","message":"Current prompt"}),
    ));
    o.apply(&event(
        90,
        json!({"type":"user_message","message":"Late previous prompt"}),
    ));
    o.apply(&question(120, "old-call", "old"));
    assert!(o.question.is_none());
    assert_eq!(o.last_prompt, "Current prompt");
    o.apply(&event(130, json!({"type":"task_complete","turn_id":"new"})));
    o.apply(&question(140, "new-call", "new"));
    assert!(o.question.is_none());
    assert_eq!(o.status, Status::Done);
}

#[test]
fn rollout_question_after_hook_prompt_is_writable_only_with_an_owned_terminal() {
    let mut o = Observation::default();
    o.apply(&meta());
    o.apply(&event(100, json!({"type":"task_started","turn_id":"turn"})));
    o.apply(&question(150, "call", "turn"));
    let mut session = crate::model::Session::new("same-sid".into(), 0);
    session.provider_event_at = Some(110);
    session.provider_turn_id = Some("turn".into());
    session.monitor_source = Some("hook".into());
    session.tmux_pane = Some("%1".into());
    session.control_mode = Some("external".into());
    assert!(apply_observed_state(&mut session, &o));
    assert_eq!(session.question.as_ref().unwrap().transport, "tmux");
    assert_eq!(session.control_mode.as_deref(), Some("tmux"));
    assert_eq!(session.status, Status::Waiting);
    let mut desktop = crate::model::Session::new("desktop".into(), 0);
    assert!(apply_observed_state(&mut desktop, &o));
    assert_eq!(desktop.question.unwrap().transport, "external");
}

#[test]
fn late_metadata_cannot_publish_a_question_older_than_a_hook_completion() {
    let mut o = Observation::default();
    o.apply(&meta());
    o.apply(&event(100, json!({"type":"task_started","turn_id":"turn"})));
    o.apply(&question(120, "call", "turn"));
    o.apply(&event(300, json!({"type":"token_count"})));
    let mut session = crate::model::Session::new("s".into(), 0);
    session.provider_event_at = Some(200);
    session.provider_turn_id = Some("turn".into());
    session.status = Status::Done;
    session.done_at = Some(200);
    assert!(!apply_observed_state(&mut session, &o));
    assert!(session.question.is_none());
    assert_eq!(session.status, Status::Done);
}

#[test]
fn only_a_matching_answers_output_can_acknowledge_a_question() {
    let mut o = Observation::default();
    o.apply(&meta());
    o.apply(&event(100, json!({"type":"task_started","turn_id":"turn"})));
    o.apply(&question(120, "call", "turn"));
    let output = |call: &str, body: Value| json!({"type":"response_item","timestamp":chrono::DateTime::from_timestamp_millis(130).unwrap().to_rfc3339(),"payload":{"type":"function_call_output","call_id":call,"output":body.to_string()}});
    o.apply(&output(
        "different",
        json!({"answers":{"scope":{"answers":["Local"]}}}),
    ));
    assert!(o.question.is_some());
    assert!(o.answered_item.is_none());
    o.apply(&output(
        "call",
        json!({"answers":{"scope":{"answers":["Local"]}}}),
    ));
    assert!(o.question.is_none());
    assert_eq!(o.answered_item.as_deref(), Some("call"));
    assert_eq!(o.state_at, 130);
    o.apply(&question(140, "next", "turn"));
    o.apply(&output("next", json!({"error":"cancelled"})));
    assert!(o.answered_item.is_none());
    assert!(
        o.question.is_some(),
        "older output cannot clear a new request"
    );
}

#[test]
fn changed_question_revision_replaces_only_the_matching_turn_state() {
    let mut o = Observation::default();
    o.apply(&meta());
    o.apply(&event(100, json!({"type":"task_started","turn_id":"turn"})));
    o.apply(&question(120, "call", "turn"));
    let mut s = crate::model::Session::new("s".into(), 0);
    apply_observed_state(&mut s, &o);
    let old = s.question.as_ref().unwrap().revision;
    let mut changed = question(130, "call", "turn");
    let mut args: Value =
        serde_json::from_str(changed["payload"]["arguments"].as_str().unwrap()).unwrap();
    args["questions"][0]["options"][0]["label"] = json!("Changed meaning");
    changed["payload"]["arguments"] = json!(args.to_string());
    o.apply(&changed);
    assert!(apply_observed_state(&mut s, &o));
    assert_ne!(s.question.unwrap().revision, old);
}

fn fork_record(ordinal: u64, kind: &str, payload: Value) -> Value {
    json!({"ordinal":ordinal,"type":kind,"timestamp":"2026-09-05T01:20:40.906Z","payload":payload})
}

#[test]
fn actual_fork_prefix_cannot_replace_owner_or_publish_inherited_lifecycle() {
    // Actual Codex 0.153 fork layout: own header, copied parent header/history,
    // then own records at subagent_history_start_ordinal. Copied timestamps
    // are rewritten to the fork time and cannot identify inherited records.
    let own = fork_record(
        0,
        "session_meta",
        json!({"id":"child","forked_from_id":"parent","subagent_history_start_ordinal":5,
        "timestamp":"2026-09-05T01:20:40.602Z","cwd":"/child-worktree","originator":"Child launcher",
        "source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","depth":1}}}}),
    );
    let copied = fork_record(
        1,
        "session_meta",
        json!({"id":"parent","timestamp":"2026-09-04T21:37:26.679Z","cwd":"/parent","originator":"Parent launcher"}),
    );
    let mut child = Observation::default();
    child.apply(&own);
    child.apply(&copied);
    child.apply(&fork_record(
        2,
        "event_msg",
        json!({"type":"task_started","turn_id":"parent-turn"}),
    ));
    child.apply(&fork_record(
        3,
        "event_msg",
        json!({"type":"user_message","message":"Parent task"}),
    ));
    child.apply(&fork_record(
        4,
        "event_msg",
        json!({"type":"task_complete","turn_id":"parent-turn"}),
    ));
    assert_eq!(child.sid, "child");
    assert_eq!(child.cwd, "/child-worktree");
    assert_eq!(child.originator, "Child launcher");
    assert_eq!(child.created_at, timestamp(&own["payload"]));
    assert_eq!(child.status, Status::Idle);
    assert!(child.title.is_empty());
    assert!(child.turn_id.is_none());
    // Incremental restore must retain the scope counter and original owner.
    let mut child: Observation =
        serde_json::from_value(serde_json::to_value(child).unwrap()).unwrap();
    child.apply(&fork_record(
        5,
        "event_msg",
        json!({"type":"task_started","turn_id":"child-turn"}),
    ));
    child.apply(&fork_record(
        6,
        "event_msg",
        json!({"type":"user_message","message":"Child task"}),
    ));
    child.apply(&fork_record(
        7,
        "event_msg",
        json!({"type":"task_complete","turn_id":"child-turn"}),
    ));
    assert_eq!(child.sid, "child");
    assert_eq!(child.title, "Child task");
    assert_eq!(child.status, Status::Done);
    assert_eq!(child.turn_id.as_deref(), Some("child-turn"));
}

#[test]
fn persisted_unscoped_owner_is_rebuilt_silently_from_the_file_header() {
    let f = Fixture::new();
    let header = fork_record(
        0,
        "session_meta",
        json!({"id":"child","subagent_history_start_ordinal":2,"cwd":"/child"}),
    );
    let parent = fork_record(1, "session_meta", json!({"id":"parent","cwd":"/parent"}));
    fs::write(
        f.file(),
        line(header)
            + &line(parent)
            + &line(fork_record(
                2,
                "event_msg",
                json!({"type":"task_started","turn_id":"child-turn"}),
            )),
    )
    .unwrap();
    let mut old = Cursor::default();
    old.committed = fs::metadata(f.file()).unwrap().len();
    old.observation.sid = "parent".into();
    old.initialized = true;
    fs::write(
        f.0.join("codex-observations.json"),
        serde_json::to_vec(&HashMap::from([(f.file(), old)])).unwrap(),
    )
    .unwrap();
    let mut monitor = Monitor::new(&f.0);
    let cursor = monitor.cursors.get_mut(&f.file()).unwrap();
    assert_eq!(cursor.committed, 0);
    assert!(!cursor.initialized);
    let update = read_increment(cursor, &f.file()).unwrap();
    assert!(update.2, "recovery is bootstrap, not a new notification");
    assert_eq!(cursor.observation.sid, "child");
    assert_eq!(cursor.observation.status, Status::Working);
}

#[test]
fn generated_context_is_cleaned_before_preview_truncation_and_old_titles_are_repaired() {
    let mut o = Observation::default();
    o.apply(&meta());
    o.title = "<recommended_plugins>cached truncated title".into();
    let context=format!("<recommended_plugins>{}</recommended_plugins><environment_context>cwd</environment_context>\nИсправь поиск", "x".repeat(2400));
    o.apply(&event(
        100,
        json!({"type":"user_message","message":context}),
    ));
    assert_eq!(o.title, "Исправь поиск");
    assert_eq!(o.last_prompt, "Исправь поиск");
    o.apply(&event(
        110,
        json!({"type":"user_message","message":"<schema>Real user XML</schema>"}),
    ));
    assert_eq!(o.last_prompt, "<schema>Real user XML</schema>");
    assert_eq!(o.title, "Исправь поиск");
    o.apply(&event(120,json!({"type":"user_message","message":"<environment_context>context only</environment_context>"})));
    assert_eq!(o.last_prompt, "<schema>Real user XML</schema>");
}

#[test]
fn explicit_guardian_metadata_or_model_marks_a_technical_observation_without_chat() {
    let mut guardian = Observation::default();
    guardian.apply(&json!({"type":"session_meta","timestamp":"2026-09-05T00:00:00Z","payload":{"id":"review","source":{"subagent":{"other":"guardian"}},"parent_thread_id":"real-parent"}}));
    guardian.apply(&event(
        100,
        json!({"type":"user_message","message":">>> APPROVAL REQUEST END"}),
    ));
    guardian.apply(&event(
        110,
        json!({"type":"agent_message","message":"{\"risk_level\":\"low\",\"outcome\":\"allow\"}"}),
    ));
    assert!(guardian.technical);
    assert_eq!(guardian.sid, "review");
    assert!(
        guardian.title.is_empty()
            && guardian.last_prompt.is_empty()
            && guardian.last_reply.is_empty()
    );
    let mut model = Observation::default();
    model.apply(&meta());
    model.apply(&event(
        100,
        json!({"type":"user_message","message":"Previously cached review context"}),
    ));
    model.apply(&json!({"type":"turn_context","payload":{"model":"codex-auto-review"}}));
    assert!(model.technical);
    assert!(model.title.is_empty() && model.last_prompt.is_empty());
}

#[test]
fn real_subagents_ignore_inherited_guardian_context_and_keep_user_security_json() {
    let mut o = Observation::default();
    o.apply(&fork_record(0,"session_meta",json!({"id":"real-child","subagent_history_start_ordinal":3,"source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}})));
    o.apply(&fork_record(
        1,
        "session_meta",
        json!({"id":"review","thread_source":"guardian_review"}),
    ));
    o.apply(&fork_record(
        2,
        "turn_context",
        json!({"model":"codex-auto-review"}),
    ));
    let text = "{\"risk_level\":\"low\",\"outcome\":\"allow\"}";
    o.apply(&fork_record(
        3,
        "event_msg",
        json!({"type":"user_message","message":text}),
    ));
    assert!(!o.technical);
    assert_eq!(o.sid, "real-child");
    assert_eq!(o.title, text);
}

#[test]
fn previous_live_cache_revision_is_rebuilt_for_context_normalization() {
    let mut o = Observation::default();
    o.identity_revision = 1;
    assert!(o.needs_scope_rebuild());
    let mut current = Observation::default();
    current.apply(&meta());
    assert!(!current.needs_scope_rebuild());
}

#[test]
fn stale_rollout_repairs_generated_display_text_without_rewinding_hook_state() {
    let mut observed = Observation::default();
    observed.apply(&meta());
    observed.apply(&event(
        100,
        json!({"type":"user_message","message":"Fix the renderer"}),
    ));
    let mut session = crate::model::Session::new("same-sid".into(), 0);
    session.title = Some("<recommended_plugins>cached context".into());
    session.last_prompt = Some("<environment_context>cached context".into());
    session.provider_event_at = Some(200);
    session.status = Status::Done;
    assert!(repair_display_text(&mut session, &observed, None));
    assert_eq!(session.title.as_deref(), Some("Fix the renderer"));
    assert_eq!(session.last_prompt.as_deref(), Some("Fix the renderer"));
    assert_eq!(session.provider_event_at, Some(200));
    assert_eq!(session.status, Status::Done);
    session.title = Some("My renamed task".into());
    session.last_prompt = Some("Newer real prompt".into());
    assert!(!repair_display_text(&mut session, &observed, None));
    assert_eq!(session.title.as_deref(), Some("My renamed task"));
    assert_eq!(session.last_prompt.as_deref(), Some("Newer real prompt"));
}

#[test]
fn native_titles_and_subagent_labels_fill_generated_titles_without_replacing_rename() {
    let mut observed = Observation::default();
    observed.apply(&fork_record(0,"session_meta",json!({"id":"child","agent_nickname":"Ada","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","agent_role":"Parser tests"}}}})));
    assert_eq!(observed.agent_label, "Ada · Parser tests");
    let mut session = crate::model::Session::new("child".into(), 0);
    assert!(repair_display_text(&mut session, &observed, None));
    assert_eq!(session.title.as_deref(), Some("Ada · Parser tests"));
    observed.native_title = "Native task title".into();
    assert!(repair_display_text(&mut session, &observed, None));
    assert_eq!(session.title.as_deref(), Some("Native task title"));
    let previous = observed.clone();
    observed.native_title = "Updated native title".into();
    assert!(repair_display_text(
        &mut session,
        &observed,
        Some(&previous)
    ));
    session.title = Some("My Jarvis rename".into());
    assert!(!repair_display_text(
        &mut session,
        &observed,
        Some(&previous)
    ));
    assert_eq!(session.title.as_deref(), Some("My Jarvis rename"));
}
