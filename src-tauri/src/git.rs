//! Чтение git-метаданных каталога без спавна git-процесса (issue #24).
//! Нужен как фоллбэк ветки сессии: Claude пишет gitBranch в транскрипт,
//! в rollout Codex ветки нет вовсе.

use std::fs;
use std::path::{Path, PathBuf};

/// Текущая ветка репозитория, которому принадлежит `dir` (cwd сессии может
/// быть подкаталогом — поднимаемся к корню как `git rev-parse`).
/// Detached HEAD и отсутствие репозитория → None.
pub fn branch_of(dir: &Path) -> Option<String> {
    dir.ancestors().find_map(branch_at)
}

/// Ветка из `.git/HEAD` ровно этого каталога. Поддержан worktree:
/// `.git` — файл `gitdir: <путь>` (относительный — от `dir`), один уровень.
fn branch_at(dir: &Path) -> Option<String> {
    let dot_git = dir.join(".git");
    let head_path = if dot_git.is_dir() {
        dot_git.join("HEAD")
    } else {
        let text = fs::read_to_string(&dot_git).ok()?;
        let gitdir = text.strip_prefix("gitdir:")?.trim();
        let mut p = PathBuf::from(gitdir);
        if p.is_relative() {
            p = dir.join(p);
        }
        p.join("HEAD")
    };
    let head = fs::read_to_string(head_path).ok()?;
    let branch = head.trim().strip_prefix("ref: refs/heads/")?.trim();
    (!branch.is_empty()).then(|| branch.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("jarvis-git-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_head(repo: &Path, content: &str) {
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".git/HEAD"), content).unwrap();
    }

    #[test]
    fn reads_regular_branch() {
        let repo = temp_dir("plain");
        write_head(&repo, "ref: refs/heads/master\n");
        assert_eq!(branch_of(&repo).as_deref(), Some("master"));
    }

    #[test]
    fn branch_with_slashes_kept_whole() {
        let repo = temp_dir("slashes");
        write_head(&repo, "ref: refs/heads/feat/branch-for-codex-sessions\n");
        assert_eq!(
            branch_of(&repo).as_deref(),
            Some("feat/branch-for-codex-sessions")
        );
    }

    #[test]
    fn detached_head_is_none() {
        let repo = temp_dir("detached");
        write_head(&repo, "3aabab6d0aafddf404aa650e5ec2d86e22f4acf7\n");
        assert_eq!(branch_of(&repo), None);
    }

    #[test]
    fn missing_repo_is_none() {
        let dir = temp_dir("norepo");
        assert_eq!(branch_of(&dir), None);
    }

    #[test]
    fn walks_up_from_subdirectory() {
        let repo = temp_dir("walkup");
        write_head(&repo, "ref: refs/heads/develop\n");
        let sub = repo.join("src/deep");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(branch_of(&sub).as_deref(), Some("develop"));
    }

    #[test]
    fn resolves_worktree_gitdir_file() {
        let base = temp_dir("wt");
        // «основной» репозиторий с каталогом worktrees/wt1
        let main_gitdir = base.join("main/.git/worktrees/wt1");
        fs::create_dir_all(&main_gitdir).unwrap();
        fs::write(main_gitdir.join("HEAD"), "ref: refs/heads/feat/wt-branch\n").unwrap();
        // сам worktree: .git — файл с абсолютным gitdir
        let wt = base.join("checkout");
        fs::create_dir_all(&wt).unwrap();
        fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main_gitdir.display()),
        )
        .unwrap();
        assert_eq!(branch_of(&wt).as_deref(), Some("feat/wt-branch"));
    }

    #[test]
    fn resolves_relative_gitdir_file() {
        let base = temp_dir("wt-rel");
        let gitdir = base.join("gitmeta");
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(gitdir.join("HEAD"), "ref: refs/heads/rel-branch\n").unwrap();
        let wt = base.join("checkout");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), "gitdir: ../gitmeta\n").unwrap();
        assert_eq!(branch_of(&wt).as_deref(), Some("rel-branch"));
    }
}
