use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const MANIFEST: &[u8] = include_bytes!("../../../plugins/agent-vm/manifest.json");
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

pub fn install_bundled() -> Result<bool, String> {
    if std::env::var("JARVIS_PLUGIN_DEV_DIR")
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(false);
    }
    let executable =
        std::env::current_exe().map_err(|err| format!("не определить Jarvis executable: {err}"))?;
    let source = executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("jarvis-agent-vm-plugin");
    if !source.is_file() {
        return Ok(false);
    }
    install_package(
        &source,
        &crate::util::jarvis_dir().join("plugins"),
        MANIFEST,
    )?;
    Ok(true)
}

fn ensure_private_dir(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "plugin install path {} должен быть обычным каталогом",
                path.display()
            ));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .map_err(|err| format!("не создать plugin install path: {err}"))?;
        }
        Err(err) => return Err(format!("не проверить plugin install path: {err}")),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|err| format!("не защитить plugin install path: {err}"))
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "plugin install file без parent".to_string())?;
    ensure_private_dir(parent)?;
    let temp = parent.join(format!(
        ".install-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)
            .map_err(|err| format!("не создать plugin install temp: {err}"))?;
        file.write_all(bytes)
            .map_err(|err| format!("не записать plugin install temp: {err}"))?;
        file.sync_all()
            .map_err(|err| format!("не сохранить plugin install temp: {err}"))?;
        drop(file);
        fs::rename(&temp, path).map_err(|err| format!("не заменить plugin install file: {err}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn install_package(source: &Path, plugins_root: &Path, manifest: &[u8]) -> Result<(), String> {
    let source_metadata = fs::symlink_metadata(source)
        .map_err(|err| format!("bundled Agent VM sidecar недоступен: {err}"))?;
    if !source_metadata.file_type().is_file() || source_metadata.permissions().mode() & 0o111 == 0 {
        return Err("bundled Agent VM sidecar должен быть executable file".into());
    }
    let package = plugins_root.join("agent-vm");
    let bin = package.join("bin");
    ensure_private_dir(plugins_root)?;
    ensure_private_dir(&package)?;
    ensure_private_dir(&bin)?;
    let binary =
        fs::read(source).map_err(|err| format!("не прочитать bundled Agent VM sidecar: {err}"))?;
    atomic_write(&bin.join("agent-vm-plugin"), &binary, 0o700)?;
    atomic_write(&package.join("manifest.json"), manifest, 0o600)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    fn root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "jarvis-agent-vm-install-{tag}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn installs_manifest_and_executable_atomically_with_owner_only_modes() {
        let root = root("ok");
        fs::create_dir_all(&root).unwrap();
        let source = root.join("bundled-sidecar");
        fs::write(&source, b"synthetic executable fixture").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
        let plugins = root.join("profile/plugins");

        install_package(&source, &plugins, MANIFEST).unwrap();

        let package = plugins.join("agent-vm");
        let installed = package.join("bin/agent-vm-plugin");
        assert_eq!(
            fs::read(&installed).unwrap(),
            b"synthetic executable fixture"
        );
        assert_eq!(
            fs::metadata(&installed).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(package.join("manifest.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        crate::plugins::manifest::load_package(&package.join("manifest.json")).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_symlinked_package_directory() {
        let root = root("symlink");
        fs::create_dir_all(root.join("profile/plugins")).unwrap();
        fs::create_dir_all(root.join("outside")).unwrap();
        symlink(root.join("outside"), root.join("profile/plugins/agent-vm")).unwrap();
        let source = root.join("sidecar");
        fs::write(&source, b"fixture").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();

        let err = install_package(&source, &root.join("profile/plugins"), MANIFEST).unwrap_err();

        assert!(err.contains("обычным каталогом"));
        assert!(fs::read_dir(root.join("outside")).unwrap().next().is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
