//! Библиотека шаблонов — то, с чего начинается первый цикл.
//!
//! Шаблон это ЗАГОТОВКА: шаги и ограничители всё равно правит человек. Смысл в
//! другом — чистый лист не подсказывает, что у цикла вообще бывает условие
//! выхода, и первый цикл без шаблона выходит без стен.

use super::model::*;

/// Шаблон в списке библиотеки.
pub struct Template {
    pub id: &'static str,
    pub name: &'static str,
    /// Подпись строкой — расписание и условие выхода одним взглядом.
    pub hint: &'static str,
    pub build: fn() -> Loop,
}

pub fn all() -> Vec<Template> {
    vec![
        Template {
            id: "test-fix",
            name: "ночной test-fix",
            hint: "02:00 · до 20 итераций · выход: гейты зелёные",
            build: test_fix,
        },
        Template {
            id: "triage",
            name: "утренний триаж",
            hint: "07:00 · до 5 итераций · выход: inbox пуст",
            build: triage,
        },
        Template {
            id: "pr-review",
            name: "авто-ревью PR",
            hint: "по событию PR · выход: ревью оставлено",
            build: pr_review,
        },
        Template {
            id: "docs",
            name: "догфидинг доков",
            hint: "раз в неделю · выход: все шаги пройдены",
            build: docs,
        },
        Template {
            id: "parallel",
            name: "параллельная правка",
            hint: "две ветки одновременно · каждая в своём worktree · слияние в конце",
            build: parallel,
        },
    ]
}

pub fn build(id: &str) -> Option<Loop> {
    all().into_iter().find(|t| t.id == id).map(|t| (t.build)())
}

/// Пайплайн с настоящей параллелью — первый, который стоит увидеть целиком.
///
/// Чистый лист не подсказывает главного: что у веток бывают РАЗНЫЕ рабочие
/// деревья и что их надо сводить обратно. Собранное руками ветвление без
/// слияния — самая частая ошибка, и стоит она ночи работы, которая никуда не
/// вольётся.
fn parallel() -> Loop {
    use super::pipeline::{Cond, Flow, Pipeline, Step, StepKind};
    let node = Step::node;
    let mut l = base("параллельная правка");
    l.limits.iterations = 30;
    l.pipeline = Some(Pipeline {
        start: "план".into(),
        steps: vec![
            node(
                "план",
                StepKind::Agent {
                    prompt: "Разбей задачу на две независимые части и опиши каждую. Файлов не трогай.".into(),
                    model: String::new(),
                },
                vec![Flow::to("разойтись", Cond::Always)],
            ),
            node(
                "разойтись",
                StepKind::Fork,
                vec![Flow::to("первая", Cond::Always), Flow::to("вторая", Cond::Always)],
            ),
            node(
                "первая",
                StepKind::Agent {
                    prompt: "Сделай ПЕРВУЮ часть плана: ${план.вывод}. Второй не касайся.".into(),
                    model: String::new(),
                },
                vec![Flow::to("свести", Cond::Always)],
            ),
            node(
                "вторая",
                StepKind::Agent {
                    prompt: "Сделай ВТОРУЮ часть плана: ${план.вывод}. Первой не касайся.".into(),
                    model: String::new(),
                },
                vec![Flow::to("свести", Cond::Always)],
            ),
            {
                // Конфликт отдаём агенту: он видел контекст своих правок, мы нет.
                let mut j = node("свести", StepKind::Join, vec![Flow::to("слилось", Cond::Always)]);
                j.on_conflict = "agent".into();
                j
            },
            // Выбор после слияния — отдельной развилкой, а не условиями на
            // выходах шлюза: параллельный шлюз в BPMN не выбирает, и схема с
            // условиями на его стрелках читалась бы в модельере как ещё одно
            // ветвление.
            node(
                "слилось",
                StepKind::Choice,
                vec![Flow::to("тесты", Cond::Ok), Flow::to("", Cond::Always)],
            ),
            node(
                "тесты",
                StepKind::Shell { command: "cargo test".into() },
                vec![Flow::to("починить", Cond::Fail), Flow::to("", Cond::Always)],
            ),
            node(
                "починить",
                StepKind::Agent {
                    prompt: "После слияния веток тесты красные:\n${тесты.вывод}\nПочини причину, а не симптом.".into(),
                    model: String::new(),
                },
                vec![Flow::to("тесты", Cond::Always)],
            ),
        ],
        ..Default::default()
    });
    l
}

fn base(name: &str) -> Loop {
    Loop {
        name: name.into(),
        agent: "claude".into(),
        created_at: crate::util::now_ms(),
        ..Default::default()
    }
}

