fn main() {
    println!("cargo:rerun-if-changed=resources/ui.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("resources/ui.manifest");
        // Scope visual styles to the UI executable; CLI tools and libraries keep
        // their existing activation context and privilege requirements.
        println!("cargo:rustc-link-arg-bin=pc-control-ui=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=pc-control-ui=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
