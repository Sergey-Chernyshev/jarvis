//! Read-only repository evidence; text matches are explicitly not authorship proof.
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MAX_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_UNTRACKED_FILE: usize = 1024 * 1024;
const MAX_PATCH_PATH_BYTES: usize = 64 * 1024;

fn command(cwd: &Path, program: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    command_with_limits(cwd, program, args, Duration::from_secs(4), MAX_OUTPUT)
}

fn command_with_limits(
    cwd: &Path,
    program: &str,
    args: &[&str],
    timeout: Duration,
    max_output: usize,
) -> Result<Vec<u8>, String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    let result = (|| {
        let mut stdout = child.stdout.take().ok_or("Нет stdout")?;
        let fd = stdout.as_raw_fd();
        // The pipe remains owned by stdout for the entire polling loop.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        let mut output = Vec::new();
        let mut buffer = [0u8; 16 * 1024];
        let mut eof = false;
        let mut status = None;
        loop {
            if start.elapsed() >= timeout {
                return Err(format!(
                    "Git: превышено время ожидания ({} мс)",
                    timeout.as_millis()
                ));
            }
            // Limit each iteration as well as the total so a continuously writing
            // process cannot prevent deadline/status checks.
            if !eof {
                match stdout.read(&mut buffer) {
                    Ok(0) => eof = true,
                    Ok(count) => {
                        if count > max_output.saturating_sub(output.len()) {
                            return Err(format!(
                                "Git: вывод превышает {max_output} байт, статистика неполная"
                            ));
                        }
                        output.extend_from_slice(&buffer[..count]);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => (),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.to_string()),
                }
            }
            if status.is_none() {
                status = child.try_wait().map_err(|e| e.to_string())?;
            }
            if eof {
                if let Some(status) = status {
                    return if status.success() {
                        Ok(output)
                    } else {
                        Err("Команда Git недоступна или завершилась ошибкой".into())
                    };
                }
            }
            // A direct child may exit while descendants still hold stdout open.
            // EOF and process completion share this same deadline.
            std::thread::sleep(
                Duration::from_millis(1).min(timeout.saturating_sub(start.elapsed())),
            );
        }
    })();
    if result.is_err() {
        // process_group(0) gives this invocation its own group. Kill descendants
        // too, including those retaining the pipe after the direct child exits.
        unsafe {
            libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
        }
        let _ = child.kill();
        // Reap without making the caller wait indefinitely on process cleanup.
        if !matches!(child.try_wait(), Ok(Some(_))) {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
    result
}

fn git(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut all = vec![
        "--literal-pathspecs",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.quotePath=false",
        "-c",
        "core.hooksPath=/dev/null",
    ];
    all.extend_from_slice(args);
    command(cwd, "git", &all)
}

pub fn root(cwd: &str) -> Option<String> {
    let path = Path::new(cwd);
    if !path.is_absolute() {
        return None;
    }
    let bytes = git(path, &["rev-parse", "--show-toplevel"]).ok()?;
    let value = String::from_utf8(bytes).ok()?;
    Some(value.trim_end_matches('\n').to_string())
}

fn source_file(path: &str) -> bool {
    if path.split('/').any(|p| {
        matches!(
            p,
            "node_modules" | "vendor" | "target" | "dist" | "build" | ".git"
        )
    }) {
        return false;
    }
    let name = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if name.contains(".min.") || name.contains(".generated.") || name.ends_with(".lock") {
        return false;
    }
    matches!(
        Path::new(path)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or(""),
        "rs" | "js"
            | "mjs"
            | "cjs"
            | "jsx"
            | "ts"
            | "tsx"
            | "py"
            | "go"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "cc"
            | "java"
            | "kt"
            | "kts"
            | "swift"
            | "m"
            | "mm"
            | "cs"
            | "fs"
            | "rb"
            | "php"
            | "scala"
            | "sh"
            | "bash"
            | "zsh"
            | "sql"
            | "html"
            | "css"
            | "scss"
            | "sass"
            | "vue"
            | "svelte"
            | "ex"
            | "exs"
            | "erl"
            | "hrl"
            | "clj"
            | "cljs"
            | "dart"
            | "lua"
            | "pl"
            | "r"
            | "R"
            | "jl"
            | "zig"
            | "nix"
            | "tf"
    )
}

fn source_file_config(path: &str, config: &Value) -> bool {
    if path.split('/').any(|p| p == ".git") {
        return false;
    }
    let directories = config
        .pointer("/git/excludeDirectories")
        .and_then(Value::as_array);
    let default_dirs = ["node_modules", "vendor", "target", "dist", "build", ".git"];
    if path.split('/').any(|part| {
        directories.map_or_else(
            || default_dirs.contains(&part),
            |dirs| dirs.iter().any(|dir| dir.as_str() == Some(part)),
        )
    }) {
        return false;
    }
    let name = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let suffixes = config
        .pointer("/git/excludeSuffixes")
        .and_then(Value::as_array);
    if suffixes.map_or_else(
        || name.ends_with(".lock"),
        |values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .any(|suffix| name.ends_with(suffix))
        },
    ) {
        return false;
    }
    let fragments = config
        .pointer("/git/excludeNameFragments")
        .and_then(Value::as_array);
    if fragments.map_or_else(
        || name.contains(".min.") || name.contains(".generated."),
        |values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .any(|fragment| name.contains(fragment))
        },
    ) {
        return false;
    }
    let extension = Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    config
        .pointer("/git/sourceExtensions")
        .and_then(Value::as_array)
        .map_or_else(
            || source_file(path),
            |values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|ext| ext.trim_start_matches('.') == extension)
            },
        )
}

