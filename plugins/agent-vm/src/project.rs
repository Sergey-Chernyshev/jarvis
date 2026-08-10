use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectIdentity {
    pub project_id: String,
    pub vm_name: String,
    pub display_name: String,
    pub canonical_path: PathBuf,
}

impl ProjectIdentity {
    pub fn from_path(path: &Path) -> Result<Self, String> {
        let canonical_path = fs::canonicalize(path)
            .map_err(|err| format!("project path {} недоступен: {err}", path.display()))?;
        if !canonical_path.is_dir() {
            return Err(format!(
                "project path {} не является каталогом",
                canonical_path.display()
            ));
        }
        let display_name = canonical_path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("project")
            .to_string();
        let hash = fnv1a64(canonical_path.as_os_str().as_bytes());
        let hash_hex = format!("{hash:016x}");
        let mut slug = dns_slug(&display_name);
        let suffix = &hash_hex[..12];
        let max_slug = 63 - suffix.len() - 1;
        slug.truncate(max_slug);
        slug = slug.trim_matches('-').to_string();
        if slug.is_empty() {
            slug = "project".into();
        }
        let vm_name = format!("{slug}-{suffix}");
        debug_assert!(is_valid_vm_name(&vm_name));
        Ok(Self {
            project_id: format!("project-{hash_hex}"),
            vm_name,
            display_name,
            canonical_path,
        })
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn dns_slug(raw: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

pub fn is_valid_vm_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 63 || name.starts_with('-') || name.ends_with('-') {
        return false;
    }
    name.bytes()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-')
}

pub fn ensure_project_link(
    project_links: &Path,
    project: &ProjectIdentity,
) -> Result<PathBuf, String> {
    fs::create_dir_all(project_links).map_err(|err| {
        format!(
            "не создать каталог project bindings {}: {err}",
            project_links.display()
        )
    })?;
    fs::set_permissions(project_links, fs::Permissions::from_mode(0o700)).map_err(|err| {
        format!(
            "не защитить каталог project bindings {}: {err}",
            project_links.display()
        )
    })?;
    let link = project_links.join(&project.vm_name);
    match fs::symlink_metadata(&link) {
        Ok(_) => {
            let target = fs::canonicalize(&link)
                .map_err(|err| format!("project binding {} повреждён: {err}", link.display()))?;
            if target != project.canonical_path {
                return Err(format!(
                    "VM name {} уже привязан к другой project path",
                    project.vm_name
                ));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            symlink(&project.canonical_path, &link).map_err(|err| {
                format!(
                    "не создать project binding {} → {}: {err}",
                    link.display(),
                    project.canonical_path.display()
                )
            })?;
        }
        Err(err) => {
            return Err(format!(
                "не проверить project binding {}: {err}",
                link.display()
            ));
        }
    }
    Ok(link)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn make_project(parent: &str, name: &str) -> PathBuf {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!(
                "jarvis-agent-vm-project-{parent}-{}-{id}",
                std::process::id()
            ))
            .join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn identity_uses_canonical_path_not_only_basename() {
        let left = make_project("left", "sup");
        let right = make_project("right", "sup");

        let a = ProjectIdentity::from_path(&left).unwrap();
        let b = ProjectIdentity::from_path(&right).unwrap();

        assert_ne!(a.project_id, b.project_id);
        assert_ne!(a.vm_name, b.vm_name);
        assert!(a.vm_name.starts_with("sup-"));
        assert!(b.vm_name.starts_with("sup-"));
        assert!(is_valid_vm_name(&a.vm_name));
        assert!(is_valid_vm_name(&b.vm_name));
        fs::remove_dir_all(left.parent().unwrap()).unwrap();
        fs::remove_dir_all(right.parent().unwrap()).unwrap();
    }

    #[test]
    fn identity_normalizes_long_and_non_dns_project_names() {
        let project = make_project(
            "normalize",
            "Very Long Project_name With Spaces и unicode and more than sixty three chars",
        );

        let identity = ProjectIdentity::from_path(&project).unwrap();

        assert!(identity.vm_name.len() <= 63);
        assert!(is_valid_vm_name(&identity.vm_name));
        fs::remove_dir_all(project.parent().unwrap()).unwrap();
    }

    #[test]
    fn project_link_is_private_and_never_retargets_an_existing_binding() {
        let state = make_project("state", "bindings");
        let first = make_project("binding-a", "sup");
        let second = make_project("binding-b", "sup");
        let identity = ProjectIdentity::from_path(&first).unwrap();

        let link = ensure_project_link(&state, &identity).unwrap();
        assert_eq!(
            fs::canonicalize(&link).unwrap(),
            fs::canonicalize(&first).unwrap()
        );

        let spoofed = ProjectIdentity {
            canonical_path: fs::canonicalize(&second).unwrap(),
            ..identity
        };
        let err = ensure_project_link(&state, &spoofed).unwrap_err();
        assert!(
            err.contains("другой project"),
            "binding conflict is explicit: {err}"
        );

        fs::remove_dir_all(state.parent().unwrap()).unwrap();
        fs::remove_dir_all(first.parent().unwrap()).unwrap();
        fs::remove_dir_all(second.parent().unwrap()).unwrap();
    }
}
