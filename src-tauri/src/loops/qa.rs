//! Real subprocess regressions; no model/network calls in the default tests.
use super::{engine, model::*, pipeline::*, runner, store::Store};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "jarvis-loops-qa-{tag}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    async fn repo(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::create_dir_all(&p).unwrap();
        let (code, why) = runner::shell(&p, "git init -q -b main && git -c user.name=QA -c user.email=qa@local commit --allow-empty -qm initial", Duration::from_secs(10)).await;
        assert_eq!(code, 0, "{why}");
        p
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn step(id: &str, kind: StepKind, next: &str) -> Step {
    Step {
        id: id.into(),
        name: id.into(),
        kind,
        retries: 0,
        next: if next.is_empty() {
            vec![]
        } else {
            vec![Flow {
                to: next.into(),
                when: Cond::Always,
            }]
        },
    }
}
fn pipeline(repo: &Path, steps: Vec<Step>) -> Loop {
    let mut l = Loop {
        id: "pipeline".into(),
        name: "QA".into(),
        pipeline: Some(Pipeline {
            start: String::new(),
            steps,
        }),
        ..Default::default()
    };
    l.sandbox.repo = repo.to_string_lossy().into_owned();
    l.sandbox.worktree = false;
    l
}
async fn exists(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture process did not start");
}

#[test]
fn published_human_return_resets_streak_once_and_reaches_engine_state() {
    let f = Fixture::new("review");
    let store = Store::load_at(f.0.clone());
    let item = Loop {
        id: "review".into(),
        ..Default::default()
    };
    store.save(item.clone());
    let mut run = engine::initial_run(&item, 1);
    run.streak = 2;
    run.iterations.push(Iteration {
        n: 1,
        verdict: Verdict::Passed,
        ..Default::default()
    });
    store.put_run(run.clone());
    store.with_run("review", |live| {
        live.streak = 0;
        live.iterations[0].reviewed = true;
        live.iterations[0].verdict = Verdict::Returned;
        live.iterations[0].critic = "needs another pass".into();
    });
    engine::publish(&store, &mut run, &|_| {});
    assert_eq!(
        run.streak, 0,
        "the engine must not exit using its old green streak"
    );
    assert_eq!(run.iterations[0].verdict, Verdict::Returned);
    run.streak = 1;
    run.iterations.push(Iteration {
        n: 2,
        verdict: Verdict::Passed,
        ..Default::default()
    });
    engine::publish(&store, &mut run, &|_| {});
    assert_eq!(
        run.streak, 1,
        "historical review must not reset future green iterations forever"
    );
}

#[tokio::test]
async fn manual_stop_cancels_live_shell_and_descendant_before_they_write() {
    let f = Fixture::new("stop");
    let repo = f.repo("repo").await;
    let item = pipeline(
        &repo,
        vec![step(
            "slow",
            StepKind::Shell {
                command: "sh -c 'echo $$ > child.pid; sleep 2; echo leaked > leaked' & wait".into(),
            },
            "",
        )],
    );
    let store = Arc::new(Store::load_at(f.0.join("data")));
    store.save(item.clone());
    let task = tokio::spawn(engine::run_loop(store.clone(), item, 1, |_| {}));
    exists(&repo.join("child.pid")).await;
    store.with_run("pipeline", |run| {
        run.state = RunState::Stopped;
        run.stop = StopReason::Stopped;
    });
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("stop must interrupt current await")
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(!repo.join("leaked").exists(), "descendant outlived stop");
    assert_eq!(store.run("pipeline").unwrap().stop, StopReason::Stopped);
    assert_eq!(
        Store::load_at(f.0.join("data"))
            .run("pipeline")
            .unwrap()
            .stop,
        StopReason::Stopped
    );
}

#[tokio::test]
async fn shell_timeout_kills_descendants() {
    let f = Fixture::new("timeout");
    let (code, _) = runner::shell(
        &f.0,
        "sh -c 'sleep 1; echo leaked > leaked' & wait",
        Duration::from_millis(100),
    )
    .await;
    assert_eq!(code, -1);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(!f.0.join("leaked").exists());
}

