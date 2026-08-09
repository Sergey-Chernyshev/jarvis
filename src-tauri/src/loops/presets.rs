//! Каталог заготовок для конструктора цикла.
//!
//! Команды из головы не вводят: чтобы написать источник задач или гейт, надо
//! ПОМНИТЬ и синтаксис `gh`, и флаги тест-раннера, и как обрезать вывод. Это
//! ровно та когнитивная нагрузка, из-за которой конструктором не пользуются.
//! Каталог переворачивает задачу: человек выбирает по названию и описанию, а
//! команда — деталь, которую он волен подправить после вставки.
//!
//! Заготовка — не пресет-намертво: она вставляется в обычное поле и остаётся
//! редактируемой. Много кастомизации, мало запоминания.

use serde::Serialize;

/// Куда заготовка вставляется.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Slot {
    /// Источник задач: stdout команды становится списком работы итерации.
    Source,
    /// Гейт: нулевой код выхода означает «прошло».
    Gate,
}

/// Одна карточка каталога.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    pub id: &'static str,
    pub slot: Slot,
    /// Раздел каталога: «Rust», «Node», «GitHub»…
    pub category: &'static str,
    pub name: &'static str,
    /// Когда это брать — человеческим языком.
    pub hint: &'static str,
    pub command: &'static str,
}

const fn p(
    id: &'static str,
    slot: Slot,
    category: &'static str,
    name: &'static str,
    hint: &'static str,
    command: &'static str,
) -> Preset {
    Preset { id, slot, category, name, hint, command }
}

