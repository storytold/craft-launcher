//! Windows only: load the helper's own imports from System32 only, and embed its icon and
//! version info. Nothing on other targets. A missing resource compiler is a warning unless
//! `ARTCRAFT_LAUNCHER_REQUIRE_WINRES=1` (set by the release packaging).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/artcraft-launcher.ico");
    println!("cargo:rerun-if-env-changed=ARTCRAFT_LAUNCHER_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // The exe's own imports come only from System32 (`LOAD_LIBRARY_SEARCH_SYSTEM32`), never its
    // folder, which the person's programs can write. Everything loaded after `main` starts is
    // covered by `SetDefaultDllDirectories` (`main.rs`).
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
        // msi.dll is loaded on first use, after `main` has limited every load to System32. Loaded
        // at start, its own imports were found by Windows' default search, which looks in this
        // program's folder first: a live run with planted DLLs failed to start that way.
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:msi.dll");
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/app-icon/artcraft-launcher.ico")
        .set("ProductName", "ArtCraft Launcher")
        .set("FileDescription", "ArtCraft Launcher installer helper")
        .set("CompanyName", "Learning Machines LLC")
        .set("LegalCopyright", "Copyright (c) the ArtCraft Launcher authors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "artcraft-launcher-helper.exe")
        .set("InternalName", "artcraft-launcher-helper");
    if let Err(e) = res.compile() {
        if std::env::var_os("ARTCRAFT_LAUNCHER_REQUIRE_WINRES").is_some() {
            println!("cargo:warning=embedding Windows resources failed: {e}");
            std::process::exit(1);
        }
        println!("cargo:warning=artcraft-launcher-helper.exe built without icon/version resources: {e}");
    }
}
