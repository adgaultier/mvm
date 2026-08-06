//! Link configuration for libkrun.
//!
//! Linux: libkrun.so.1 lives in the default loader paths; nothing to add.
//! macOS: Homebrew (libkrun/krun tap) installs it under a prefix the
//! linker does not search by default. The runtime side (libkrun dlopening
//! libkrunfw by bare name) needs an rpath, which .cargo/config.toml adds.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let prefix = std::env::var("HOMEBREW_PREFIX")
            .ok()
            .filter(|p| !p.is_empty())
            .or_else(|| {
                std::process::Command::new("brew")
                    .arg("--prefix")
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            })
            .unwrap_or_else(|| "/opt/homebrew".to_string());
        println!("cargo:rustc-link-search=native={prefix}/lib");
        println!("cargo:rerun-if-env-changed=HOMEBREW_PREFIX");
    }
}
