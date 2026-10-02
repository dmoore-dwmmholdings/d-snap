use std::path::PathBuf;

fn main() {
    ensure_sidecar_placeholder();
    tauri_build::build();
}

/// tauri-build fails when the `externalBin` sidecar is missing, which breaks plain
/// `cargo build`/`clippy`. Write an empty placeholder; `tauri build` replaces it with the real
/// dsnap CLI first (beforeBuildCommand runs `scripts/sidecar.mjs`). If writing fails,
/// tauri-build reports the missing file.
fn ensure_sidecar_placeholder() {
    let Ok(target) = std::env::var("TARGET") else {
        return;
    };
    let ext = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let path = PathBuf::from("binaries").join(format!("dsnap-{target}{ext}"));
    if !path.exists() {
        let _ = std::fs::create_dir_all("binaries");
        let _ = std::fs::write(&path, b"");
    }
}
