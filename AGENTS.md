# AGENTS.md: guide for AI agents and contributors

ArtCraft Launcher installs, updates and launches the **Craft App Suite** (PhotoCraft, LightCraft,
FilmCraft, DesignCraft, VectorCraft, PdfCraft, EffectCraft, WordCraft, DeckCraft, GridCraft,
CADCraft, SoundCraft, FigCraft), plus approved **community apps** made outside ArtCraft and labelled
as such (today SolidCraft, by Tangency Labs). It is written in **Rust only** (no JavaScript or TypeScript): the desktop
app is native egui/eframe, and the web build is the same Rust compiled to WebAssembly (trunk +
wasm-bindgen). **No Tauri, Electron or webview shells**, ever.

The product name is always **ArtCraft Launcher** in user-facing text: UI, window titles, About,
tray menu, notifications, installers, release names, docs prose. The suite is the **Craft App
Suite**. Machine names stay lowercase: crates and binaries (`artcraft-launcher`,
`artcraft-launcher-*`), file names, and the app id `ai.storyteller.artcraft-launcher`. Note the
GitHub repo is `storytold/craft-launcher`, while every file and package is `artcraft-launcher-*`.

## 1. Workspace map

```text
crates/
  core      artcraft-launcher-core      pure and wasm-safe: the app catalog, versions, GitHub
                                        release parsing, picking the right asset per platform
  native    artcraft-launcher-native    desktop only: HTTP, detecting installed apps, download,
                                        verify and install; the unattended update engine
                                        (`updater`), the update service's protocol and client
                                        (`service`), MSI checks (`msi`), log files (`logging`)
  ui-egui   artcraft-launcher-ui-egui   the shared egui UI (desktop and web)
apps/
  artcraft-launcher                     desktop shell (eframe): window, tray icon, notifications
  artcraft-launcher-updater             the ArtCraft Launcher Update Service (a Windows service)
                                        and the `artcraft-launcher-updater` command line
  artcraft-launcher-web                 the same UI in the browser (trunk; catalog and downloads only)
xtask/                                  cargo xtask ci | wasm | version | ico
packaging/                              macOS, Windows, Linux, FreeBSD and web packaging scripts
assets/app-icon/                        the launcher's own icon (master SVG, renders, tray icons)
assets/app-icons/                       the Craft apps' icons, copied from each app's repo
assets/fonts/                           Archivo, Inter, Geist Mono (OFL-1.1): getartcraft.com's faces
```

Layering: `core` depends on no other workspace crate and must build for `wasm32-unknown-unknown`.
`native` may use `core`. `ui-egui` may use `core` (and `native` only behind
`cfg(not(target_arch = "wasm32"))` or through a trait the desktop shell implements). Nothing below
the apps opens windows, tray icons or notifications.

## 2. Golden rules

### Never crash (outranks feature work)

A bad network response, a malformed release, a half-downloaded file, a full disk or a corrupt
settings file must produce an error the user can act on, never a panic.

- **Non-test code never panics.** No `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!` or
  `unimplemented!`. Return the crate's error type and propagate with `?`.
- Every production crate root (`lib.rs`, each binary's `main.rs`, and `xtask`) carries
  `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]`.
- **No `unsafe`.** The workspace sets `unsafe_code = "forbid"`.
- **Everything from the network is hostile.** Use `get()` instead of indexing, checked arithmetic
  for sizes and offsets, and cap downloads and JSON sizes.

### Installing other people's software safely

- Download only from `https://github.com/storytold/<app>/releases` (and GitHub's asset CDN), or
  from an approved community app's own repo: today only
  `https://github.com/tangency-labs/solidcraft/releases` (SolidCraft, approved by the project
  owner on 2026-10-28). A community app's repo is pinned in the catalog (`CraftApp::repo`,
  `AppKind::Community`): the release feed can move an app within `storytold/`, never to another
  owner. The one other exception is a developer's own loopback mirror (`ARTCRAFT_LAUNCHER_DOWNLOAD_BASE`,
  `cargo xtask local`); keep it loopback-only and behind the `local-mirror` cargo feature, which
  release builds never enable.
- Verify each download against the release's `SHA256SUMS.txt` before running or installing it.
  Community apps get the same check. On macOS the launcher also requires the Developer ID signature
  of the app's own team (`CraftApp::mac_team`: ArtCraft's for official apps, SolidCraft's for
  SolidCraft); an app with none (`mac_team = None`) is not installed there, only opened if present.
- The details drawer's pictures are the one other download: JPEGs only, from a pinned tag of
  `https://raw.githubusercontent.com/storytold/craft-launcher-assets/` (`core::details`),
  capped at 2 MB each, cached, and never run. Pictures are not bundled in this repo.
