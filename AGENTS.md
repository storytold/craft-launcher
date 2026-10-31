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
                                        verify and install
  ui-egui   artcraft-launcher-ui-egui   the shared egui UI (desktop and web)
apps/
  artcraft-launcher                     desktop shell (eframe): window, tray icon, notifications
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
- **No `unsafe`.** The workspace sets `unsafe_code = "forbid"`. The one exception is the installer
  helper (`apps/artcraft-launcher-helper`), which sets `unsafe_code = "deny"` for reviewed Windows
  calls with no safe wrapper: `SetDefaultDllDirectories` in its `main.rs`, and WinTrust's chain plus
  Windows Installer's read-only database API in `package_facts.rs`. Add nothing else there; any new
  call needs a maintainer's review.
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
- Never delete a user's files outside what the launcher itself installed.

### Windows elevated installs: guards that must stay

Each of these closes a hole found in review or in a real incident
(`docs/incident-2026-10-windows-antivirus.md`, storytold/craft-launcher#1080). None is
redundant. Removing or "simplifying" one reopens it, so change them only with that reason in hand.

- **No script interpreter, encoded command or hidden window.** Elevated work runs in
  `artcraft-launcher-helper.exe` or in `msiexec` called directly. A hidden, elevated, base64
  PowerShell (#974) was deleted by antivirus as a Trojan, and users reported it as a compromise.
- **The helper is a separate exe that loads DLLs from System32 only** (`/DEPENDENTLOADFLAG`,
  `SetDefaultDllDirectories` first in `main`, no graphics imports). It sits in the person's own
  launcher folder, where any of their programs can plant a DLL.
- **The helper is resolved, held open and publisher-checked before the prompt**
  (`elevated::checked_helper`). That folder is writable by the person's programs, so the prompt
  would otherwise name whatever was put there.
- **The helper must be this build's helper, not just ours** (`helper_build::check`): its signed
  VERSIONINFO must say `artcraft-launcher-helper.exe` and the launcher's own version. The publisher
  alone admits any program we ever signed, such as an older launcher renamed as the helper, which
  loads planted DLLs when elevated.
- **The job travels on the command line, and the helper checks every part again.** It trusts
  nothing the unelevated launcher wrote.
- **Packages are resolved, then held, then hashed and publisher-checked, and that one path goes to
  `msiexec`** (`held`, `run_msi`, `downgrade_msi`). The downloads folder is the person's own: a
  file or a junction above it could be swapped between the check and the install.
- **The helper installs only packages whose signature chains to the pinned Microsoft root**
  (`identity::SIGNING_ROOT_SHA256`). Never fall back to the subject and issuer names alone: Windows
  trusts roots the person's own account adds, and the helper runs as that account, so any of their
  programs could give itself our publisher's name and have our genuine prompt install its package
  for all users (shown live on #1080).
- **The unelevated launcher refuses a package whose signing certificate was revoked**
  (`revocation::check`, from `held::check_publisher`), before an all-users install and before a
  downgrade's prompt. Expiry is no substitute: the packages are timestamped, so a signature stays
  valid after its certificate expires. The helper repeats that decision against Windows' cache
  through required `PackageFacts::check_revocation`, before reading the product or removing it,
  so starting the helper directly cannot bypass a known revocation. Every helper trust check is
  cache-only (`WTD_CACHE_ONLY_URL_RETRIEVAL` for the raw WinTrust root check); skipping revocation
  alone does not prevent network retrieval. An unanswerable check (offline) continues with a
  warning; that is a stated `shortcut:`, not a promise of fresh revocation information.
- **The helper installs a package only for the app it replaces:** Windows Installer's HKLM
  registration must link the package's UpgradeCode to every product being removed
  (`identity::check`). Our publisher signs every Craft app and the launcher, so the signature alone
  would let one replace another.
- **A release build refuses an all-users install of an official app when it can't verify its own
  signature** (`held::skips_unsigned_self`). Only a development build skips the check. A community
  app is signed by its own developer, so its package checksum is the check, and its all-users
  downgrade is refused before the prompt.
- **Run folders live in `Program Files\ArtCraft\Launcher installer runs`, never ProgramData**, and a
  link or junction in place of that folder is refused. Ordinary accounts can add files in
  ProgramData, so they could plant a result or redirect the clean-up.
- **`msiexec` comes from `GetSystemDirectory`, never `SystemRoot`.** The elevated helper inherits
  the person's environment, which their programs can set.
- **Elevated removal takes only all-users (HKLM) registrations.** A per-user uninstall record is in
  HKCU, so it could name any product on the machine.
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