#[tokio::test]
async fn answer_after_reload_preserves_history_outputs_and_run_identity() {
    let f = Fixture::new("answer");
    let repo = f.repo("repo").await;
    let item = pipeline(
        &repo,
        vec![
            step(
                "prepare",
                StepKind::Shell {
                    command: "echo run >> count; echo payload".into(),
                },
                "ask",
            ),
            step(
                "ask",
                StepKind::Human {
                    question: "Approve ${prepare.output}?".into(),
                },
                "finish",
            ),
            step(
                "finish",
                StepKind::Shell {
                    command: "printf '%s|%s' '${prepare.output}' '${ask.output}' > answer".into(),
                },
                "",
            ),
        ],
    );
    let root = f.0.join("data");
    let store = Arc::new(Store::load_at(root.clone()));
    store.save(item.clone());
    engine::run_loop(store.clone(), item.clone(), 7, |_| {}).await;
    let before = store.run("pipeline").unwrap();
    assert_eq!(before.state, RunState::Asking);
    assert_eq!(before.ask.as_ref().unwrap().question, "Approve payload?");
    drop(store);
    let store = Arc::new(Store::load_at(root));
    store.with_run("pipeline", |run| {
        run.interventions.push("yes".into());
        run.state = RunState::Running;
        run.tokens = 23;
    });
    engine::run_loop(store.clone(), item, 7, |_| {}).await;
    let after = store.run("pipeline").unwrap();
    assert_eq!(after.state, RunState::Done, "{after:?}");
    assert_eq!(after.n, 7);
    assert_eq!(after.started_at, before.started_at);
    assert_eq!(after.tokens, 23);
    assert_eq!(after.iterations.len(), 2);
    assert_eq!(
        std::fs::read_to_string(repo.join("count")).unwrap(),
        "run\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("answer")).unwrap(),
        "payload|yes"
    );
}

#[tokio::test]
async fn limit_resume_continues_next_step_instead_of_repeating_side_effects() {
    let f = Fixture::new("limit");
    let repo = f.repo("repo").await;
    let mut item = pipeline(
        &repo,
        vec![
            step(
                "one",
                StepKind::Shell {
                    command: "echo once >> count".into(),
                },
                "two",
            ),
            step(
                "two",
                StepKind::Shell {
                    command: "echo finished > done".into(),
                },
                "",
            ),
        ],
    );
    item.limits.iterations = 1;
    let store = Arc::new(Store::load_at(f.0.join("data")));
    store.save(item.clone());
    engine::run_loop(store.clone(), item.clone(), 1, |_| {}).await;
    assert_eq!(store.run("pipeline").unwrap().stop, StopReason::Iterations);
    item.limits.iterations = 5;
    store.with_run("pipeline", |run| {
        run.state = RunState::Running;
        run.stop = StopReason::None;
        run.ended_at = 0;
    });
    engine::run_loop(store.clone(), item, 1, |_| {}).await;
    assert_eq!(store.run("pipeline").unwrap().state, RunState::Done);
    assert_eq!(
        std::fs::read_to_string(repo.join("count")).unwrap(),
        "once\n"
    );
    assert!(repo.join("done").exists());
}

#[tokio::test]
async fn same_name_loops_never_reuse_another_repositories_worktree() {
    let f = Fixture::new("isolation");
    let a = f.repo("a").await;
    let b = f.repo("b").await;
    let mut first = pipeline(&a, vec![]);
    first.sandbox.worktree = true;
    first.id = "a".into();
    let mut second = first.clone();
    second.id = "b".into();
    second.sandbox.repo = b.to_string_lossy().into_owned();
    let (wa, branch) = runner::make_sandbox(&f.0, &first, 1).await.unwrap();
    let (wb, _) = runner::make_sandbox(&f.0, &second, 1).await.unwrap();
    assert_ne!(wa, wb);
    assert!(runner::validate_sandbox(&second, &wa, &branch)
        .await
        .is_err());
    assert_eq!(runner::make_sandbox(&f.0, &first, 1).await.unwrap().0, wa);
}

#[tokio::test]
async fn failed_pipeline_retains_diagnostic_and_never_runs_next_unmatched_step() {
    let f = Fixture::new("failed");
    let repo = f.repo("repo").await;
    let mut broken = step(
        "broken",
        StepKind::Shell {
            command: "echo diagnostic >&2; exit 7".into(),
        },
        "",
    );
    broken.retries = 1;
    let item = pipeline(&repo, vec![broken]);
    let store = Arc::new(Store::load_at(f.0.join("data")));
    store.save(item.clone());
    engine::run_loop(store.clone(), item, 1, |_| {}).await;
    let run = store.run("pipeline").unwrap();
    assert_eq!(run.stop, StopReason::Failed);
    assert_eq!(run.iterations.len(), 1);
    assert!(run.iterations[0].summary.contains("diagnostic"));
}

