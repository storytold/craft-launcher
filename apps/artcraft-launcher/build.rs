//! Windows only: embed the app icon and version info (VERSIONINFO) into `artcraft-launcher.exe`.
//!
//! On every other target this does nothing. A missing resource compiler is a warning, so a
//! cross-compile from macOS or Linux still links, unless `ARTCRAFT_LAUNCHER_REQUIRE_WINRES=1` (set
//! by the release packaging) turns it into an error.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/artcraft-launcher.ico");
    println!("cargo:rerun-if-env-changed=ARTCRAFT_LAUNCHER_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/app-icon/artcraft-launcher.ico")
        .set("ProductName", "ArtCraft Launcher")
        .set("FileDescription", "ArtCraft Launcher")
        .set("CompanyName", "Learning Machines LLC")
        .set("LegalCopyright", "Copyright (c) the ArtCraft Launcher authors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "artcraft-launcher.exe")
        .set("InternalName", "artcraft-launcher");
    if let Err(e) = res.compile() {
        if std::env::var("ARTCRAFT_LAUNCHER_REQUIRE_WINRES").as_deref() == Ok("1") {
            println!("cargo:warning=embedding Windows resources failed: {e}");
            std::process::exit(1);
        }
        println!("cargo:warning=artcraft-launcher.exe built without icon/version resources: {e}");
    }
}