fn test_fix() -> Loop {
    let mut l = base("ночной test-fix");
    l.source = Source {
        goal: "чинить флаки-тесты, которые падают в CI".into(),
        command: "gh issue list --label agent --json title,number --jq '.[] | \"#\\(.number) \\(.title)\"'".into(),
    };
    l.exit.gates = vec![
        Gate { name: "тесты".into(), command: "cargo test".into() },
        Gate { name: "clippy".into(), command: "cargo clippy -- -D warnings".into() },
    ];
    l.schedule.wake = Wake::Daily { at: "02:00".into() };
    l
}

fn triage() -> Loop {
    let mut l = base("утренний триаж");
    l.source = Source {
        goal: "разобрать входящие issue: метка, приоритет, ответ автору".into(),
        command: "gh issue list --search 'no:label' --json number,title --jq '.[] | \"#\\(.number) \\(.title)\"'".into(),
    };
    // У триажа нет детерминированного гейта — работа не компилируется.
    // Условие выхода несёт критик, и потому он тут обязателен.
    l.exit.gates = Vec::new();
    l.exit.streak = 1;
    l.limits.iterations = 5;
    l.schedule.wake = Wake::Daily { at: "07:00".into() };
    l
}

fn pr_review() -> Loop {
    let mut l = base("авто-ревью PR");
    l.source = Source {
        goal: "оставить ревью на открытых PR: замечания по существу, без придирок к стилю".into(),
        command: "gh pr list --json number,title --jq '.[] | \"#\\(.number) \\(.title)\"'".into(),
    };
    l.exit.gates = Vec::new();
    l.exit.streak = 1;
    l.limits.iterations = 10;
    l.schedule.wake = Wake::Every { minutes: 60 };
    l
}

fn docs() -> Loop {
    let mut l = base("догфидинг доков");
    l.source = Source {
        goal: "пройти документацию как новый пользователь и починить то, что не сходится".into(),
        command: String::new(),
    };
    l.exit.gates = vec![Gate { name: "ссылки".into(), command: "npm run docs:check".into() }];
    l.limits.iterations = 8;
    l.schedule.wake = Wake::Every { minutes: 7 * 24 * 60 };
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_is_launchable_once_a_repo_is_picked() {
        for t in all() {
            let mut l = (t.build)();
            // Репозиторий шаблон знать не может — его выбирает человек.
            assert_eq!(l.problems(), vec!["не указан репозиторий".to_string()], "{}", t.id);
            l.sandbox.repo = "/repo".into();
            assert!(l.problems().is_empty(), "{} → {:?}", t.id, l.problems());
        }
    }

    /// Шаблон параллели существует ровно затем, чтобы показать полную форму:
    /// ветвление, ветки и — обязательно — слияние. Без слияния он учил бы
    /// ошибке, которая стоит ночи работы.
    #[test]
    fn the_parallel_template_shows_the_whole_shape() {
        use super::super::pipeline::StepKind;
        let l = build("parallel").unwrap();
        let p = l.pipeline.as_ref().expect("шаблон параллели без пайплайна");
        assert!(p.has_parallel());
        let fork = p.steps.iter().find(|s| matches!(s.kind, StepKind::Fork)).unwrap();
        assert_eq!(p.join_after(&fork.id).as_deref(), Some("свести"), "ветви никуда не сходятся");
        assert_eq!(p.join_arity("свести"), 2);
        // Правило конфликта — половина смысла шаблона: без него первая же
        // пара веток, тронувших один файл, остановит прогон до утра.
        assert_eq!(p.step("свести").unwrap().on_conflict, "agent");
        // И отдельное дерево у него включено: иначе запустить его нельзя.
        assert!(l.sandbox.worktree);
    }

    #[test]
    fn templates_without_gates_lean_on_the_critic() {
        // У триажа и ревью нет команды, которая скажет «сделано». Значит
        // условие выхода держится на критике — и без него шаблон нельзя было
        // бы запустить вообще.
        for id in ["triage", "pr-review"] {
            let l = build(id).unwrap();
            assert!(l.exit.gates.is_empty(), "{id}");
            assert!(l.exit.critic.enabled, "{id}: без гейтов критик обязателен");
        }
    }

    #[test]
    fn every_template_has_walls() {
        for t in all() {
            let l = (t.build)();
            let has_wall =
                l.limits.tokens > 0 || l.limits.iterations > 0 || l.limits.minutes > 0;
            assert!(has_wall, "{}: шаблон без ограничителей", t.id);
        }
    }

    #[test]
    fn unknown_template_is_not_invented() {
        assert!(build("нет такого").is_none());
    }
}