/// Launched only by scripts/qa/loops-bundle-fixture.py in a separate process
/// with private PATH, JARVIS_DIR and TMUX_TMPDIR. The fake CLI writes no auth.
#[tokio::test]
#[ignore = "requires explicit disposable CLI/tmux fixture"]
async fn isolated_cli_and_tmux_round_trip() {
    let root = PathBuf::from(std::env::var("JARVIS_QA_LOOPS_FIXTURE").expect("run fixture script"));
    let actual = root.canonicalize().unwrap();
    assert_eq!(actual.parent(), Some(Path::new("/private/tmp")));
    assert!(actual
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("jarvis-loops-bundle-qa-"));
    assert_eq!(
        crate::claude_bin::resolve_claude_bin().unwrap(),
        root.join("bin/claude")
    );
    assert_eq!(
        crate::backend::codex::resolve_codex_bin().unwrap(),
        root.join("bin/codex")
    );
    let f = Fixture::new("cli");
    let repo = f.repo("repo").await;
    let store = Arc::new(Store::load_at(f.0.join("data")));
    for agent in ["claude", "codex"] {
        let mut item = Loop {
            id: agent.into(),
            name: agent.into(),
            agent: agent.into(),
            ..Default::default()
        };
        item.sandbox.repo = repo.to_string_lossy().into_owned();
        item.source.goal = "QA benign deterministic edit".into();
        item.exit.critic.enabled = false;
        item.exit.streak = 1;
        item.exit.gates = vec![Gate {
            name: "actual artifact".into(),
            command: "test -f fake-change.txt".into(),
        }];
        store.save(item.clone());
        engine::run_loop(store.clone(), item, 1, |_| {}).await;
        let run = store.run(agent).unwrap();
        assert_eq!(run.state, RunState::Done, "{run:?}");
        assert_eq!(
            run.tokens, 12,
            "actual fake CLI JSON spend must reach journal"
        );
        assert_eq!(run.iterations.len(), 1);
        assert!(Path::new(&run.worktree).join("fake-change.txt").exists());
    }
    let bad = runner::run_agent("claude", &repo, "QA_FAIL", None, Duration::from_secs(5)).await;
    assert!(
        bad.failed && bad.text.contains("deterministic failure"),
        "{bad:?}"
    );
    for provider in ["claude", "codex"] {
        crate::bundle::launch::preflight(&crate::bundle::host::Host::Local, provider, false)
            .await
            .unwrap();
        let command = crate::bundle::launch::hand_command(provider, true).unwrap();
        assert!(command.contains(&root.join("bin").to_string_lossy().to_string()));
        assert!(command.contains(if provider == "codex" {
            "--dangerously-bypass-approvals-and-sandbox"
        } else {
            "--dangerously-skip-permissions"
        }));
    }
    assert!(
        crate::bundle::launch::preflight(&crate::bundle::host::Host::Local, "unknown", false)
            .await
            .is_err()
    );
    let cmd = crate::util::shell_quote(&root.join("bin/fake-tui").to_string_lossy());
    let pane = crate::bundle::launch::spawn(&repo, "qa-safe", &cmd)
        .await
        .unwrap();
    crate::bundle::launch::first_message(&pane, "QA_REPLY_SENTINEL")
        .await
        .unwrap();
    exists(&repo.join("reply.txt")).await;
    assert_eq!(
        std::fs::read_to_string(repo.join("reply.txt"))
            .unwrap()
            .trim(),
        "QA_REPLY_SENTINEL"
    );
    crate::bundle::launch::interrupt(&pane).await;
    exists(&repo.join("interrupted.txt")).await;
    crate::tmux::kill_pane(&pane).await.unwrap();
    assert!(!crate::tmux::pane_alive(&pane).await);
    let start = std::time::Instant::now();
    assert!(crate::bundle::launch::spawn(&repo, "qa-exit", "exit 7")
        .await
        .is_err());
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "dead CLI waited the full readiness timeout"
    );
    std::fs::write(
        root.join("passed.txt"),
        "fake Claude/Codex engine + actual tmux start/reply/interrupt/dead CLI: PASS\n",
    )
    .unwrap();
}
