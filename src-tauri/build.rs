fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        for source in [
            "src/platform/macos/screen_audio.m",
            "src/platform/macos/power.m",
            "src/platform/macos/permissions.m",
            "src/platform/macos/insert_paste.m",
            "src/platform/macos/hotkey_carbon.m",
        ] {
            println!("cargo:rerun-if-changed={source}");
        }
        cc::Build::new()
            .file("src/platform/macos/screen_audio.m")
            .file("src/platform/macos/power.m")
            .file("src/platform/macos/permissions.m")
            .file("src/platform/macos/insert_paste.m")
            .file("src/platform/macos/hotkey_carbon.m")
            .flag("-fobjc-arc")
            .compile("pronto_screen_audio");
        for framework in [
            "ScreenCaptureKit",
            "CoreMedia",
            "CoreAudio",
            "Foundation",
            "CoreGraphics",
            "AVFoundation",
            "ApplicationServices",
            "AppKit",
            "Carbon",
        ] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
    tauri_build::build()
}
