<!-- craft-house-rules:start core source=dde09ea31e2e457f5b4653a6e76f7cdf55944bdf sha256=540e6cea75d12d0710c3dca89658328e81e508ecef102446ca5bb95b7c0b92bb -->
## House rules

These apply in every Craft repository. A later rule may be stricter, not weaker. The source is `standards/house-rules-core.md` in `storytold/craft-contrib`. Propose changes there, not in a copy.

1. **A person is accountable.** Whoever runs an agent answers for what it opens and for its review comments.
2. **Check, then claim.** Before writing code, read the issue comments, search pull requests for `#N` and the keywords, and check recent commits on `main`. Comment on the issue first. Release a claim you will not finish.
3. **Verify, don't guess.** Open a definition before using it. Confirm the cause in the code, even when someone names a location.
4. **Keep fixes narrow and tested.** Fix what the issue describes, where the bug is. Add a test that fails without the fix.
5. **Review the diff before opening a pull request.** Identifiers exist as used, the test fails without the fix, nothing unrelated changed, and the description is true.
6. **Look for a better pull request.** If another fixes the same thing, close yours when it is better or equal, and say why.
7. **Report only what ran.** Say what you tested and what you could not run. Without a local build or test, say so and rely on CI before marking ready.
8. **`main` changes through a pull request.** Do not push to it. Branch protection enforces this. There is no exception.
9. **Stop and ask only about** licensing outside the allowlist, publishing, releases, a new remote, secrets, spending money, destructive git, and anything else hard to undo. Otherwise keep working. `plan/` is a gitignored notebook, not rules. Without it, do not ask for it and do not stop.
10. **Keep secrets and personal details out** of the repository, logs and requests.
11. **CI stays shared.** Workflows call `storytold/craft-common-actions`. Do not change CI in a feature or fix pull request. If CI cannot cover a change, open an issue there.
12. **Guards say why.** A check that looks removable says what it prevents. Read that before changing it.
13. **Instructions must be readable by everyone.** No notes about one machine, and no links to files a reader cannot open. `craftrules` is retired. Do not link to it.
14. **Keep pull requests moving.** Concurrent pull requests are fine; unanswered actionable feedback is not. Respond in 7 days, a reminder at 14, closure eligible at 30, from when an actionable request waits on the author. Suggestions and unconfirmed bot findings do not start the clock. A fix, explanation or agreed plan resets it; unrelated activity does not. Waiting on a reviewer or maintainer pauses it and does not count against the author. A maintainer decides closure; it is never automatic. Reuse search results, do not poll, and stop at a rate limit.
15. **No agent attribution.** A pull request does not say an agent wrote it. A repository rule may not require that.
<!-- craft-house-rules:end -->
<!-- craft-house-rules:start rust source=dde09ea31e2e457f5b4653a6e76f7cdf55944bdf sha256=387e0ccb4b49e567f60951d7d6fff7305054c89f43ad812b9993dc7c38528013 -->
## Rust Craft repositories

These apply to native Craft apps and to shared Rust libraries. Select them by repository type. A repository that merely contains some Rust does not take this block. Rules later in a repository's file may be stricter, but not weaker. The source is `standards/house-rules-rust.md` in [storytold/craft-contrib](https://github.com/storytold/craft-contrib).

1. **Never crash.** Non-test code does not panic and has no `unsafe`, except where the repository file names an exception. Return errors, treat input as hostile, and bound recursion and allocations. New production crate roots copy the `deny` attribute from an existing crate: `clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic`, `clippy::unimplemented`, `clippy::todo`, `clippy::unreachable`. Every crash fix comes with a test that panicked before the fix.
2. **Clean room.** Learn from observed behaviour and public specifications. Do not copy another product's code, assets, strings or artwork. Check a dependency's licence before adding it.
3. **Assets and fonts.** Every asset is original or suitably licensed, and is recorded in `ATTRIBUTION.md` in the same change. Font files live in [storytold/craft-fonts](https://github.com/storytold/craft-fonts), not in the app repository.
4. **Rust only for native Craft apps.** No handwritten JavaScript or TypeScript, and no Tauri, Electron or webview shell, in those apps. Shared tooling follows its documented technical scope and maintainer-approved exceptions. Craft PR Review's existing Tauri and TypeScript UI is one such exception. It does not relax this rule for native apps. Preserve any existing web build.
5. **Time goes through `chrono`.** Do not hand-roll calendar conversion. For dates beyond what `chrono` can represent, follow [standards/time.md](https://github.com/storytold/craft-contrib/blob/main/standards/time.md).
6. **Contributor credits.** Add or edit only your own person's entry in `contributors/people.toml`, and only after asking them. Never guess a name or hand-edit `contributors.json`.

## Licence allowlist

Dependencies and assets use SPDX identifiers. An expression is allowed when every identifier is in this list. A repository may name a shorter list. It may not add an identifier here by editing its own copy of these rules.

- `ISC`
- `MIT`
- `BSD-2-Clause`
- `BSD-3-Clause`
- `Apache-2.0`
- `OFL-1.1`
- `Ubuntu-font-1.0`
- `Bitstream-Vera`
- `CC0-1.0`
- `CC-BY-4.0`
- `CC-BY-SA-4.0`
- `LicenseRef-Public-Domain`

`LicenseRef-Storyteller-Trademark` is not in this list. It covers Storyteller brand marks only, where a repository already says so.
<!-- craft-house-rules:end -->

# AGENTS.md: guide for AI agents and contributors

ArtCraft Launcher installs, updates and launches the **Craft App Suite** (PhotoCraft, LightCraft,
FilmCraft, DesignCraft, VectorCraft, PdfCraft, EffectCraft, WordCraft, DeckCraft, GridCraft,
CADCraft, SoundCraft, FigCraft, Craft Anything, TextureCraft, MaterialCraft), plus approved **community apps** made outside ArtCraft and labelled
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
only; English where there is none): never bundle a CJK font, they are 10 MB and more. Georgian
borrows the system's Georgian font the same way.

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
6. **Time in this repository goes through `jiff`.** Do not hand-roll calendar conversion, and do not add `chrono` beside it.

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

New workspace crates include `[lints] workspace = true` in `Cargo.toml`. Production crate roots declare the crate-level lint attributes before modules; crate-level `//!` documentation may precede them.
