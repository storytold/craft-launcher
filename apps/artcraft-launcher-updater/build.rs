//! Windows only: embed the icon, version info (VERSIONINFO) and an application manifest into
//! `artcraft-launcher-updater.exe`.
//!
//! The manifest asks to run as whoever starts it (`asInvoker`). Without one, Windows' installer
//! detection would take an executable with "update" in its name for a setup program and ask for
//! administrator rights each time a user runs `artcraft-launcher-updater check` (32-bit builds).
//!
//! On every other target this does nothing. A missing resource compiler is a warning, so a
//! cross-compile from macOS or Linux still links, unless `ARTCRAFT_LAUNCHER_REQUIRE_WINRES=1` (set
//! by the release packaging) turns it into an error.

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="ai.storyteller.artcraft-launcher-updater" version="1.0.0.0" />
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 and 11 -->
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}" />
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/artcraft-launcher.ico");
    println!("cargo:rerun-if-env-changed=ARTCRAFT_LAUNCHER_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/app-icon/artcraft-launcher.ico")
        .set_manifest(MANIFEST)
        .set("ProductName", "ArtCraft Launcher")
        .set("FileDescription", "ArtCraft Launcher Update Service")
        .set("CompanyName", "Learning Machines LLC")
        .set("LegalCopyright", "Copyright (c) the ArtCraft Launcher authors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "artcraft-launcher-updater.exe")
        .set("InternalName", "artcraft-launcher-updater");
    if let Err(e) = res.compile() {
        if std::env::var_os("ARTCRAFT_LAUNCHER_REQUIRE_WINRES").is_some() {
            println!("cargo:warning=embedding Windows resources failed: {e}");
            std::process::exit(1);
        }
        println!("cargo:warning=artcraft-launcher-updater.exe built without icon/version resources: {e}");
    }
}
