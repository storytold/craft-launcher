# Releasing

The version lives only in `[workspace.package] version` in the root `Cargo.toml`
(`cargo xtask version`, `cargo xtask version set X.Y.Z[-pre]`). Pushing to the `release` branch runs
`.github/workflows/release.yml`, which builds every platform and creates the draft GitHub Release
**ArtCraft Launcher v<version>** with `SHA256SUMS.txt`.

Release asset names are fixed; the launcher's own self-update and the README rely on them:

| Platform | Files |
|---|---|
| Windows | `artcraft-launcher-<ver>-windows-{x64,x86,arm64}.msi`, `…-windows-{x64,x86,arm64}-portable.zip` |
| macOS | `artcraft-launcher-<ver>-macos-universal.dmg` |
| Linux | `artcraft-launcher-<ver>-linux-{x86_64,aarch64}.{AppImage,AppImage.zsync,deb,rpm,tar.gz,flatpak}`, `artcraft-launcher-<ver>-linux-riscv64.{deb,rpm,tar.gz}` |
| FreeBSD | `artcraft-launcher-<ver>-freebsd-x86_64.tar.gz` |
| Web | `artcraft-launcher-web-<ver>.zip` |

Packaging facts the code relies on:

- The Windows MSI is **per-user** (no UAC prompt), by default into
  `%LOCALAPPDATA%\Programs\ArtCraft Launcher`. Double-clicked it opens a WixUI setup wizard
  (Welcome; a folder page with a "Create a desktop shortcut" box; Ready; progress; Finish with
  "Launch"), whose art `package.ps1` draws at build time; self-update runs it with `/passive`, no
  wizard. The folder and the shortcut choice are recorded under
  `HKCU\Software\Learning Machines LLC\ArtCraft Launcher` (`InstallDir`,
  `DesktopShortcutChoice`) and reused by the next version, and `self_update.rs` reads `InstallDir`
  to know the running copy came from the MSI. Its shortcuts set AppUserModelID
  `ai.storyteller.artcraft-launcher`, which toast notifications need. `portable.txt` beside the exe switches on portable mode
  (`ArtCraftLauncherData\` next to the exe).
- When automatic updates are enabled, the launcher can update an existing per-user Craft app
  MSI with `/qn`. First installs, downgrades, machine-scope updates, and launcher self-updates
  retain visible progress. A queued app update reads the preference when its installer gets its
  turn; changing the preference does not affect an already-running installer. Required Windows
  permission prompts are still shown.
- The macOS bundle is `ArtCraft Launcher.app` (executable `Contents/MacOS/ArtCraft Launcher`), a
  normal Dock app that can also sit in the menu bar; `LSUIElement` is not set.
- Linux: the tray uses StatusNotifierItem over D-Bus and notifications use
  `org.freedesktop.Notifications`, both in pure Rust (no GTK, appindicator or libdbus).
- Inside the Flatpak launcher (`process::in_sandbox()`), apps normally install as host Flatpaks
  and run through `flatpak-spawn --host`. When the host has no usable Flatpak — none installed,
  or `flatpak-spawn` not allowed — `linux::order_available_kinds` offers the release tarball
  instead: it is unpacked into `<data>/apps/<app>` (`bin/`, `share/`) and run with `HOME` =
  `<data>/apps/<app>/home`, so a Craft App's settings stay in the sandbox. An update swaps
  `bin/` and `share/` and keeps `home/`; removing the app takes those two folders only. Outside
  a sandbox the tarball is not on offer at all.
- `--socket=pulseaudio` in the launcher manifests is for an app running *inside* the sandbox
  (SoundCraft's ALSA reaches the runtime's PulseAudio plugin through it); it also covers the
  microphone, which SoundCraft records through.
- `build.rs` reads `ARTCRAFT_LAUNCHER_BUILD_SHA` / `ARTCRAFT_LAUNCHER_BUILD_DATE` and, on Windows,
  embeds `assets/app-icon/artcraft-launcher.ico`, failing the build when
  `ARTCRAFT_LAUNCHER_REQUIRE_WINRES=1` and it can't.
