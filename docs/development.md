# Development notes

Facts about the code that `AGENTS.md` summarises.

## Workspace map

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

`core` depends on no other workspace crate and builds for `wasm32-unknown-unknown`. `native` uses `core`. `ui-egui` uses `core`, and `native` only behind `cfg(not(target_arch = "wasm32"))` or through a trait the desktop shell implements. Windows, tray icons and notifications are opened only in the apps. File-system, process and OS code is `cfg(not(target_arch = "wasm32"))`.

The product name in user-facing text is ArtCraft Launcher, and the suite is the Craft App Suite. Machine names stay lowercase: crates and binaries (`artcraft-launcher`, `artcraft-launcher-*`), file names, and the app id `ai.storyteller.artcraft-launcher`.

## Errors and tests

- Errors use each crate's error type and `?`. Production crate roots (`lib.rs`, each binary's `main.rs`, `xtask`) carry `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]`.
- Release parsing and asset picking in `core` are tested with real GitHub release JSON fixtures, small and committed under `crates/core/tests/fixtures/`.

## Look and feel

The UI follows the apps page on getartcraft.com (`artcraft-website-next` in `artcraft-services`): square corners, 1 px hairlines instead of shadows, Archivo (620, 118 % width) for headings and app names, Inter for body text, upper-case Geist Mono labels and buttons, and the site's colour tokens by name (`theme::Tokens`). Widgets live in `widgets.rs`.

Animations follow the OS: with Windows' "Animation effects" off, macOS' "Reduce motion" on, a Linux desktop's animations off or a browser's `prefers-reduced-motion`, nothing fades, wipes or glides (`Host::reduce_motion`, `crates/ui-egui/src/motion.rs`). Animations take their time from egui's `style.animation_time` or switch off on `LauncherUi::reduce_motion`. Progress bars and spinners keep moving.

## Languages

Every user-facing string goes through `crates/ui-egui/src/i18n`: `tl!("…")` for literals, `fmt` for `{placeholders}`, `tn` for counts. English in the code is the lookup key. Catalogs are the `*.tsv` files there. Product and app names (ArtCraft Launcher, Craft App Suite, PhotoCraft, ...) stay in English.

A CJK language is drawn with the system's own CJK font (`crates/ui-egui/src/cjk_font.rs`, desktop only; English where there is none). Georgian borrows the system's Georgian font the same way.

## Icons and brand assets

`ATTRIBUTION.md` has a row per bundled asset: path, title, author, source, licence. The ArtCraft name, logo and "A" mark in `docs/brand/` (the header logo and the tray icon) are trademarks, not MIT/Apache (`docs/brand/LICENSE-brand.txt`). The app icon and the tray icon are renders of the supplied "A" mark. `packaging/icons.sh` (needs `resvg`; runs on any OS) regenerates them. `assets/app-icons/` is copied from each app's repo (`assets/app-icon/hicolor/128x128/apps/ai.storyteller.<app>.png`).