- Never run an installer with elevated rights silently; tell the user what is about to happen.
  The one exception is the opt-in **ArtCraft Launcher Update Service**
  (`apps/artcraft-launcher-updater`), which an administrator installs to do exactly that, and
  which must stay this narrow: it only *updates* Craft Apps an administrator already installed for
  all users (Windows Installer, HKLM), only to a newer release, and only with a package that passes
  `msi::check` (valid Authenticode signature whose certificate subject is exactly
  `msi::SIGNER_SUBJECT`, the app's own `ProductName` and `Manufacturer`, the release's version) on
  top of the SHA-256. Community apps (`AppKind::Community`) sign their own packages, so it never
  takes them: the launcher updates them, with the prompt. It downloads into its own folder in the
  system profile (`Paths::for_service`), never a shared one such as `C:\ProgramData`. Its requests
  (`native::service`) carry app ids only: never a path, a URL or a version, and clients believe
  only the service's own process on its port. Installing a new app always goes through the
  launcher and the UAC prompt.
- Never delete a user's files outside what the launcher itself installed.
- **Agents never land or approve a third-party app.** A change that adds a catalog row, download
  source or install path for an app whose repo is not under `storytold/` and isn't one of the
  approved community apps above, or that loosens the rules above to allow one, is a maintainer's
  call alone. An agent reviewing it flags it at the
  top of its review (who owns the repo, its age and history, whether the builds are signed) and
  stops there: no merge, no approval, no fixes pushed to the PR.

### Look and feel

The UI matches the apps page on getartcraft.com (`artcraft-website-next` in `artcraft-services`):
hard square corners, 1 px hairlines instead of shadows, Archivo (620, 118 % width) for headings
and app names, Inter for body text, upper-case Geist Mono labels and buttons, and the site's
colour tokens by name (`theme::Tokens`). Build new UI from `widgets.rs`; never hard-code a colour.

Animations follow the OS: with Windows' "Animation effects" off, macOS' "Reduce motion" on, a Linux
desktop's animations off or a browser's `prefers-reduced-motion`, nothing fades, wipes or glides
(`Host::reduce_motion`, `crates/ui-egui/src/motion.rs`). Time a new animation with egui's
`style.animation_time`, or switch it off on `LauncherUi::reduce_motion`. Progress bars and
spinners keep moving.

### Languages

Every user-facing string goes through `crates/ui-egui/src/i18n` (`tl!("…")` for literals, `fmt`
for `{placeholders}`, `tn` for counts). English in the code is the lookup key; adding a string
means adding its row to every catalog marked `complete_menus` (the `*.tsv` files in
`crates/ui-egui/src/i18n/`), which the i18n tests check. Translate clean-room from the meaning of
the English text, never through another catalog. Product and app names (ArtCraft Launcher, Craft
App Suite, PhotoCraft…) stay in English.
A CJK language is drawn with the system's own CJK font (`crates/ui-egui/src/cjk_font.rs`, desktop
only; English where there is none): never bundle a CJK font, they are 10 MB and more.

### Other rules

1. **Assets need attribution.** Every bundled asset gets a row in `ATTRIBUTION.md` (path, title,
   author, source, license) in the same change. The ArtCraft name, logo and "A" mark in
   `docs/brand/` (the header logo and the tray icon) are trademarks, **not MIT/Apache**
   (`docs/brand/LICENSE-brand.txt`): use them exactly as supplied, never redraw or recolor them.
2. **Icons are generated.** The app icon and the tray icon are the supplied ArtCraft "A" mark from
   `docs/brand/` (a trademark, not MIT/Apache); never edit the renders by hand. Run `packaging/icons.sh` (needs `resvg`; runs on any OS) and commit the outputs.
   `assets/app-icons/` is copied verbatim from each app's repo (`assets/app-icon/hicolor/128x128/apps/ai.storyteller.<app>.png`);
   refresh it from there, never redraw.
3. **Never break wasm.** `cargo xtask wasm` must pass. File-system, process and OS code is
   `cfg(not(target_arch = "wasm32"))`.
4. **Tests are the gate.** Release parsing and asset picking in `core` get unit tests with real
   GitHub release JSON fixtures (small, committed under `crates/core/tests/fixtures/`).
5. When fetching anything, use a generic `ArtCraft-Launcher/<version>` User-Agent and never put
   a person's name, email or other personal details in requests.