/// Весь каталог. Порядок — порядок показа внутри раздела.
pub fn all() -> Vec<Preset> {
    use Slot::{Gate, Source};
    vec![
        // ---------- источники задач: GitHub ----------
        p(
            "src-gh-label",
            Source,
            "GitHub",
            "issue с меткой agent",
            "классика ночного цикла: людям — обсуждение, агенту — метка",
            "gh issue list --label agent --state open --limit 20 --json number,title --jq '.[] | \"#\\(.number) \\(.title)\"'",
        ),
        p(
            "src-gh-issues",
            Source,
            "GitHub",
            "все открытые issue",
            "маленький репозиторий, где агенту можно всё",
            "gh issue list --state open --limit 20 --json number,title --jq '.[] | \"#\\(.number) \\(.title)\"'",
        ),
        p(
            "src-gh-prs",
            Source,
            "GitHub",
            "открытые PR",
            "для цикла-ревьюера: пройтись по каждому и оставить ревью",
            "gh pr list --state open --limit 20 --json number,title --jq '.[] | \"#\\(.number) \\(.title)\"'",
        ),
        p(
            "src-gh-ci-red",
            Source,
            "GitHub",
            "красные прогоны CI",
            "чинить то, что уже сломано: последние упавшие прогоны",
            "gh run list --status failure --limit 10 --json displayTitle,headBranch --jq '.[] | \"\\(.headBranch): \\(.displayTitle)\"'",
        ),
        // ---------- источники задач: тесты ----------
        p(
            "src-cargo-failed",
            Source,
            "Rust",
            "падающие тесты cargo",
            "список красных тестов — по одному на итерацию",
            "cargo test 2>&1 | grep -E '^test .* FAILED' | head -20",
        ),
        p(
            "src-npm-failed",
            Source,
            "Node",
            "падающие тесты npm",
            "хвост прогона: там имена упавших и их ошибки",
            "npm test 2>&1 | tail -40",
        ),
        p(
            "src-pytest-failed",
            Source,
            "Python",
            "падающие тесты pytest",
            "короткая сводка красных без остального шума",
            "pytest -x -q 2>&1 | tail -30",
        ),
        // ---------- источники задач: код ----------
        p(
            "src-todo",
            Source,
            "Код",
            "TODO и FIXME в коде",
            "разгребать помеченное «потом» — идеальная ночная рутина",
            "grep -rn 'TODO\\|FIXME' --include='*.rs' --include='*.ts' --include='*.js' --include='*.py' . | grep -v node_modules | head -30",
        ),
        p(
            "src-clippy-list",
            Source,
            "Rust",
            "предупреждения clippy",
            "по одному предупреждению за итерацию, без спешки",
            "cargo clippy 2>&1 | grep -E '^warning' | head -20",
        ),
        p(
            "src-eslint-list",
            Source,
            "Node",
            "замечания eslint",
            "линт как источник работы: чинить по файлу за шаг",
            "npx eslint . 2>&1 | head -40",
        ),
        // ---------- гейты: Rust ----------
        p("gate-cargo-test", Gate, "Rust", "тесты", "прошли все тесты", "cargo test"),
        p(
            "gate-clippy",
            Gate,
            "Rust",
            "clippy строго",
            "ни одного предупреждения",
            "cargo clippy --all-targets -- -D warnings",
        ),
        p("gate-cargo-fmt", Gate, "Rust", "формат", "rustfmt не видит расхождений", "cargo fmt --check"),
        p("gate-cargo-build", Gate, "Rust", "сборка release", "собирается без ошибок", "cargo build --release"),
        // ---------- гейты: Node ----------
        p("gate-npm-test", Gate, "Node", "тесты", "npm test зелёный", "npm test"),
        p("gate-npm-lint", Gate, "Node", "линт", "eslint без ошибок", "npm run lint"),
        p("gate-tsc", Gate, "Node", "типы", "typescript-компилятор молчит", "npx tsc --noEmit"),
        p("gate-npm-build", Gate, "Node", "сборка", "npm run build проходит", "npm run build"),
        // ---------- гейты: Python ----------
        p("gate-pytest", Gate, "Python", "тесты", "pytest зелёный", "pytest -q"),
        p("gate-ruff", Gate, "Python", "линт", "ruff без замечаний", "ruff check ."),
        p("gate-mypy", Gate, "Python", "типы", "mypy молчит", "mypy ."),
        // ---------- гейты: Go ----------
        p("gate-go-test", Gate, "Go", "тесты", "go test по всем пакетам", "go test ./..."),
        p("gate-go-vet", Gate, "Go", "vet", "go vet без замечаний", "go vet ./..."),
        // ---------- гейты: общие ----------
        p("gate-make-test", Gate, "Общее", "make test", "когда проверки собраны в Makefile", "make test"),
        p(
            "gate-git-clean",
            Gate,
            "Общее",
            "нет мусорных файлов",
            "агент не оставил в дереве неучтённого",
            "test -z \"$(git status --porcelain --untracked-files=all | grep '^??')\"",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_fields_are_filled() {
        let items = all();
        let mut seen = std::collections::HashSet::new();
        for it in &items {
            assert!(seen.insert(it.id), "дубль id: {}", it.id);
            assert!(!it.name.is_empty() && !it.hint.is_empty() && !it.command.is_empty(), "{}", it.id);
            assert!(!it.category.is_empty(), "{}", it.id);
        }
    }

    #[test]
    fn both_slots_are_well_stocked() {
        // «Под все случаи жизни» — это проверяемое обещание, а не оборот речи:
        // каталог, где для слота полторы карточки, не снимает нагрузку.
        let items = all();
        let sources = items.iter().filter(|i| i.slot == Slot::Source).count();
        let gates = items.iter().filter(|i| i.slot == Slot::Gate).count();
        assert!(sources >= 8, "источников маловато: {sources}");
        assert!(gates >= 12, "гейтов маловато: {gates}");
    }

    #[test]
    fn commands_do_not_wait_for_a_human() {
        // Цикл крутится ночью: команда, вставшая с вопросом, повесит итерацию
        // до утра. Интерактивные флаги в каталоге запрещены.
        for it in all() {
            for bad in [" -i ", "--interactive", "less ", "vim ", "nano "] {
                assert!(!it.command.contains(bad), "{}: интерактивная команда", it.id);
            }
        }
    }

    #[test]
    fn gates_look_like_gates_and_sources_like_sources() {
        for it in all() {
            match it.slot {
                // Гейт отвечает кодом выхода; обрезать его вывод головами и
                // хвостами незачем — это признак перепутанного слота.
                Slot::Gate => assert!(!it.command.contains("| head"), "{}", it.id),
                // Источник печатает список: без ограничения длинный вывод
                // раздует промт итерации. Разрешаем и head, и tail, и jq —
                // но хоть что-то ограничивающее у длинных списков быть должно.
                Slot::Source => {
                    let bounded = ["head", "tail", "--limit", "-x"].iter().any(|m| it.command.contains(m));
                    assert!(bounded, "{}: источник без ограничения вывода", it.id);
                }
            }
        }
    }
}