fn supported_source_path(path: &str, config: &Value) -> bool {
    // Such names are quoted/escaped in patch headers; skip them consistently in
    // the denominator instead of treating their escaped spelling as a real path.
    !path
        .chars()
        .any(|c| c.is_control() || matches!(c, '"' | '\\'))
        && source_file_config(path, config)
}

fn read_bounded(reader: impl Read, limit: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    let limited = bytes.len() > limit;
    Ok((bytes, limited))
}

fn evidence_lines(
    root: &Path,
    evidence: &[Value],
    config: &Value,
) -> HashMap<String, HashMap<String, usize>> {
    let mut by_file: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for e in evidence {
        let Some(path) = e["path"].as_str() else {
            continue;
        };
        let path = Path::new(path);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            Path::new(e["cwd"].as_str().unwrap_or("")).join(path)
        };
        // No traversal out of this repo, and aliases/symlinks resolve consistently.
        let Ok(absolute) = absolute.canonicalize() else {
            continue;
        };
        let Ok(relative) = absolute.strip_prefix(root) else {
            continue;
        };
        let relative = relative.to_string_lossy().to_string();
        if !source_file_config(&relative, config) {
            continue;
        }
        let Some(lines) = e["addedLines"].as_array() else {
            continue;
        };
        let mut occurrence = HashMap::new();
        for line in lines.iter().filter_map(Value::as_str) {
            // Braces and empty lines are especially ambiguous matches.
            if line.trim().chars().count()
                < config
                    .pointer("/git/minMatchChars")
                    .and_then(Value::as_u64)
                    .unwrap_or(12)
                    .clamp(1, 1000) as usize
            {
                continue;
            }
            *occurrence
                .entry(line.trim_end_matches('\r').to_string())
                .or_insert(0usize) += 1;
        }
        let counts = by_file.entry(relative).or_default();
        for (line, count) in occurrence {
            counts
                .entry(line)
                .and_modify(|n| *n = (*n).max(count))
                .or_insert(count);
        }
    }
    by_file
}

fn added_from_patch(text: &str) -> BTreeMap<String, Vec<String>> {
    added_from_patch_config(text, &Value::Null)
}

