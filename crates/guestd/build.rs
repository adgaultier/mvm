fn main() {
    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() != Some(std::ffi::OsStr::new("linux")) {
        return;
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let package = root.join("../guest-ebpf");
    println!("cargo:rerun-if-changed={}", package.display());

    aya_build::build_ebpf(
        [aya_build::Package {
            name: "mvm-guest-ebpf",
            root_dir: package.to_str().expect("guest-ebpf path is UTF-8"),
            features: &["ebpf"],
            ..Default::default()
        }],
        aya_build::Toolchain::Nightly,
    )
    .expect("failed to build embedded guest eBPF program");
}
