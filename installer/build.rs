use std::path::PathBuf;

/// Embeds the Pronto app payload (a zip of pronto.exe and its resources,
/// produced by scripts/build-installer.ps1) when PRONTO_PAYLOAD_ZIP is set.
/// Without it the installer still builds, so CI can check and test it, but it
/// refuses to install.
fn main() {
    println!("cargo:rerun-if-env-changed=PRONTO_PAYLOAD_ZIP");
    println!("cargo:rerun-if-changed=../src-tauri/tauri.conf.json");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let payload = match std::env::var_os("PRONTO_PAYLOAD_ZIP") {
        Some(path) => {
            let path = PathBuf::from(path);
            assert!(
                path.is_file(),
                "PRONTO_PAYLOAD_ZIP does not exist: {}",
                path.display()
            );
            println!("cargo:rerun-if-changed={}", path.display());
            path
        }
        None => {
            let empty = out.join("empty-payload.zip");
            std::fs::write(&empty, []).unwrap();
            empty
        }
    };
    println!("cargo:rustc-env=PRONTO_PAYLOAD={}", payload.display());

    // The installer reports the version of the app it carries.
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string("../src-tauri/tauri.conf.json").unwrap())
            .unwrap();
    let version = config["version"].as_str().expect("app version");
    println!("cargo:rustc-env=PRONTO_APP_VERSION={version}");
    tauri_build::build()
}