fn added_from_patch_config(text: &str, config: &Value) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let mut current = None;
    let mut in_hunk = false;
    for line in text.lines() {
        if line.starts_with("diff --git ") {
            current = None;
            in_hunk = false;
        }
        if let Some(path) = line.strip_prefix("+++ b/") {
            // Git adds a delimiter tab to unquoted paths containing spaces.
            let path = path.strip_suffix('\t').unwrap_or(path);
            current = supported_source_path(path, config).then(|| path.to_string());
            in_hunk = false;
        } else if line.starts_with("@@ ") {
            in_hunk = true;
        } else if in_hunk {
            if let (Some(path), Some(added)) = (current.as_ref(), line.strip_prefix('+')) {
                out.entry(path.clone())
                    .or_insert_with(Vec::new)
                    .push(added.to_string());
            }
        }
    }
    out
}

fn git_ai(root: &Path) -> Value {
    // Invoke only an installed binary. Do not install hooks or change Git config.
    let installed = std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| p.join("git-ai").is_file()));
    if !installed {
        return json!({"available":false,"scope":"HEAD","caveat":"Git AI не установлен. Ручное авторство по обычному Git определить нельзя."});
    }
    let result = command(root, "git-ai", &["stats", "HEAD", "--json"])
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string()));
    match result {
        Ok(v) => git_ai_value(&v),
        Err(e) => json!({"available":false,"scope":"HEAD","error":e}),
    }
}

fn git_ai_value(v: &Value) -> Value {
    let counts = [
        "ai_additions",
        "human_additions",
        "unknown_additions",
        "git_diff_added_lines",
    ]
    .map(|key| v[key].as_u64());
    let [Some(ai), Some(human), Some(unknown), Some(total)] = counts else {
        return json!({"available":false,"scope":"HEAD","error":"Версия Git AI не сообщает unknown_additions; точная доля ручного кода недоступна."});
    };
    if ai.checked_add(human).and_then(|n| n.checked_add(unknown)) != Some(total) {
        return json!({"available":false,"scope":"HEAD","error":"Несогласованные знаменатели Git AI"});
    }
    let pct = |n: u64| (total > 0).then_some(n as f64 * 100.0 / total as f64);
    json!({"available":true,"scope":"HEAD","addedLines":total,"aiLines":ai,"humanLines":human,"unknownLines":unknown,
        "aiPct":pct(ai),"humanPct":pct(human),"unknownPct":pct(unknown),"coveragePct":pct(ai+human),
        "caveat":"Авторство добавленных строк последнего коммита по Git AI. Его фильтры файлов отличаются от текущего diff; полнота зависит от захвата редактора и агента."})
}

pub fn inspect(path: &str, evidence: &[Value]) -> Value {
    inspect_with_config(path, evidence, &Value::Null)
}

pub fn inspect_with_config(path: &str, evidence: &[Value], config: &Value) -> Value {
    match inspect_inner(path, evidence, config) {
        Ok(value) => value,
        Err(e) => json!({"ok":false,"error":e}),
    }
}