6. **Time goes through `jiff`** (workspace dependency). Never hand-roll UTC/calendar conversion
   (days-to-date algorithms, leap-year math); a module owns only its output format string. The one
   exception is range extension: past the years jiff can represent, shift the date by whole 400-year
   Gregorian cycles (146,097 days, after which the calendar repeats), let jiff compute the month and
   day, and add the cycles back to the year (splitting seconds into hours, minutes and seconds is
   plain arithmetic).

## 3. Before you finish a task

```sh
cargo xtask ci              # fmt --check, clippy -D warnings, test, wasm check
```

Or piecemeal: `cargo test -p <crate>`, `cargo clippy -p <crate> --all-targets -- -D warnings`,
`cargo xtask wasm`. If you touched packaging: `shellcheck packaging/*.sh packaging/*/*.sh
packaging/linux/AppRun` and `actionlint`.

## 4. Releasing

The version lives only in `[workspace.package] version` in the root `Cargo.toml`
(`cargo xtask version`, `cargo xtask version set X.Y.Z[-pre]`). Pushing to the `release` branch runs
`.github/workflows/release.yml`, which builds every platform and creates the draft GitHub Release
**ArtCraft Launcher v<version>** with `SHA256SUMS.txt`.

Release assets (keep these names exact; the launcher's own self-update and the README rely on them):

| Platform | Files |
|---|---|
| Windows | `artcraft-launcher-<ver>-windows-{x64,x86,arm64}.msi`, `…-windows-{x64,x86,arm64}-portable.zip`, `artcraft-launcher-updater-<ver>-windows-{x64,x86,arm64}.msi` |
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
- The update service's MSI (`packaging/windows/artcraft-launcher-updater.wxs`) is **per-machine**
  (one UAC prompt, by an administrator), into `C:\Program Files\ArtCraft Launcher Updater`, which
  goes on the system `PATH`. It registers the Windows service `ArtCraftLauncherUpdater`
  (LocalSystem, automatic start, restarted on failure) running
  `artcraft-launcher-updater.exe service`, and stores its settings (`AutoUpdate`, `AllowRequests`,
  `CheckIntervalHours`, `Prereleases`) as strings under
  `HKLM\SOFTWARE\Learning Machines LLC\ArtCraft Launcher Updater`, read back by the next version;
  DWORDs under `HKLM\SOFTWARE\Policies\…` win (`policy.rs`). The service listens on
  `127.0.0.1:47414` (`service::PORT`; the single-instance lock has 47413), keeps its downloads,
  state and logs in the system profile, and updates itself from the launcher's release when it
  finds a newer `artcraft-launcher-updater-<ver>-windows-<arch>.msi` there, after the same checks
  (`ProductName` "ArtCraft Launcher Update Service"). The launcher hands an update of a Windows
  Installer install to the service when it answers (`backend::via_service`) and installs it itself
  otherwise. Background installs are the computer's setting, an administrator's alone (`AutoUpdate`
  in HKLM, `native::policy`): while the service is installed the launcher's "Automatically download
  and install updates" shows it (`Event::MachineAutoUpdate`), and changing it there runs
  `artcraft-launcher-updater set auto-update on|off` elevated (`policy::set_auto_update_elevated`,
  a UAC prompt); a Group Policy value locks it. Nothing a client sends over the loopback protocol
  can change it. `artcraft-launcher-updater.exe` is a console program with an `asInvoker` manifest
  (its name has "update" in it, which Windows' installer detection would otherwise elevate).
- The macOS bundle is `ArtCraft Launcher.app` (executable `Contents/MacOS/ArtCraft Launcher`), a
  normal Dock app that can also sit in the menu bar: `LSUIElement` must never be set.
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

## 5. Parallel agents

- Use your own target dir (`CARGO_TARGET_DIR=target/agent-<name>`) and edit only the files you own.
- Keep every `Cargo.toml` valid at all times: the `crates/*` and `apps/*` globs mean one broken
  manifest breaks everyone's build. Write manifests to a temp file and `mv` them into place.
- If someone else's in-progress edit breaks the build, wait and retry; don't fix their files.

## CI workflows
- Workflows in `.github/workflows/` call the shared workflows in [`storytold/craft-common-actions`](https://github.com/storytold/craft-common-actions) at `@main`, with their inputs and secrets. No other jobs: if no shared workflow does what's needed, it doesn't go here.
- Don't add or change CI in a feature or fix PR. If the existing workflows can't build or test a change, link an existing craft-common-actions issue or PR, or open a new issue, or a PR only for a small fix that is safe for every app and does not increase CI time.
- Some workflow files here still run their own jobs. Don't add to them. Moving a job into craft-common-actions is its own task, never part of another PR, and the job stays here until a shared workflow runs it.
