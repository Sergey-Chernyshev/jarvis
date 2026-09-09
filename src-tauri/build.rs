mod build_node_bundle;

fn main() {
    build_node_bundle::embed();
    build_meeting_system_audio();
    // Dev-only: встроить Info.plist (с NSMicrophoneUsageDescription) в RAW-бинарь
    // `jarvis`, чтобы macOS мог показать диалог разрешения микрофона при запуске
    // через `cargo run` (без .app-бандла). Гейтим переменной JARVIS_DEV_SIGN, чтобы
    // нотаризованный бандл (со своим Info.plist) остался нетронутым.
    println!("cargo:rerun-if-env-changed=JARVIS_DEV_SIGN");
    #[cfg(target_os = "macos")]
    if std::env::var_os("JARVIS_DEV_SIGN").is_some() {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
        println!("cargo:rerun-if-changed=dev-Info.plist");
        println!(
            "cargo:rustc-link-arg-bin=jarvis=-Wl,-sectcreate,__TEXT,__info_plist,{manifest}/dev-Info.plist"
        );
    }
    // Отпечаток встроенной панели и ветки, из которой собирали.
    //
    // Ассеты `ui/` вшиваются в бинарь макросом на этапе компиляции, и cargo
    // сам по себе не знает, что этот каталог — его вход: правка одного лишь
    // JS могла не вызвать пересборку, и приложение молча продолжало работать
    // со старой панелью. Отсюда же берётся и целый класс «я поправил, а
    // ничего не изменилось».
    println!("cargo:rerun-if-changed=../ui");
    println!("cargo:rustc-env=JARVIS_UI_FINGERPRINT={}", ui_fingerprint());
    println!("cargo:rustc-env=JARVIS_BUILD_REF={}", build_ref());

    // ВНИМАНИЕ на будущее: `jarvis-mcp` НЕ требует ни `externalBin`, ни
    // `resources` — бандлер сам кладёт в пакет каждый `[[bin]]` этого манифеста
    // (см. install/mod.rs, `mcp_src`). Попытка «доложить» мост через
    // `externalBin` роняет ровно эту функцию: `tauri_build::build()` копирует
    // внешние бинари отсюда, из build.rs, то есть ДО того, как cargo соберёт
    // `jarvis-mcp` — цель того же манифеста; отсутствующий файл там жёсткая
    // ошибка, и обычный `cargo build`/`cargo test` падает у всех.
    tauri_build::build()
}

/// A small Objective-C bridge avoids requiring Swift or a separately installed
/// helper at runtime. Weak linking keeps the microphone-only path usable on
/// the application's older supported macOS versions.
fn build_meeting_system_audio() {
    println!("cargo:rerun-if-changed=src/meetings/system_audio.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let object = out.join("jarvis_meeting_audio.o");
    let archive = out.join("libjarvis_meeting_audio.a");
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        _ => panic!("unsupported macOS architecture for meeting audio"),
    };
    let status = std::process::Command::new("xcrun")
        .args([
            "clang",
            "-c",
            "-fobjc-arc",
            "-fblocks",
            "-Wall",
            "-Wextra",
            "-Wno-unused-parameter",
            "-mmacosx-version-min=11.0",
            "-arch",
            arch,
        ])
        .arg("src/meetings/system_audio.m")
        .arg("-o")
        .arg(&object)
        .status()
        .expect("Xcode Command Line Tools are required for macOS meeting audio");
    assert!(status.success(), "compiling ScreenCaptureKit bridge failed");
    let status = std::process::Command::new("xcrun")
        .args(["ar", "crs"])
        .arg(&archive)
        .arg(&object)
        .status()
        .expect("xcrun ar");
    assert!(status.success(), "archiving ScreenCaptureKit bridge failed");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=jarvis_meeting_audio");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=CoreMedia");
    println!("cargo:rustc-link-arg=-Wl,-weak_framework,ScreenCaptureKit");
}

/// Короткий отпечаток содержимого `ui/`: по нему видно, та ли панель внутри.
fn ui_fingerprint() -> String {
    let dir = std::path::Path::new("../ui");
    let mut names: Vec<std::path::PathBuf> = Vec::new();
    collect(dir, &mut names);
    names.sort();
    // Хватит суммы длин и простой свёртки: это отпечаток «то же или другое»,
    // а не криптография.
    let mut acc: u64 = 1469598103934665603;
    for f in names {
        let bytes = std::fs::read(&f).unwrap_or_default();
        for b in f.to_string_lossy().bytes().chain(bytes) {
            acc ^= b as u64;
            acc = acc.wrapping_mul(1099511628211);
        }
    }
    // Ширину задаём форматом, а не срезом: срез по индексу упал бы на коротком
    // числе и уронил бы сборку целиком.
    format!("{acc:016x}").chars().take(8).collect()
}

fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Ветка и коммит сборки — чтобы не гадать, что именно запущено.
fn build_ref() -> String {
    let run = |args: &[&str]| -> String {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let branch = run(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let sha = run(&["rev-parse", "--short", "HEAD"]);
    if branch.is_empty() && sha.is_empty() {
        "?".into()
    } else {
        format!("{branch}@{sha}")
    }
}