fn inspect_inner(path: &str, evidence: &[Value], config: &Value) -> Result<Value, String> {
    let root = PathBuf::from(path)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let head = git(&root, &["rev-parse", "--verify", "HEAD"])
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_string());
    let branch = git(&root, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_string());
    // For an unborn repository the empty tree is a read-only base.
    let base = head
        .as_deref()
        .unwrap_or("4b825dc642cb6eb9a060e54bf8d69288fbee4904");
    let numstat = git(
        &root,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--numstat",
            "-z",
            base,
            "--",
        ],
    )?;
    let mut added = 0u64;
    let mut removed = 0u64;
    let mut changed = 0usize;
    let mut excluded = 0usize;
    let mut limited = false;
    let mut source_paths = Vec::new();
    let mut path_bytes = 0usize;
    for record in numstat.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let fields: Vec<&[u8]> = record.splitn(3, |b| *b == b'\t').collect();
        if fields.len() != 3 {
            continue;
        }
        let Ok(file) = std::str::from_utf8(fields[2]) else {
            excluded += 1;
            continue;
        };
        if !supported_source_path(file, config) {
            excluded += 1;
            continue;
        }
        let a = String::from_utf8_lossy(fields[0]).parse::<u64>();
        let r = String::from_utf8_lossy(fields[1]).parse::<u64>();
        if let (Ok(a), Ok(r)) = (a, r) {
            added += a;
            removed += r;
            changed += 1;
            if a > 0 {
                if source_paths.len() >= 500
                    || file.len() + 1 > MAX_PATCH_PATH_BYTES.saturating_sub(path_bytes)
                {
                    limited = true;
                } else {
                    path_bytes += file.len() + 1;
                    source_paths.push(file.to_string());
                }
            }
        } else {
            excluded += 1;
        }
    }
    let mut lines = if source_paths.is_empty() {
        BTreeMap::new()
    } else {
        // Excluded generated files can be enormous; never request their patch.
        // Explicit prefixes/color override the user's display preferences, and
        // literal pathspecs ensure names such as :(glob)*.rs remain just filenames.
        let mut args = vec![
            "diff",
            "--no-color",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--unified=0",
            base,
            "--",
        ];
        args.extend(source_paths.iter().map(String::as_str));
        let patch = git(&root, &args)?;
        added_from_patch_config(&String::from_utf8_lossy(&patch), config)
    };
    let untracked = git(&root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut untracked_bytes = 0usize;
    for (index, bytes) in untracked
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .enumerate()
    {
        if index >= 500 {
            limited = true;
            break;
        }
        let Ok(file) = std::str::from_utf8(bytes) else {
            excluded += 1;
            continue;
        };
        if !supported_source_path(file, config) {
            excluded += 1;
            continue;
        }
        let absolute = root.join(file);
        let Ok(meta) = std::fs::symlink_metadata(&absolute) else {
            limited = true;
            continue;
        };
        if !meta.is_file() || meta.file_type().is_symlink() {
            excluded += 1;
            continue;
        }
        if meta.len() > MAX_UNTRACKED_FILE as u64 {
            limited = true;
            continue;
        }
        let limit = MAX_UNTRACKED_FILE.min(MAX_OUTPUT.saturating_sub(untracked_bytes));
        if limit == 0 {
            limited = true;
            break;
        }
        // The file may grow or be replaced between metadata and open. Reject a
        // replaced symlink/FIFO and bound the actual read, not its earlier size.
        let Ok(reader) = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&absolute)
        else {
            limited = true;
            continue;
        };
        if !reader.metadata().is_ok_and(|m| m.is_file()) {
            excluded += 1;
            continue;
        }
        let Ok((bytes, truncated)) = read_bounded(reader, limit) else {
            limited = true;
            continue;
        };
        untracked_bytes += bytes.len();
        if truncated {
            limited = true;
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            excluded += 1;
            continue;
        };
        if text.contains('\0') {
            excluded += 1;
            continue;
        }
        let file_lines: Vec<String> = text.lines().map(String::from).collect();
        added += file_lines.len() as u64;
        changed += 1;
        lines.insert(file.to_string(), file_lines);
    }
    let mut possible = evidence_lines(&root, evidence, config);
    let mut matched = 0u64;
    for (file, lines) in lines {
        let Some(counts) = possible.get_mut(&file) else {
            continue;
        };
        for line in lines {
            if let Some(n) = counts.get_mut(&line) {
                if *n > 0 {
                    matched += 1;
                    *n -= 1;
                }
            }
        }
    }
    matched = matched.min(added);
    let pct = |n: u64| (added > 0).then_some(100.0 * n as f64 / added as f64);
    Ok(
        json!({"ok":true,"head":head,"branch":branch,"changedFiles":changed,"addedLines":added,"removedLines":removed,
        "excludedFiles":excluded,"limited":limited,"scope":"current-working-tree-source-files",
        "attribution":{"method":"exact-successful-edit-match","scope":"working-tree-added-lines",
            "aiMatchedLines":matched,"unknownLines":added-matched,"aiPct":pct(matched),"unknownPct":pct(added-matched),
            "humanPct":Value::Null,"coveragePct":pct(matched),
            "minMatchChars":config.pointer("/git/minMatchChars").and_then(Value::as_u64).unwrap_or(12).clamp(1,1000),
            "caveat":"Совпадение текста с успешной правкой ИИ — эвристика, не доказательство авторства. Остальное неизвестно. Только текущие добавления файлов по выбранным фильтрам; минимальная длина совпадения настраивается."},
        "gitAi":git_ai(&root)}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestRepo(PathBuf);
    impl TestRepo {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "jarvis-repository-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            let repo = Self(path.canonicalize().unwrap());
            git(&repo.0, &["init", "--quiet"]).unwrap();
            repo
        }
        fn commit(&self) {
            git(&self.0, &["add", "--all"]).unwrap();
            git(
                &self.0,
                &[
                    "-c",
                    "user.name=Jarvis Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "--quiet",
                    "--no-gpg-sign",
                    "-m",
                    "fixture",
                ],
            )
            .unwrap();
        }
    }
    impl Drop for TestRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn descendant_holding_stdout_cannot_bypass_deadline() {
        let started = Instant::now();
        let result = command_with_limits(
            Path::new("/tmp"),
            "/bin/sh",
            &["-c", "sleep 5 & exit 0"],
            Duration::from_millis(80),
            1024,
        );
        assert!(result.unwrap_err().contains("превышено время ожидания"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn commands_preserve_output_and_reject_overflow() {
        let output = command_with_limits(
            Path::new("/tmp"),
            "/bin/sh",
            &["-c", "printf 12345678"],
            Duration::from_secs(1),
            8,
        )
        .unwrap();
        assert_eq!(output, b"12345678");
        assert!(command_with_limits(
            Path::new("/tmp"),
            "/bin/sh",
            &["-c", "printf 123456789"],
            Duration::from_secs(1),
            8
        )
        .unwrap_err()
        .contains("вывод превышает"));
    }
    #[test]
    fn growing_reader_is_limited_by_actual_bytes() {
        let (bytes, limited) = read_bounded(io::repeat(b'x'), 32).unwrap();
        assert_eq!(bytes.len(), 33);
        assert!(limited);
        let (bytes, limited) = read_bounded(&b"exact"[..], 5).unwrap();
        assert_eq!(bytes, b"exact");
        assert!(!limited);
    }
    #[test]
    fn source_diff_ignores_display_config_and_large_excluded_artifacts() {
        let repo = TestRepo::new();
        let paths = ["normal.rs", "space name.rs", ":(glob)*.rs"];
        for path in paths {
            std::fs::write(repo.0.join(path), "let original_value = true;\n").unwrap();
        }
        std::fs::write(repo.0.join("Cargo.lock"), "small\n").unwrap();
        repo.commit();
        git(&repo.0, &["config", "color.ui", "always"]).unwrap();
        git(&repo.0, &["config", "diff.noprefix", "true"]).unwrap();
        git(&repo.0, &["config", "diff.mnemonicPrefix", "true"]).unwrap();
        let mut evidence = Vec::new();
        for path in paths {
            std::fs::write(repo.0.join(path), "let changed_value = true;\n").unwrap();
            evidence
                .push(json!({"path":path,"cwd":repo.0,"addedLines":["let changed_value = true;"]}));
        }
        std::fs::write(
            repo.0.join("fresh.rs"),
            "let first_value = 1;\nlet second_value = 2;\n",
        )
        .unwrap();
        std::fs::write(repo.0.join("skip.min.js"), "generated\n").unwrap();
        // A patch of this excluded artifact would exceed the command byte cap.
        std::fs::write(repo.0.join("Cargo.lock"), vec![b'x'; MAX_OUTPUT + 1]).unwrap();
        let value = inspect_inner(repo.0.to_str().unwrap(), &evidence, &Value::Null).unwrap();
        assert_eq!(value["addedLines"], 5);
        assert_eq!(value["removedLines"], 3);
        assert_eq!(value["changedFiles"], 4);
        assert_eq!(value["excludedFiles"], 2);
        assert_eq!(value["attribution"]["aiMatchedLines"], 3);
        assert_eq!(value["attribution"]["unknownLines"], 2);
        assert!(value["attribution"]["humanPct"].is_null());
        assert_eq!(value["limited"], false);
    }
    #[test]
    fn unknown_is_never_human_and_old_git_ai_is_rejected() {
        let old = json!({"ai_additions":6,"human_additions":4,"git_diff_added_lines":10});
        assert_eq!(git_ai_value(&old)["available"], false);
        let v = git_ai_value(
            &json!({"ai_additions":3,"human_additions":2,"unknown_additions":5,"git_diff_added_lines":10}),
        );
        assert_eq!(v["humanPct"], 20.0);
        assert_eq!(v["unknownPct"], 50.0);
        assert_eq!(
            git_ai_value(
                &json!({"ai_additions":3,"human_additions":2,"unknown_additions":0,"git_diff_added_lines":10})
            )["available"],
            false
        );
    }
    #[test]
    fn patch_does_not_count_headers_or_generated_files() {
        let p="diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -0,0 +1 @@\n+let useful = true;\ndiff --git a/a.min.js b/a.min.js\n+++ b/a.min.js\n@@ -0,0 +1 @@\n+minified;\n";
        let v = added_from_patch(p);
        assert_eq!(v.len(), 1);
        assert_eq!(v["a.rs"], vec!["let useful = true;"]);
    }
    #[test]
    fn empty_denominator_is_null() {
        let v = git_ai_value(
            &json!({"ai_additions":0,"human_additions":0,"unknown_additions":0,"git_diff_added_lines":0}),
        );
        assert!(v["aiPct"].is_null());
    }

    #[test]
    fn custom_language_filters_and_match_threshold_apply_to_real_repository() {
        let repo = TestRepo::new();
        std::fs::write(repo.0.join("seed.rs"), "fn main() {}\n").unwrap();
        repo.commit();
        std::fs::create_dir(repo.0.join("sources")).unwrap();
        let custom = repo.0.join("sources/code.customlang");
        std::fs::write(&custom, "abc\n").unwrap();
        let evidence = vec![json!({"path":custom,"addedLines":["abc"],"cwd":repo.0})];
        assert_eq!(
            inspect(repo.0.to_str().unwrap(), &evidence)["addedLines"],
            0
        );
        let mut config = json!({"git":{"sourceExtensions":[".customlang"],"excludeDirectories":[],
            "excludeSuffixes":[],"excludeNameFragments":[],"minMatchChars":3}});
        let report = inspect_with_config(repo.0.to_str().unwrap(), &evidence, &config);
        assert_eq!(report["addedLines"], 1);
        assert_eq!(report["attribution"]["aiMatchedLines"], 1);
        config["git"]["minMatchChars"] = json!(4);
        assert_eq!(
            inspect_with_config(repo.0.to_str().unwrap(), &evidence, &config)["attribution"]
                ["aiMatchedLines"],
            0
        );
        config["git"]["excludeDirectories"] = json!(["sources"]);
        assert_eq!(
            inspect_with_config(repo.0.to_str().unwrap(), &evidence, &config)["addedLines"],
            0
        );
        assert!(!source_file_config(".git/config.customlang", &config));
    }
}
