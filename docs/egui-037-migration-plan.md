# egui 0.37 migration plan for ArtCraft Launcher

Current review candidate (2026-10-25): reconciles main `f0cb3b0439d15cae02d806e5ddc0132fd487c89c`. Managed run `egui037-launcher-review-ci-20261025-0816` passed all four CI stages including WASM: 442 passing test executions, zero failures and five ignored across 22 summaries. Unicode credential redaction uses byte boundaries; regression run 0821 reproduced the previous panic and run 0823 passed on the restored fixed source. External-link buttons share the validated opener, and terminal text sanitizes C1 controls and Unicode line separators. Independent review covered the reconciliation and fixes. Old v0.7 single-instance interoperability remains unqualified; quit the existing tray process before upgrading. Current-source native/browser/package and paired performance acceptance remain open. Publication requires explicit approval; human review and human merge are required.

The qualification records below describe earlier source snapshots.

Status (2026-10-24): the local migration incorporates frozen main
`c0fa1c69f452bab61ec3fd806646ef4daff9912a` and pins egui
`6d518fc799681571963842f95b55a6b68431b15a` and winit
`a517b3846ee8457d84ee4975110bfda6797c44dc`. The regenerated lockfile passed the
source audit and all four stages of `egui037-launcher-final-ci-20261024-2027`:
429 tests passed, none failed and five were ignored. Packaging shellcheck/actionlint
also passed. The current native Linux workflow passed Settings, resize, theme,
restart persistence and normal quit on private Wayland/software Vulkan.
The earlier action-dispatch mutation tests remain evidence for the retained implementation.
The local merge preserves both migration history and the reviewed upstream changes;
final code and dependency bytes match the passing CI. Subsequent status edits are prose only.
The external dependency override is no longer required for the canonical pins and lock.
Native CJK rendering, other platforms, real screen readers and before/after performance
remain unqualified. Human review and approval are required before app publication.

Sections 1–11 below began as research and planning on baseline
`9419d8977bc70f80373da8558356e4ba7387c7b2`. They are retained as historical research and migration
guidance, not as a claim that the current tree is unchanged or that a build/test/GUI run happened
at that baseline. The status above and the dated implementation amendments at the end supersede
their stale status and decision statements. F1's dependency-publication gate is now closed by the canonical pins and generated lock;
the original plan's emoji-font decision was revised on 2026-10-24.

This plan covers only `storytold/craft-launcher` (the ArtCraft Launcher repository). It records the
app-specific migration, not a generic egui compatibility layer. `craft-libs`, the individual Craft
apps, the egui fork and the winit fork each keep their own plans.

## 1. Baseline, provenance and method

### 1.1 Exact source baseline

| Item | Value |
| --- | --- |
| Repository | `/home/codex/work/CraftApps/egui037-migration-20261022/craft-launcher` |
| Branch | `codex/egui-037-plan-20261022` |
| Commit | `9419d8977bc70f80373da8558356e4ba7387c7b2` (`Merge pull request #756: install wrapped portable ZIP releases`) |
| Commit date | 2026-10-23 02:40:45 +0200 |
| Workspace version | 0.7.0 |
| Edition / declared MSRV | 2024 / Rust 1.90 (the real dependency floor is already higher; see §6.4) |
| `Cargo.lock` SHA-256 | `206787ae3cea71d88dbc0b3b2ff90f882a004c08eb8be72451ed57eac6f7b713` |
| `Cargo.toml` SHA-256 | `0d8e9a8b8e4a154b704c0c8d33b5e34d810c0ebcbcbebb3425fe987e1feed332` |
| Working tree | clean at inspection |

The assigned checkout is a sparse checkout: `git sparse-checkout list` contains `.github`,
`apps`, `crates`, `docs`, `packaging` and `xtask`, but not `assets/`. The source still includes
`assets/fonts/Inter-Variable.ttf`, `Archivo-Variable.ttf`, `GeistMono-Variable.ttf`, the app icons,
the social icons and `docs/brand` through `include_bytes!`/`include_image!`. A build in this exact
worktree will therefore fail. Before any implementation build, restore the missing paths (for
example `git sparse-checkout add assets`) or use a complete worktree at the same commit. This plan
read the font files through `git show HEAD:assets/...` without changing the checkout.

### 1.2 Current dependency family

`Cargo.lock` currently resolves one registry copy of each egui-family crate at 0.36.2:

| Crate | Version | Source |
| --- | --- | --- |
| `egui` | 0.36.2 | crates.io |
| `eframe` | 0.36.2 | crates.io |
| `egui-winit` | 0.36.2 | crates.io |
| `egui-wgpu` | 0.36.2 | crates.io |
| `egui_glow` | 0.36.2 | crates.io; optional `eframe` dependency, not selected by the app |
| `egui_extras` | 0.36.2 | crates.io |
| `egui_kittest` | 0.36.2 | crates.io |
| `epaint` / `emath` / `ecolor` | 0.36.2 | crates.io |
| `winit` | 0.30.13 | crates.io, checksum `a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d` |
| `accesskit` | 0.24.1 | crates.io |
| `accesskit_consumer` | 0.35.0, 0.36.0 and 0.38.0 | crates.io; pre-existing duplicates |
| `kittest` | 0.4.0 | crates.io |

There is one workspace, one root `Cargo.lock`, no `[patch.crates-io]`, no git dependency, no
submodule and no `vendor/` directory. The existing lock already has unrelated duplicate packages
(`accesskit_consumer`, `windows-sys`, `syn`, `toml`, and others); `cargo tree -d` will not be empty
and must not be used as an "empty output" gate. The gate is no new duplicate source for any
egui-family crate or `winit`.

The workspace pins in `Cargo.toml:23-26` (the workspace dependency block begins at 21) are:

```toml
egui = "0.36"
eframe = { version = "0.36", default-features = false }
egui_extras = { version = "0.36", default-features = false }
egui_kittest = "0.36"
```

`crates/ui-egui` adds `egui_extras` with `image` and `svg`, and `egui_kittest` with `wgpu` and
`snapshot` (`crates/ui-egui/Cargo.toml:15-16,36`). The native app enables
`default_fonts`, `wgpu`, `accesskit`, `wayland`, `x11`, `persistence` and `links` on eframe
(`apps/artcraft-launcher/Cargo.toml:23`). The web app enables
`default_fonts`, `wgpu`, `accesskit` and `persistence` (`apps/artcraft-launcher-web/Cargo.toml:25`).

### 1.3 Target revisions and research sources

| Item | Revision / tag | Use here |
| --- | --- | --- |
| Upstream egui 0.37.0 | annotated tag `0.37.0`; peeled commit `841a328ba2dd8f6996beb7189ebb7fc00e668a2a` | API and release-note source |
| Candidate egui comparison input | `aiongg/egui` `craft/0.37` = `a4f9a17f64a6bae61350fb85804c9df0d9865916` | candidate used for source comparison only; not a production pin |
| Candidate winit comparison input | `aiongg/winit` `craft/0.30.13-lang` = `8b35388bd719e8cc86a95971ba0bc026c9a8f058` | candidate used for source comparison only; not a production pin |
| storytold winit base | `craft/0.30.13` = `ffae5198770d7730e718043f035a772b7dbc3b1b`; winit tag `v0.30.13` = `e9809ef5` | provenance |
| Craft-libs reconciled base | `3832c47ef68a0e6214f2c1e85f3526b6adcb686e` | merged PR 199; preserve later shared-library fixes |
| Craft-libs PR 199 | head `b68de8c5f4f1c060904d211a7b1320aa0f7bce78`, audit merge `ec8840af8c5fda695cf195cdd05e5ae078415c07`, API-port ancestor `8614e4a04fae516fba16c22b31ff1008cb6be10b` | historical stale/unsafe trees; superseded by the reconciled merged base `3832c47`; not a dependency of this repo |
| Final production egui/winit/craft-libs pins | unknown until the reviewed storytold revisions are approved and published | production manifests use only those immutable full revisions |
| Language work | craft-libs `#198`, represented in the fork by E5/E12-E21, plus new E21b/P7b | suite alignment decision |

Sources inspected read-only (no checkout was switched):

- `/home/codex/work/CraftApps/egui037-migration-20261022/PLANNING-CONTRACT.md`
- `git -C /home/codex/work/CraftApps/egui037-migration-20261022/egui show 0.37.0:PATH` and
  `git show aiongg/craft/0.37:PATH`
- the 0.37.0 `CHANGELOG.md` and the per-crate changelogs
- `PATCHES.md` from `aiongg/craft/0.37`
- `/home/codex/work/ssd2/task-evidence/egui037-migration-20261022/upstream-discussions.json`
  (upstream issue snapshots; reported measurements, not this task's measurements)
- `/home/codex/work/ssd2/task-evidence/egui037-migration-20261022/lifecycle-source-audit.md`
  (authoritative native/web lifecycle evidence)
- `/home/codex/work/ssd2/task-evidence/egui037-migration-20261022/ime-surrounding-gap.md`
  (authoritative E21b/W27 outgoing-surrounding-text gap inventory)
- historical, non-acceptance evidence:
  `/home/codex/work/ssd2/task-evidence/egui037-audit-20261022/{fork,stock}/craft-launcher.errors`,
  `/home/codex/work/ssd2/task-evidence/craft-fork-egui037-20261021/RESULTS.md`,
  `/home/codex/work/ssd2/task-evidence/craft-fork-ai-lines-20261022/SURVEY.md`
- the sibling plan `/home/codex/work/CraftApps/egui037-migration-20261022/craft-libs/docs/egui-037-migration-plan.md`

The historical compiler errors are clues only. They contain two `motion.rs` errors about
`Event::MouseWheel` missing `source`; the current source has moved on, and no historical log is
acceptance evidence for this plan or for a future implementation.

## 2. Inventory and targets

### 2.1 Packages, binaries and features

| Package | Kind | egui-facing? | Notes |
| --- | --- | --- | --- |
| `artcraft-launcher-core` (`crates/core`) | library | no | pure, wasm-safe; release JSON parsing, catalog, protocol |
| `artcraft-launcher-native` (`crates/native`) | library | no | desktop backend, installer, HTTP, self-update, tray helpers; `local-mirror` feature |
| `artcraft-launcher-ui-egui` (`crates/ui-egui`) | library | yes | shared UI used by both apps; own `#![deny(...)]` panic lints |
| `artcraft-launcher` (`apps/artcraft-launcher`) | binary | yes | eframe shell, tray, window, notifications, settings file |
| `artcraft-launcher-web` (`apps/artcraft-launcher-web`) | binary | yes (wasm only) | trunk build; browser catalog/downloads |
| `xtask` | binary | no | `ci`, `wasm`, `version`, icon, local mirror |

Platform targets that the repository packages or checks:

- Linux x86_64 and aarch64: AppImage, deb, rpm, tar.gz, Flatpak.
- Linux riscv64: deb, rpm, tar.gz.
- Windows x64, x86, arm64: MSI and portable ZIP.
- macOS universal DMG.
- FreeBSD x86_64 tarball.
- Web `artcraft-launcher-web-<version>.zip`; wasm32-unknown-unknown.

### 2.2 Entry points

| Path | Anchor | Role |
| --- | --- | --- |
| `apps/artcraft-launcher/src/main.rs` | `fn main` at 36; `eframe::run_native` at 118; window loop 114-142 | native process, single instance, tray loop, window recreation |
| `apps/artcraft-launcher/src/main.rs` | `native_options` at 144 | viewport, decorations, persistence path, wgpu options |
| `apps/artcraft-launcher/src/main.rs` | `impl eframe::App for Shell` at 715; `logic` 716; `ui` 746; `raw_input_hook` 763; `save` 769; `clear_color` 773 | native eframe entry points |
| `apps/artcraft-launcher-web/src/web.rs` | `start()` at 68; `WebRunner` at 78; `impl eframe::App for WebApp` at 112 | wasm entry point |
| `crates/ui-egui/src/app.rs` | `LauncherUi::new` 177; `attach` 227; `raw_input` 239; `logic` 315; `handle` 360; `ui` 564 | shared UI lifecycle |
| `xtask/src/main.rs` | `cmd_ci`, `cmd_wasm` | repository gate |

There are no examples, benches, nested workspaces, build scripts for egui, or alternate eframe
shells beyond the native and web binaries.

### 2.3 Current lifecycle paths

- **Visible frame**: native `EpiIntegration::update` and web `AppRunner::logic` call
  `app.logic` then `app.ui` **inside** the same `Context::run_ui` closure, once per layout pass.
  Here that is `Shell::logic` -> `Launcher::tick` -> `LauncherUi::logic` (backend poll, scheduled
  checks, auto-update, restart request) followed by `Shell::ui` -> `LauncherUi::ui` (panels,
  toolbar, grid/table, drawer, dialogs, toasts, title-bar edges). `App::logic` can therefore run
  twice in one visible frame after `request_discard`.
- **Hidden/minimized window**: `Context::run_logic` calls `App::logic` without events layout or
  fresh input time; `LauncherUi::ui` does not normally run. `ViewportInfo::visible() == Some(false)`
  is not proof of a logic-only tick because native integration may request a hidden UI pass
  (`RequestPaintWhileHidden`) for capture or input. The tray-only path is different again: when
  the window closes to the tray, the process leaves `run_native` and `Launcher::wait_in_tray`
  calls `ctx.run_logic(&no_window(), ...)` (`main.rs:608-620`).
- **Tray and single instance**: `Launcher::tick` drains `starts` and `TrayAction`s (`main.rs:565`),
  pushes `Request::Show/Toggle/Quit`, and updates the tray badge.
- **UI commands**: install, update, uninstall, launch, pause, cancel, reveal, check, configure and
  fetch-pictures go directly through `LauncherUi::send` -> `Backend::send` from both `logic` and
  `ui` call sites. This is the main multi-pass risk.
- **Persistence**: eframe calls `Shell::save` (`main.rs:769`) for the `settings` key; while no
  window exists, `wait_in_tray` writes the same key through `settings_file::save` (`main.rs:620`).
- **Input**: `Shell::raw_input_hook` applies the Linux portal theme and
  `LauncherUi::raw_input`; the only raw rewrite is `motion::unsmooth_wheel`.
- **Fonts**: `LauncherUi::new`/`attach` call `theme::install_fonts` and
  `egui_extras::install_image_loaders`; `ui` installs a system CJK font when a CJK language is
  selected and returns for one frame through `fonts_ready`.
- **Window show/hide/close**: `Shell::show`, `put_away`, `quit`, and `on_close`
  (`main.rs:354`, `666-713`) send `Visible`, `Minimized`, `Focus`, `CancelClose`, `Close`.
- **Screenshots**: the app's `Event::Screenshot` is its own protocol event for downloaded app
  pictures (`crates/core/src/protocol.rs:226`; emitted at `crates/native/src/backend.rs:958` and
  `apps/artcraft-launcher-web/src/web.rs:168`). It is **not** egui's
  `ViewportCommand::Screenshot`. The launcher has no egui screenshot command.

### 2.4 Current tests and CI

Source-declared `#[test]` counts at the baseline (not proof of execution; the five rows sum to 438):

| Area | Count |
| --- | --- |
| `crates/core` | 44 |
| `crates/native` | 214 |
| `crates/ui-egui` | 119 (62 in `tests/ui.rs`, 9 language binaries, 31 in `i18n/tests.rs`, 6 motion, 5 CJK, and others) |
| `apps/artcraft-launcher` | 45 |
| `xtask` | 16 |
| workspace total | 438 |

Existing UI tests drive `LauncherUi` through `egui_kittest::Harness` with fake backends
(`crates/ui-egui/tests/ui.rs`). They assert labels, commands, geometry, focus, scroll, dialogs,
details pictures and window commands. `snapshot()` writes a PNG only when
`ARTCRAFT_LAUNCHER_SNAPSHOTS` is set; there are no committed pixel goldens. The two documentation
screenshots in `docs/screenshots/` are not test fixtures.

CI (`.github/workflows/ci.yml`) runs on Ubuntu and Windows:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo xtask wasm
cargo clippy -p artcraft-launcher-core -p artcraft-launcher-web \
  --target wasm32-unknown-unknown --locked -- -D warnings
packaging/web/package.sh
```

`cargo xtask ci` (`xtask/src/main.rs:87`) runs fmt, clippy, test and wasm without `--locked`.
FreeBSD, Nix, Windows ARM64 packaging and release workflows are separate; macOS runtime is not in
PR CI.

`packaging/web/package.sh:27-41` enforces a 24 MiB wasm limit (Cloudflare's 25 MiB cap).

## 3. What actually changes for this app in egui 0.37

The suite already uses `eframe::App::logic` + `App::ui(&mut egui::Ui, ...)` and root panels on
0.36.2 (the 0.36.2 `eframe::App` trait already has this shape). The migration must **not** rewrite
that split or claim 0.37 introduced it. The real changes here are the following.

### 3.1 Hard compile breaks

| Change | Affected source | Required correction |
| --- | --- | --- |
| `Event::MouseWheel` gained `source: MouseWheelSource` (`crates/egui/src/data/input/event.rs` 0.37.0; upstream #8665) | `crates/ui-egui/src/motion.rs:30-55` destructures/constructs wheel events; test wheel helper at `:69`; tests from `:80`, including a direct `Event::MouseWheel` at `:132` | Match `source`, copy it to every rewritten step, and add a source-preservation test. Do not invent a source: the original event's value is authoritative; `Unknown` only for a deliberately synthetic event. |
| `FontData::font` changed from `Cow<'static, [u8]>` to `Blob = Arc<dyn AsRef<[u8]> + Send + Sync>` (`crates/epaint/src/text/font_data.rs` 0.37.0) | `crates/ui-egui/src/cjk_font.rs:350` calls `FontRef::from_index(&font.font, font.index)` | Use the public `font.bytes()` (or `font.font.as_ref().as_ref()` explicitly). Keep the `font.index` assertion. |
| `WidgetType` is now a deprecated alias of `accesskit::Role` (0.37.0 `egui/src/lib.rs:642-659`; #8555) | 22 `WidgetInfo::*` call sites in `widgets.rs`, `details.rs`, `views.rs`, `bar.rs` | Use `egui::Role::{Label, Button, Link, Image, ProgressIndicator, ...}`. The current names map one-to-one except the documented aliases (`TextEdit` -> `TextInput`, `DragValue` -> `SpinButton`, `Checkbox` -> `CheckBox`, `Other` -> `Unknown`), none of which the launcher currently uses. |

`Id::new` and `AsId` are deprecated (warnings under `-D warnings`), but **`From<&str>`/`From<String>
for Id` were removed** in 0.37: there is no implicit string-to-`Id` conversion. Any `Area::new("id")`
style construction is a hard error and must use `Id::unique("id")`/`Id::new` explicitly; do not
treat this as warning-only. `Ui::id()` is renamed `scope_id`. There are 15 `egui::Id::new` root IDs
and 7 `ui.id()` calls in production UI source. `Id::unique` hashes the same source with the same
seeds, so existing widget/scroll/drawer state is intended to remain stable; verify with ID tests
rather than assuming.

`Panel::top/bottom/left/right` now take `impl AsIdSalt`; `&str` still implements it
(`id_salt.rs:9`), so `Panel::top("header")` and `Panel::bottom("footer")` need no source change.
`CentralPanel` must remain last.

### 3.2 Feature and default changes

- **`default_fonts` is no longer an egui default feature** (#8583). `eframe` still maps its own
  `default_fonts` feature to `egui/default_fonts`, and both app manifests enable that eframe
  feature. The standalone `artcraft-launcher-ui-egui` test build does not use eframe, so it must
  add `egui/default_fonts` explicitly to its dev dependency (or workspace feature set) or
  `FontDefinitions::default()` will be empty and most labels will have no glyphs.
- **`monochrome_emoji_fonts` is separate** from `default_fonts`. 0.36's
  `FontDefinitions::default()` inserted `NotoEmoji-Regular` and `emoji-icon-font` into its fallback
  chain, and `emoji-icon-font` covers U+2699 and U+27F3; the launcher therefore rendered the
  Settings gear `⚙` (`views.rs:83`) and refresh symbol `⟳` (`bar.rs:81`) through the default emoji
  fallback. 0.37 gates those fonts behind the `monochrome_emoji_fonts` feature, and its default
  `egui-icons.ttf` contains only five private-use icons, not these two code points. This is a
  **new 0.36→0.37 visual regression**, not pre-existing tofu: without the feature the glyphs
  disappear and kittest's `MissingGlyphPolicy::Panic` fails the first header/bar frame. The
  2026-10-23 planning decision to draw both with the painter and skip the emoji fonts is
  superseded for dynamic text by the 2026-10-24 amendment: release notes, feed notices and alerts
  are arbitrary network text and need the same emoji coverage 0.36 supplied. **Current decision:
  enable `monochrome_emoji_fonts` on native and web (about 1 MiB), keep the painter-drawn gear and
  refresh controls, and add an attributed font row plus a representative dynamic-text glyph
  fixture.** Do not enable `system_fonts` or bundle a CJK face for this.
- **`eframe`'s `clipboard` feature is now optional** (#8392). The native app builds eframe with
  `default-features = false` and does not currently enable `clipboard`; on 0.37 that yields the
  simulated in-app clipboard, not the OS clipboard. Add `clipboard` to
  `apps/artcraft-launcher/Cargo.toml` and test copy/paste and PRIMARY.
- **`system_fonts` is a new eframe default feature**, but this workspace disables eframe defaults
  and does not enable it. `NativeOptions::system_font_fallback` defaults to true; with the feature
  off, eframe installs a `MissingSystemFontsWarning` provider. Set `system_font_fallback: false`
  explicitly in `native_options` to preserve the app's deliberate font stack and avoid changing
  CJK/emoji discovery. Enabling `system_fonts` would change language availability, add
  `fontique`/`font-types`/`harfrust` dependencies and an asynchronous font enumeration cost; it is
  a separate product decision, not part of this migration.
- **`eframe`'s web build now uses eframe's Canvas glyph rasterizer** (#8490/#8545). It can draw
  glyphs that the bundled fonts lack, including color emoji. The launcher still forces English
  for CJK languages through `theme::cjk_font_available` (false on wasm), so this must not silently
  turn into a web-only CJK feature. Keep the existing English fallback for this migration and add
  a test that the web catalog stays English where the native UI does.

### 3.3 Egui-kittest and test-harness changes

- Default `MissingGlyphPolicy::Panic` means a missing glyph fails a test instead of drawing tofu.
  This is desirable. The `⚙`/`⟳` case is a new 0.37 missing-glyph regression (0.36 rendered them
  through the default emoji fallback), not a pre-existing latent failure. Do not call
  `allow_missing_glyphs()` globally; draw the glyphs or scope an exception to a deliberate
  negative fixture.
- Default `check_accessibility` is true: every visible input widget must have an accessible name.
  The current `ComboBox::from_id_salt` call sites (`views.rs:238`, `settings_dialog.rs:124,149,197,226`)
  have no label association, and `MenuButton::from_button` uses only the visual `…`. They must be
  named. `TextEdit` hint text counts as a placeholder name, but the search field and install
  folder should still get explicit names/labels.
- `HarnessBuilder::with_render_cursor` defaults to true. `Harness::render()` now paints a synthetic
  cursor over the frame. Any pixel golden or saved screenshot must set
  `.with_render_cursor(false)` (the current `snapshot()` helper does not).
- `Harness::render()` returns `Result<image::RgbaImage, String>` in 0.37, not a bare
  `image::RgbaImage`. Golden helpers must propagate or handle the error; do not carry over an
  `unwrap` from the 0.36 helper.
- `HarnessBuilder::with_context_setup` exists so fonts, theme and image loaders are installed
  before the harness's first pass. The current harness constructs `LauncherUi` inside the first
  `build_ui_state` closure, after the root `Ui` has taken its initial style. Use `with_context_setup`
  to install fonts/theme/loaders first, or the first table layout can keep default style metrics.
- `with_context_setup`, `with_render_cursor`, `unnamed_widgets` and `allow_missing_glyphs` do not
  exist in 0.36.2 `egui_kittest` 0.4. A single L0 harness source cannot use them and still compile
  on 0.36.2. Split the fixture into a small version-neutral measurement core plus
  baseline/candidate harness modules behind `cfg`/feature gates (or a compile-time shim), and make
  the 0.36.2 compile check part of L0 acceptance before its baseline numbers are trusted.
- The automatic accessibility check runs after settling. A probe that inspects
  `harness.unnamed_widgets()` must use a dedicated harness built with
  `.with_accessibility_check(false)`, because a named-widget violation otherwise panics before the
  assertion runs. Keep a separate strict harness (check enabled) for release-acceptance tests.
  This is a scoped inspection exception, not a global disable.
- `egui_kittest` 0.37 raises `kittest` to 0.5 and `accesskit` to 0.25. The existing queries
  (`get_by_label`, `get_by_role_and_label`, `query_all_by_role`, `accesskit_node`, `rect`,
  `value`, `click_accesskit`, `type_text`, `scroll_to_me`) need to be recompiled and adapted
  where 0.5 renamed anything. This is test-only work, but it is part of the acceptance gate.

### 3.4 Behaviour/default changes to pin or verify

- Style defaults are roomier: `Spacing::button_padding` 4x1 -> 8x2,
  `interact_size` y 18 -> 20, and widget corner radii 2/3 -> 4/6 (#8617, #8445). `theme::visuals`
  already overrides widget fills/strokes/radius (`theme.rs:169-204`) and `theme::apply` overrides
  `button_padding`, `item_spacing`, `interact_size.y`, `menu_margin` and `window_margin`
  (`theme.rs:206-232`). It does not pin `indent`, `combo_width`, `slider_width`, `text_edit`
  margins or every built-in widget default. Add geometry tests for the ComboBoxes, search field,
  install folder field, table header/rows and collapsible notes; if a 0.37 default moves them,
  pin the specific token in `theme.rs` rather than accepting a new golden.
- `WindowSettings` changed schema in 0.37 (#8687): the old flat
  `inner_position_pixels/outer_position_pixels/inner_size_points` becomes
  `normal_geometry { outer_position_pixels, scale_factor, inner_size_points }`. The upstream commit
  explicitly says old saved window settings are ignored once. `settings` and other keys survive
  because `Settings` has `#[serde(default)]` and `settings_file::RonFile` preserves unknown keys,
  but the window will open at its default size/position once. **Decision for this migration:
  accept the one-time window-geometry reset** rather than writing a migration that would have to
  guess the old scale factor; test that app settings are preserved and that the new window key is
  written on the next save. A product decision to preserve position would require a separate,
  reviewed migration.
- `egui` persisted memory (`egui` key) may also be ignored once if its schema changed. This must
  not panic or drop settings; test old-key loading.
- `Context::root_ui` panics if called while the root `Ui` is borrowed (#8535). The app currently
  uses `run_ui`/App::ui and passes the root `Ui` into panels; overlays use `Area`/`Modal` with a
  `Context`, not `Context::root_ui`, so it is safe. Add an explicit invariant test and a source
  check that no `root_ui(` call exists in `crates/ui-egui` or `apps/artcraft-launcher`.
- `Context::run_ui` can call its closure more than once after `request_discard`; `Context::run_logic`
  leaves events/time stale and performs no layout. 0.37 calls `request_discard` at least from
  `Resize` (`containers/resize.rs:286`), the first `Aligned` sizing pass (`containers/aligned.rs:95`)
  and the first `Grid` pass (`grid.rs:468`). The launcher source currently contains no `Resize`,
  `Aligned` or `Grid`; its table uses `egui_extras::TableBuilder` (`views.rs:522,541`). The
  single-pass observation is therefore a property of the current widget tree, to be confirmed by
  instrumentation or a source scan on the implemented tree, not a general 0.37 property. The
  action design in §4.2 makes repeated passes safe regardless.
- After a normally completed `run_ui`, `current_pass_index()` is zero even after multiple passes
  (`end_pass` takes the output and increments only the returned copy); a first-pass guard is not
  evidence about hidden work. See §4.2.

### 3.5 Upstream regressions that affect this app

These are reported upstream measurements, not this task's results:

- **#8731**: plain-text tessellation is reported ~61% slower in 0.37.0 because of the color-glyph
  check inside `tessellate_text`'s per-vertex closure. The launcher is text-heavy (grid cards,
  table rows, labels). The inspected candidate `a4f9a17f` contains the 0.37 code, not the proposed
  per-row branch fix; the final storytold revision must state whether it carries a reviewed fix.
  This is a likely performance-gate failure, not something a 0.37 API rename can hide.
- **#8736**: the new `wrap_texel_coord` in `egui.wgsl` makes the DX12/FXC pipeline ~4× slower to
  compile, reported as ~+40 ms first-frame startup on Windows. The launcher deliberately pins FXC
  (`main.rs:225`), so it is directly affected. The inspected candidate contains the 0.37 shader,
  not the proposed float/select fix; the final storytold revision must state its resolution.
- **#8763 / PR #8764**: clicking outside a `TextEdit` can collapse its selection before the clicked
  button runs. #8764 is an open, unmerged proposed fix; the separate 2026-10-23 snapshot verifies its head. The launcher's
  search and install-folder fields do not have selection-based toolbar actions, but selection
  followed by a toolbar/context click is still a required regression test (the intended range is
  used by the action, and pointer and keyboard paths agree).
- **#8512** system-font startup cost, **#8657** Wayland clipboard/PRIMARY, **#8577** bidi
  corrections, **#8748** hidden-parent deferred children and **#8428** PRIMARY are relevant to the
  suite. #8512 is avoided by not enabling `system_fonts` (still measure first-missing-glyph
  lookup); #8657 is covered by E4 regular clipboard plus W15P PRIMARY; #8577 by E5/E12/E16;
  #8428 by the clipboard feature and W15P. #8748 was **closed unmerged**: do not describe its
  draft behavior as a release fix; the launcher has no deferred child viewports, so test only its
  own hidden/occluded root lifecycle.

Both #8731 and #8736 remain open at inspection. The migration cannot claim performance acceptance
until they are measured and either fixed in the shared fork or explicitly waived by the user with
the profile attached. See §8.

## 4. Migration design

### 4.1 Root `Ui`, panels and overlays

The root `Ui` path is already correct and should stay:

- `Shell::ui` receives the root `Ui` from eframe and passes it to `LauncherUi::ui`
  (`main.rs:746-749`); the web app does the same (`web.rs:117-119`).
- `LauncherUi::ui` adds `Panel::top("header")` (`app.rs:604`), then
  `Panel::bottom("footer")` (`app.rs:609`), then `CentralPanel::default()` (`app.rs:614`) last.
  Keep this order. Do not create a top-level panel inside another panel.
- `details_drawer`, `dialogs` and `draw_toasts` are overlays in `Area`/`Modal`, after the panels.
  They take `&Context` intentionally. They must not call `Context::root_ui`; they do not today.
  Add `.role(Role::Dialog)` / accessible names to the details drawer and lightbox, and
  `.role(Role::Alert)` to the toast area if that role is available in the pinned accesskit.
- There is no left/right panel, no docking, no popout viewport, no `show_viewport_deferred`, and
  no `ViewportHooks` use. `E1` must compile in the dependency but is not integrated here. If a
  future launcher version adopts craft-ui `DockArea`, it uses the explicit opt-in
  `DockArea::direction` policy: saved physical splits, rectangles and logical tab order stay
  unchanged, and paint, hit tests, drag insertion, accessibility order and keyboard traversal share
  one geometry calculation. No app may reverse saved panel order to simulate RTL, and display-bidi
  text must never be fed back into shaping, persistence or clipboard payloads.
- The panel frames use explicit heights (`HEADER_H = 48`, `FOOTER_H = 34`). 0.37's clip fix (#8562)
  should keep the central child clip inside the parent; assert it.

### 4.2 One action boundary for UI commands and multi-pass safety

**Problem.** `LauncherUi::send` writes a `Backend::Command` immediately. It is called from logic,
from dialogs, from card/table actions, from the toolbar, from details and from the update/launch
paths. If `App::ui` is invoked twice in one frame after `request_discard`, a click that is still
present in the same input state can enqueue the same install/uninstall/launch twice. `launch_app`
also increments the `launches` counter every time it runs.

**Design.** Keep `LauncherUi::logic` as the owner of direct backend traffic where it already is
(configure, scheduled checks, auto-updates, poll). Add an explicit UI-action boundary:

```rust
enum UiAction {
    Command(Command),
    Launch(String),       // Command::Launch plus one launch-count increment
    Toast { text: String, error: bool }, // created by a UI interaction
}
```

Store `pending: Vec<UiAction>`, `frame: Option<u64>` and `flushed: Vec<UiAction>` in
`LauncherUi`. `cumulative_frame_nr()` increments once after a completed `run_ui`, not per pass, so
it distinguishes user-visible frames while keeping dedup across the passes of one frame. On every
`ui` call:

1. Read `ctx.cumulative_frame_nr()`. When it differs from `frame`, clear `flushed` and set `frame`.
2. UI handlers call `act(action)`, which only queues. The flush counts equal actions in draw
   order (the first Install in a pass is the first Install of the frame, the second is the second),
   so a repeated pass skips the ranks already sent while two distinct intents with an equal payload
   still both send. `Command` is `PartialEq` (`crates/core/src/protocol.rs:16`); `UiAction` derives
   `PartialEq` for this comparison.
3. At the end of every `ui` pass, including an early return such as the `fonts_ready` pass, drain
   `pending`, perform the side effect once, and append it to `flushed`. A second pass with the same
   click sees the action in `flushed` and does nothing; a genuinely new action in the second pass
   is still sent.
4. `Launch` increments `settings.launches` exactly once, in the flush, not in the click handler.
5. UI-created toasts go through the same guard; logic-created toasts use the pending-toast queue in
   §4.3.

This is deliberately **not** a global first-pass guard. `App::logic` runs inside every `run_ui`
pass and may run twice in one visible frame; after a completed `run_ui` the pass index is zero, and
visibility does not distinguish a logic-only tick from a forced hidden UI pass. A narrow
first-pass guard can only gate genuinely once-per-layout-frame work, with a regression proving
normal hidden ticks still run; it must never suppress fresh async results, control commands or
widget actions merely because a pass index is nonzero.

Change these UI call sites to `act`:

- `views.rs`: refresh/check, update-all, card/table actions, pause/cancel/reveal, launch, and both
  launcher-update banner actions (`Command::Cancel` at `views.rs:140`, `Command::UpdateLauncher`
  at `views.rs:153`). `ctx.open_url` calls stay as egui output commands, not backend actions.
- `dialogs.rs`: update launcher, uninstall, replace, install and dialog close side effects.
  `configure` may stay direct because `sent_options` makes it idempotent across passes.
- `details.rs`: `request_pictures` / `FetchScreenshots`.
- `app.rs`: `install_app`, `launch_app`, `check_now` when called from a UI button.

`egui::OutputCommand::OpenUrl` stays where it is (the shell filters the Discord deep link at
`main.rs:750-761`); it is an egui output, not a backend command. `configure` already deduplicates
through `sent_options`; keep that dedup and do not send a duplicate `Configure` from a repeated
pass. `trigger_auto_updates` already deduplicates through `auto_update_attempted`; keep it.

**Tests.** Drive two passes with `ctx.request_discard("test")` and assert:

- click Update/Install/Launch in pass 1: exactly one command and one launch-count increment;
- click the same action again in pass 2: still one command, no second increment;
- advance to the next `run_ui`/`cumulative_frame_nr` and click the same action again: a second
  command and a second launch increment arrive (proves `flushed` is cleared);
- two distinct actions in pass 2: both delivered;
- a pending action followed by the `fonts_ready` early return: delivered exactly once, not lost;
- a launcher-update banner Cancel/UpdateLauncher click: exactly once through the boundary.

The same tests are the mutation probes for removing the boundary or the per-frame reset.

### 4.3 Logic-only iterations, hidden windows and toasts

`LauncherUi::logic` already does the right things while hidden: configures the backend, sends the
startup check once, polls events, detects focus/installs on a schedule, auto-updates, requests a
quit, and asks for a repaint in 60 s. Keep those in `logic`, not `ui`. It also runs once per
`run_ui` pass on both native and web, so every logic side effect must be driven by a consumed queue,
revision/generation identity or a monotonic deadline, not by an assumption that it runs once per
physical frame. Do not use `viewport().visible()` to infer the phase.

Two corrections belong to this migration because `run_logic` explicitly leaves input time stale:

1. **Toast timing.** `LauncherUi::toast` reads `ctx.input(|i| i.time)` (`app.rs:269`). When
   `run_logic` delivers a `JobFinished`, an update notice, or another event while the window is
   hidden, that time is from the last painted frame: a queued toast can be expired before the
   window is ever shown, or persist too long. Store UI toasts as pending `(text, error)` and set
   `until = ctx.input(time) + duration` only when the next `ui` displays them. Cap both the
   pending queue and the visible list at four.
2. **No UI input polling in `logic`.** Audit and keep the only `ctx.input` read in logic to
   `i.viewport().focused` (`app.rs:322`), which `run_logic` does populate. Do not read events,
   pointer state, key state or `i.time` for logic decisions. The backend clock (`Backend::now`)
   remains the time source for scheduled checks.

**Tests.** With `Context::run_logic` and a fake backend, deliver a `JobFinished` while hidden:
state changes, no panic, no toast visible yet. Then run `run_ui` once: the toast is visible with a
fresh expiry. Add the lifecycle-source-audit cases: repeated hidden ticks after a completed
multi-pass frame; a forced hidden UI pass (`RequestPaintWhileHidden`) that must handle current
input and not replay a consumed queue item; buffered input restored with the window; a background
completion between passes; and 60 logic-only ticks after completion that assert no further repaint
request from launcher state (the shell's 60 s scheduled repaint remains). This preserves the
existing tray-only behaviour, where `wait_in_tray` calls `run_logic` and saves settings when they
change. The test driver must mirror the actual native/web callback order, not a fictional
once-per-frame native host.

### 4.4 Commands, undo, save and reopen

There is **no document, undo stack, autosave or playback engine in the launcher**. Do not invent
one. The equivalent contracts are:

- **Commands**: the action boundary in §4.2 makes UI-originated install/update/uninstall/launch/
  pause/cancel/reveal/check exactly-once per user gesture. Logic-originated commands keep their
  existing `sent_options`/`auto_update_attempted` dedup. Existing tests already assert command
  sequences for install folders and updates; extend them to the multi-pass case.
- **Undo**: owned by egui's `TextEdit` for search and the install folder. Do not add an app-level
  undo stack. A test should type into the folder field, press Ctrl+Z (or Command+Z on macOS), and
  assert one text-edit step, not one backend command or one settings write.
- **Save/reopen**: `Settings` is `#[serde(default)]` and includes view, filter, sort, theme,
  language, tray/desktop/login choices, update policy, install locations, hidden apps, notices and
  launch counts (`crates/ui-egui/src/settings.rs:84-140`). `settings_file::RonFile` preserves
  unknown keys while writing the `settings` key. Add a fixture test using a 0.36-style `app.ron`
  with `window`, `egui` and `settings` keys: loading in 0.37 keeps `settings`; saving keeps the
  other keys; the old `window` value may be ignored once (§3.4); the next eframe save writes the
  new `window` schema.
- **Search** is session-only and is not in `Settings`; reopening intentionally clears it.
  **Notes zoom** is session-only. Assert this rather than accidentally persisting it.
- **Tray-only save**: `wait_in_tray` compares `self.ui.settings` with the last saved value and
  writes on change (`main.rs:620-634`). Keep this; 0.37 does not change eframe's storage path or
  key names (`window`, `egui`), only the window value schema.

### 4.5 IDs, scopes and persisted state

- Replace the 15 root `egui::Id::new(...)` calls with `Id::unique(...)` and the 7 `ui.id()` calls
  with `ui.scope_id()`. Keep the same tuple payloads so the hash is unchanged. Affected anchors:
  `widgets.rs:225` (`window-border`), `details.rs:104,131,273,399,409,422,425,460,474,477`,
  `dialogs.rs:23,48`, `app.rs:772`, `icons.rs:37`; `widgets.rs:131,174,429`,
  `views.rs:408,418,614`, `notes.rs:70`.
- `ScrollArea::id_salt`, `TextEdit::id_salt`, `CollapsingHeader::id_salt` and
  `ComboBox::from_id_salt` keep their existing strings/tuples. They now take `impl AsIdSalt`, which
  is source-compatible for these values.
- **Panel IDs are parent-salted in 0.37.** `Panel::id` uses
  `parent_ui.scope_id().with_salt(self.id_salt)`. The header/footer use
  `exact_size(48/34)` and `resizable(false)`; there is no adjustable panel preference
  to migrate. Keep the salts and assert the fixed geometry on the first and later
  frames, including an old egui memory blob. Do not add a `PanelState` migration.
  Window geometry is the separate eframe `window` entry handled in section 3.4.

- Keep global IDs global: the drawer, scrim, lightbox, placeholder, arrow and window-border IDs
  are not translated and do not depend on locale or data that can collide.
- Add an ID stability test: the same source yields the same `Id` across frames; different app IDs
  yield different card/row IDs; the details drawer and lightbox IDs differ; no duplicate-ID warning
  is emitted on a two-pass run.
- Do not use translated labels as IDs. The existing code does not; keep it that way.

### 4.6 Fonts, glyph coverage and CJK

The font stack is deliberate:

- `theme::install_fonts` (`theme.rs:298-335`) starts from `FontDefinitions::default()` and inserts
  Inter at 400/500/600, Archivo 620/118%, and Geist Mono 400/500/700 under named families. It
  appends the current default fallback stack behind each family. Keep the named families and the
  `FontTweak` coordinates; they are part of the visual identity.
- `FontTweak::subpixel_binning = Some(false)` and the variation coordinates remain supported in
  0.37. E14 (`FontTweak::fallback_align` plus shared fallback baseline/row growth) is compiled by
  the selected fork revision even though this app never sets `fallback_align`; the field's default
  is `FallbackAlign::Center`, but E14's row/baseline behavior still changes fallback metrics. Do
  not claim "not adopting E14" while pinning a revision that contains it. Keep the existing
  `y_offset_factor` nudge in `cjk_font.rs:75-80` and add a CJK/Indic fallback row-height and
  baseline comparison against 0.36.2; if E14 causes a product-visible change, either exclude it
  from the final storytold revision or accept it explicitly with before/after evidence.
- The CJK loader (`cjk_font.rs:62-95`, `theme.rs:338-377`) reads a known system font by path,
  caches it per language, and appends it after every named family. CJK languages with no font stay
  in English. Preserve that policy. Do not enable eframe `system_fonts` in this migration; its
  `fontique` discovery would make the language decision asynchronous and would change which face
  is chosen for Han characters. A future opt-in can replace the hand-written candidate list, but it
  needs its own product and performance review. Do not bundle a CJK font or add a proprietary face
  for the emoji repair.
- **Current required fix (2026-10-24):** keep `Glyph::Settings` and `Glyph::Refresh` painter-drawn
  beside `Glyph::Pause/Close`, with the accessible names `Settings` and `Check for updates`.
  Separately enable egui's `monochrome_emoji_fonts` feature in the workspace `egui` dependency so
  native and web both bundle `NotoEmoji-Regular` (OFL-1.1) and `emoji-icon-font` (MIT) through
  `epaint_default_fonts`. Those are dependency assets, not files copied into this repository; add
  their attribution rows from the fonts' own OFL/MIT files and `README` in `epaint_default_fonts`.
  This restores 0.36's monochrome emoji coverage for arbitrary release notes, feed notices and
  alerts without a system-font fallback assumption or a CJK bundle.
- Add a glyph-coverage test over every non-ASCII character the UI paints through a font:
  painter-drawn `⚙`/`⟳` are shapes, not font glyphs; `↗ ← → ‹ › · — − • … ▲ ▼ € “ ”` and
  Latin/Cyrillic catalog text must pass `fonts.has_glyphs` for the relevant family. Add a strict
  fixture built from representative dynamic release-note and feed-notice text containing emoji
  (for example 🚀 🐛 ✨ 🎉 ✅ ❌ ⚠ 💡 🔒 🎨), installed with `install_fonts(ctx, None)` and no
  system provider. Run it after one `run_ui` pass, not after `run_logic`, and assert the bundled
  font-data upper bound so an accidental system/extra font cannot make the test pass. Keep the
  deliberate negative fixture (a missing glyph under `MissingGlyphPolicy::Panic`) out of release
  acceptance.
- `FontData::bytes()` is the public accessor after the `Blob` change. Fix the CJK test that used
  `&font.font`.
- Web has no bundled CJK face and `effective_lang` must keep returning English for CJK without a
  font, even though the 0.37 Canvas rasterizer could now draw it. Test the web catalog in English
  and leave web CJK as a separate feature decision.

### 4.7 Accessibility and names

0.37's kittest fails unnamed input widgets. Fix the actual names; do not disable the check.

| Control | Required name/role | Source anchor |
| --- | --- | --- |
| Search field | `Role::TextInput`, name `Search apps` (accessible name or placeholder) | `views.rs:194` |
| Grid sort picker | `Role::ComboBox`, name `Sort`; selected text remains the visible value | `views.rs:238` |
| Settings language picker | `Role::ComboBox`, name `Language` | `settings_dialog.rs:124` |
| Check-for-updates picker | `Role::ComboBox`, name `Check for updates` | `settings_dialog.rs:149` |
| Windows install kind | `Role::ComboBox`, name `Install new apps as` | `settings_dialog.rs:197` |
| Linux install kind | `Role::ComboBox`, name `Install new apps as` | `settings_dialog.rs:226` |
| Install folder | `Role::TextInput`, already `labelled_by` the visible label | `dialogs.rs:263-272` |
| Settings gear | `Role::Button`, name `Settings` (drawn glyph) | `views.rs:83` |
| Refresh | `Role::Button`, name `Check for updates` (drawn glyph) | `bar.rs:86` |
| View toggle | `Role::Button`, selected, names from the tooltip | `bar.rs:46` |
| Card/row open | `Role::Button`, `{name} details` | `views.rs:419,616` |
| More/app menu | `Role::Button`, name `More actions` plus the app name; tooltip remains | `widgets.rs:485`, `views.rs:738` |
| Custom buttons/links/badges | current labels are good; map `WidgetType` to `Role` | `widgets.rs`, `details.rs` |
| Details drawer/lightbox | `Role::Dialog` (or `Role::Pane` where appropriate) with an app-specific name | `details.rs:131,399` |
| Toasts | `Role::Alert` if available; text remains the label | `app.rs:772` |
| Table headers | `Role::Button`, logical label, not the arrow hint | `views.rs:595` |

Implementation details:

- Make `settings_dialog::row` return the label's `Id` (or the label `Response`) so each control
  can call `.labelled_by(label_id)`; the visible label is the right name source. In 0.37
  `ComboBox::show_ui` returns `InnerResponse<Option<R>>`, not a `Response`. Use
  `.show_ui(...).response.labelled_by(label_id)` (or destructure the `InnerResponse` and use its
  `.response`), or `ComboBox::from_label`/`.accessible_name(...)` where no visible label exists.
  A literal `show_ui(...).labelled_by(...)` does not compile.
- Use `.accessible_name(tl!("Search apps"))` on the search field; the hint alone is a placeholder,
  which 0.37 accepts but which disappears from a screen reader's useful name once text is present.
- Use `Response::accessible_name` for the `…` menu button and icon-only buttons; do not rely on a
  tooltip alone. The 0.37 docs explicitly recommend `labelled_by` for visible labels and
  `accessible_name` for icon-only controls.
- Add a strict-harness test (automatic accessibility check enabled) for grid, table, Settings,
  install dialog, details drawer and lightbox, plus a scoped inspection harness with
  `.with_accessibility_check(false)` that asserts `harness.unnamed_widgets()` is empty and lets us
  report the offending names. The inspection harness is required because the automatic check
  panics before `unnamed_widgets()` can be read; it is not a global disable. Add a negative probe
  that removes the sort combo's label and shows the strict test failing, and one that asserts the
  inspection harness reports the same widget.
- Add a compile-time/UI test that the actual combo button node is named and has `Role::ComboBox`,
  not merely that some ancestor has a label.
- `Context::set_language`/`Frame::set_ui_language` (E17/E18) are fork-only. This migration does not
  call them, so the source remains buildable against stock 0.37. Native AccessKit root language
  therefore remains unset; web `detect_language()` already sets `<html lang>` (`web.rs:60-62`).
  If screen-reader language becomes a product requirement, adopt E18 in a separate stage and drop
  the stock-compatibility promise for that API.

### 4.8 Input, IME, clipboard, PRIMARY and shortcuts

- `motion::unsmooth_wheel` is the only raw-input rewrite. Preserve `MouseWheelSource` on every
  rewritten event. The rewrite must still leave non-`Move` phases, NaN/INF, and huge deltas alone.
  Add tests for `Wheel`, `Trackpad`, `Momentum` and `Unknown`; assert the total delta is unchanged
  and the source is preserved.
- `zoom_input` (`app.rs:540-562`) uses `ctx.zoom_delta()` and `egui::gui_zoom::kb_shortcuts` for
  the notes dialog. Keep the notes zoom session-only and clamped to 0.5-3.0; keep the main zoom
  clamp 0.2-5.0. Test Ctrl/Command +, -, 0 and that a focused text field does not lose typed text
  to a zoom shortcut.
- **Clipboard:** add eframe `clipboard` for native. Test copy/paste in the search and folder
  fields, including paste of multi-line text (single-line fields should flatten or reject it
  consistently). On Wayland, test regular clipboard and middle-click PRIMARY separately. E4 routes
  the regular clipboard through winit's data device; PRIMARY is **not** delegated to arboard as the
  default. The required design is the new winit **W15P**: a winit-owned PRIMARY path on the same
  connection/event queue and per-seat lifecycle, using SCTK 0.19.2
  `PrimarySelectionManagerState`/device/source/offer, without binding a second regular
  `wl_data_device`. egui E4's `get_primary_text`/`set_primary_text` use that path. Test serial
  freshness, keyboard-only focus, password exclusion/no unchanged-selection reclaim, owner/seat
  changes, empty/absent offers, self-paste, and canceled/slow transfers on wlroots/Hyprland, KWin
  and Mutter, recording the compositor and supported capabilities. A regular-clipboard pass is not
  PRIMARY acceptance. If W15P is unavailable, mark PRIMARY blocked with a measured waiver; do not
  present an arboard fallback as the default.
- **IME:** search and folder fields are the only `TextEdit`s. Test typing Japanese/Chinese with a
  real IME on Windows/Linux; kittest `type_text` is not IME evidence. Check that focus changes
  during composition do not interrupt composition (E6), that click-away does not lose the
  selection in a way the UI depends on (#8763/#8764), and that one undo step reverses an IME
  replacement (E21). The launcher has no document undo stack. **E21/W27 is incomplete as shipped:**
  candidate egui-winit never calls `set_ime_surrounding_text` and `IMEOutput` carries no logical
  text. Require the new E21b/P7b revision: a bounded transient surrounding-text payload (<=4000
  UTF-8 bytes, valid boundaries, preedit excluded and indices remapped), produced by the built-in
  `TextEdit` after delete/commit/cursor updates, sent by the egui-winit W27 adapter on
  text/selection/IME-owner/focus changes, cleared on password/no-context/no-IME/focus loss, never
  shared across widgets or windows, and appended atomically with the IME output across passes.
  This app has no custom `IMEOutput` producers, so it inherits the TextEdit producer; the suite's
  Word/Vector/Fig/Effect/Film/Design/Deck/Photo custom editors are out of scope here but must not
  be represented by `None` placeholders in their own plans. E21b/P7b must land as one atomic,
  separately reviewed compatibility stage (producer, bounded payload, adapter and every
  custom-editor handoff together), not a partial API that compiles with empty context. Test
  outgoing text after deletion and commit, deletion-only batches, supplementary/combining/RTL
  logical text, preedit exclusion, long selection/window limits, field switch and focus loss. A
  Wayland Vietnamese/predictive-IME protocol trace is native evidence, not kittest.
- **Shortcuts on non-Latin layouts:** E19 supplies per-key Latin layout characters. Test the
  header's macOS Command+M minimize, notes zoom shortcuts and any text-editing shortcut on a
  Russian/Arabic/Hebrew layout and a Dvorak-Command layout. `Key` alone is not enough. The native
  app currently has no application menu shortcuts beyond those.
- **RTL:** no catalog language is RTL. E9's `Style::right_to_left` is not used by the launcher and
  there is no DockArea; do not add an RTL layout policy in this migration. If docking is ever
  adopted, use the explicit opt-in `DockArea::direction` policy in §4.1 and keep persisted
  physical splits/logical tab order unchanged. E5/E12 still shape any mixed-script text in labels;
  add one mixed Hebrew/Arabic label fixture to ensure no panic and finite geometry. Never pass
  display-reordered (visual bidi) text back into shaping, search, save or clipboard payloads; use
  logical text there. Do not claim an RTL UI.

### 4.9 Screenshots, images, viewports and GPU resources

- **No egui screenshot command.** The app's `Event::Screenshot` is a backend event
  for downloaded app pictures with an `(app, key)` identity. It stays unchanged.
  The launcher adds no capture feature, slot or craft-ui dependency in this migration.
  Any future capture work must adopt the then-reviewed shared contract in craft-libs;
  its design and acceptance belong to that separate change, not this rollout.

- **kittest rendering:** set `.with_render_cursor(false)` for saved screenshots/goldens; otherwise
  the cursor is captured. Use `.with_context_setup` to install fonts/theme/loaders before the
  first pass. `Harness::render()` returns `Result<image::RgbaImage, String>` in 0.37; handle the
  result and compare raw RGBA, not PNG bytes.
- **Images:** details pictures use `ImageSource::Bytes` with `bytes://screenshots/{app}/{key}.jpg`
  and are validated by `core::details::screenshot_within_budget` (`details.rs:33-43,79-87`).
  Preserve the 8:5 aspect, the 2 MB/byte and pixel checks, and the placeholder geometry. The grey
  app icon cache uses `Id::unique(("grey-app-icon", id))` and a `TextureHandle` in `ctx.data`; a
  new window gets a new `Context` and new textures. Test close/reopen does not reuse a stale
  texture handle and does not re-fetch already-cached bytes.
- **Viewports:** one root viewport only. Test `ViewportId::ROOT`, show/hide/minimize/focus,
  close-to-tray and reopen. The macOS transparent full-size title bar (`native_options` 154-162)
  and traffic-light spacing are a real macOS gate; no PR CI runs macOS.
- **GPU:** `wgpu_options` selects DX12/GL on Windows and the desktop's GPU on Linux
  (`main.rs:190-280`); `pin_dx12_compiler` forces FXC unless the environment overrides it. The
  `WgpuConfiguration`/`WgpuSetup` fields used by this code are unchanged between 0.36.2 and 0.37.0
  (verified against both source trees). E2 surface fitting changes only oversized surfaces; image
  geometry is qualified through the existing rendering/viewport tests. The parent environment
  exposes an i915 render device (PCI 8086:3E96, `/sys/class/drm/renderD128`) and reportedly
  `/dev/dri/renderD128`, but this is availability evidence only: the worker sandbox may hide
  `/dev/dri`, no display/compositor is inherited, and the backend/driver is unqualified. Qualify
  the chosen adapter through managed jobs, separately from llvmpipe and the Windows Basic Render
  Driver.
- **Texture uploads:** count `textures_delta.set/free` bytes over 100 frames for grid, table,
  details and settings. No repeated upload of the same image; no unbounded galley/atlas growth.

## 5. Fork, winit and shared-work integration

### 5.1 What this repository needs from the fork

The launcher uses no fork-only API today. It benefits from these fork behaviours without source
changes:

| Patch | Relevance to ArtCraft Launcher | Decision |
| --- | --- | --- |
| E1 `ViewportHooks` | none; one root viewport | keep in dependency; compile only |
| E2+E3 surface fit / error scopes | large or multi-monitor windows | keep; measure oversized window and error path |
| E4 Wayland clipboard | search/folder copy/paste; PRIMARY | keep; regular text uses W15, PRIMARY requires new W15P on the same connection/event queue; test separately |
| E5, E12, E14, E15, E16, E20 | bidi shaping, fallback baseline/row growth, line breaks, language-aware widget text; CJK fallback | keep; E14 is compiled even without setting `fallback_align`; compare CJK/Indic rows; ASCII geometry unchanged |
| E6, E8, E13, E21, E21b | IME focus, glyph overshoot, grapheme editing, IME delete, outgoing surrounding text | keep; E21b is a new prerequisite; test search/folder fields |
| E7, E19 | letter keys on non-Latin layouts | keep; test native shortcuts |
| E9 | RTL chrome | no RTL catalog language; keep, do not enable |
| E10 | web keys/language input | keep; browser test |
| E11 | vello multithreading rasterization | launcher does not enable that feature; keep, compile only |
| E17/E18 | AccessKit/UI language | not called to preserve stock-compatible source; keep; revisit only with a product decision |

Winit: W15 (Wayland data device/clipboard) is required by E4 regular clipboard; the new **W15P**
(same-connection per-seat PRIMARY) is required by E4 PRIMARY; W25 (Latin layout) by E19; and
W26/W27 (IME delete/surrounding) by E21/E21b. W27 must actually be called by the E21b adapter;
the candidate's presence of a winit API without an egui-winit call is not integration. The other
W1-W27 fixes are suite policy and must remain in the fork revision; do not delete or re-vendor
them here. The launcher has no vendored winit/egui sources, so there is nothing to reconcile
locally.

### 5.2 Craft-libs PR 199 and language work #198

- `storytold/craft-libs#199` is the craft-libs egui 0.37 update. The launcher does **not** depend
  on `craft-libs` or `craft-ui`, so its source is not a dependency and must not be copied into
  this repository. Adopt only its dependency hygiene: full immutable revisions, every egui-family
  package patched, stock-package proof, no branch or machine-local path in the lock.
- **The PR199 head `b68de8c` and audit merge `ec8840a` are unsafe stale trees.** `b68de8c` predates
  the historical craft-libs main `4919088` and lacks `craft-platform`; `ec8840a` also has a stale
  conflicted lock. Never merge or cherry-pick either tree. The earlier hunk-only reconciliation from `8614e4a` / `1764bca` onto `4919088` is historical.
  The current implementation is reconciled on merged PR 199 `3832c47ef68a0e6214f2c1e85f3526b6adcb686e`,
  preserving newer security, settings, docking and printer fixes. This launcher does not consume those crates, but
  the suite pin and review evidence must follow the corrected base.
- #198's language work is represented in the selected candidate fork revision (E5/E12-E21), but
  the candidate is a comparison input, not the production pin. The final production revision is
  the reviewed, published storytold egui revision (including E21b) and must be selected after
  approval; no launcher source calls a fork-only API. The shared craft-libs pin is likewise its
  final reviewed storytold revision; `3832c47ef68a0e6214f2c1e85f3526b6adcb686e` is the merged base for the shared-library follow-up.
- If a future launcher version uses `craft-ui`, it enters the dependency DAG then; the current
  plan does not add a `craft-ui` dependency merely to share code.

### 5.3 Stock vs fork

Production uses the suite fork with every family member patched at final reviewed,
published storytold egui/winit revisions, including W15P, E21b and required regression fixes.
Those full SHAs are recorded once the predecessor fork work is complete. The inspected aiongg
candidates and registry 0.37 are comparison inputs only. Registry 0.37 is never an intermediate
production stage or rollback target; rollback uses the prior coherent source/lock/executable.

L1 lands the final storytold pins atomically. Preserve stock-compatible API usage where it is
appropriate, then compare both graphs in isolation: one fork contract build and one registry-0.37 contract build of the launcher
(or at least `cargo check`/tests against a registry-only isolated fixture), plus an equivalent
stock-0.37 performance control where workloads are comparable. Because the launcher is
`publish = false`, there is no crates.io package promise, but the dual build is still the cheapest
proof that no fork-only API leaked into app source. No stage may substitute a moving branch, a
candidate `aiongg` SHA or a machine-local path for the approved final revision.

## 6. Dependency, lock and packaging design

### 6.1 Workspace pins

Change the root workspace dependencies to the 0.37 family, keeping `default-features = false`
where it is already explicit:

```toml
egui = { version = "0.37", features = ["monochrome_emoji_fonts"] }
eframe = { version = "0.37", default-features = false }
egui_extras = { version = "0.37", default-features = false }
egui_kittest = "0.37"
```

`monochrome_emoji_fonts` is enabled at the workspace level so the native app, the web app and the
shared UI all agree on the same fallback chain. It adds about 1 MiB of attributed OFL/MIT font data
from `epaint_default_fonts`; it does not enable `system_fonts` or bundle CJK.

Do not assume that `egui_extras` or `egui_kittest` share the core egui version number beyond the
verified 0.37.0 release; check each manifest. Do not add `egui_plot`/`egui_dock`.

Add `egui/default_fonts` to the `crates/ui-egui` dev dependency (or explicitly to its test
feature set). Keep the native `clipboard` feature, and set `system_font_fallback: false` in
`native_options`. Keep `default_fonts` in both app feature lists.

### 6.2 Suite fork patch section

For L1, the root `Cargo.toml` gets a `[patch.crates-io]` block naming every used package at the
same **final reviewed storytold** full revision, plus winit at its final full revision. The exact
SHAs are unknown until the revised fork is reviewed, approved and published; the inspected
`aiongg` SHAs above are comparison inputs only and must not be committed as production pins:

```toml
# Fill with the approved final storytold revisions before L1; candidate SHAs are comparison-only.
[patch.crates-io]
egui = { git = "https://github.com/storytold/egui", rev = "<FINAL_EGUI_REV>" }
eframe = { git = "https://github.com/storytold/egui", rev = "<FINAL_EGUI_REV>" }
egui-winit = { git = "https://github.com/storytold/egui", rev = "<FINAL_EGUI_REV>" }
egui-wgpu = { git = "https://github.com/storytold/egui", rev = "<FINAL_EGUI_REV>" }
egui_glow = { git = "https://github.com/storytold/egui", rev = "<FINAL_EGUI_REV>" }
egui_extras = { git = "https://github.com/storytold/egui", rev = "<FINAL_EGUI_REV>" }
egui_kittest = { git = "https://github.com/storytold/egui", rev = "<FINAL_EGUI_REV>" }
epaint = { git = "https://github.com/storytold/egui", rev = "<FINAL_EGUI_REV>" }
epaint_default_fonts = { git = "https://github.com/storytold/egui", rev = "<FINAL_EGUI_REV>" }
ecolor = { git = "https://github.com/storytold/egui", rev = "<FINAL_EGUI_REV>" }
emath = { git = "https://github.com/storytold/egui", rev = "<FINAL_EGUI_REV>" }
winit = { git = "https://github.com/storytold/winit", rev = "<FINAL_WINIT_REV>" }
```

`egui_glow` is in the lock today as an optional `eframe` dependency (`Cargo.lock:1304` at the
baseline) and resolves to registry 0.37 after the bump even though this app selects wgpu. Patch it
with the same family revision, or explicitly record it as lock-only/unselected with proof that no
accepted feature enables it; "every egui-family package patched" must be literally true.
`egui_system_font_provider` is not patched because `system_fonts` is not enabled. `accesskit`,
`accesskit_consumer`, `accesskit_winit` and `kittest` remain registry packages at the versions the
final lock uses (0.25.1 / 0.39.1 / 0.34.0 / 0.5.0, if unchanged). If a future change enables
`system_fonts`, patch `egui_system_font_provider` too and re-audit `fontique`/`font-types`.

Patch sections in dependencies do not propagate into this root. A root patch of only `egui` would
leave `eframe`/`egui-winit`/`egui-wgpu`/`egui_extras` on the registry and can produce two families;
the full list above is deliberate.

Use `rev`, never a branch or tag, in the manifest. `Cargo.lock` should show
`git+https://...?rev=...#<40-hex>` and no `branch=`/`tag=` URL for production. A branch in a lock is
review-blocking even if the lock currently records the SHA.

### 6.3 Lock checks

After each lock change, record and inspect:

```sh
sha256sum Cargo.lock
cargo tree -d
cargo tree -i egui@0.37.0
cargo tree -i winit@0.30.13
grep -n 'name = "egui"\|name = "eframe"\|name = "egui-winit"\|name = "egui-wgpu"\|name = "egui_glow"\|name = "egui_extras"\|name = "egui_kittest"\|name = "epaint"\|name = "winit"' Cargo.lock
grep -n 'source = "git+' Cargo.lock
```

Acceptance:

- Exactly one egui-family source and one winit source for the suite build.
- No registry `egui`/`epaint`/`egui-winit`/`egui-wgpu`/`egui_glow` copy beside the git copy.
- No new duplicate egui-family package. Existing unrelated duplicates are recorded, not "fixed"
  blindly.
- No `branch =`, no machine-local `path =`, no `file://` and no patch entry outside the root.
- `--locked` builds/tests pass; the lock is committed with the manifest change, never left with a
  0.37 manifest and a 0.36 lock.

### 6.4 MSRV, Nix and packaging

- `Cargo.toml` declares Rust 1.90 while both egui 0.36.2 and 0.37.0 declare 1.95. Raise the
  workspace `rust-version` to 1.95 as the established dependency floor. CI uses
  stable, currently 1.99 on this host; record the exact `rustc -Vv` for every run.
- `flake.nix` uses `cargoLock.lockFile` and `buildRustPackage`. Git dependencies in the lock need
  `cargoLock.outputHashes` entries for the exact revisions (all patched git packages plus winit);
  otherwise Nix evaluation/build will refuse them. Generate the hashes from the pinned revisions
  during implementation, record them in the review, and run the Nix gate. Do not invent hashes and
  do not use a local path to avoid them.
- `packaging/web/package.sh` still enforces 24 MiB. Record the baseline wasm size before the
  upgrade and the candidate size; a hard-limit failure blocks the stage.
- Windows ARM64, FreeBSD and release workflows build from the same lock; `--locked`/network access
  to the public git revisions is required. No release publication is part of this plan.

## 7. Test and regression plan

### 7.1 Build/test commands

Every new command is submitted through `maintenance/task.py run/status/finish` with its own task,
exact source, durable evidence and unique job ID. The commands below are proposals for the
implementation stage, not commands run for this plan. Use a complete checkout (assets restored).

```sh
python3 /home/codex/work/maintenance/task.py run \
  --job-id launcher-egui037-fmt-01 --task launcher-egui037 --kind build \
  --source /home/codex/work/CraftApps/egui037-migration-20261022/craft-launcher \
  --evidence /home/codex/work/ssd2/task-evidence/launcher-egui037 \
  -- cargo fmt --all -- --check

python3 /home/codex/work/maintenance/task.py run \
  --job-id launcher-egui037-clippy-01 --task launcher-egui037 --kind build \
  --source /home/codex/work/CraftApps/egui037-migration-20261022/craft-launcher \
  --evidence /home/codex/work/ssd2/task-evidence/launcher-egui037 \
  -- cargo clippy --workspace --all-targets --locked -- -D warnings

python3 /home/codex/work/maintenance/task.py run \
  --job-id launcher-egui037-test-01 --task launcher-egui037 --kind build \
  --source /home/codex/work/CraftApps/egui037-migration-20261022/craft-launcher \
  --evidence /home/codex/work/ssd2/task-evidence/launcher-egui037 \
  -- cargo test --workspace --locked

python3 /home/codex/work/maintenance/task.py run \
  --job-id launcher-egui037-wasm-01 --task launcher-egui037 --kind build \
  --source /home/codex/work/CraftApps/egui037-migration-20261022/craft-launcher \
  --evidence /home/codex/work/ssd2/task-evidence/launcher-egui037 \
  -- cargo xtask wasm

python3 /home/codex/work/maintenance/task.py run \
  --job-id launcher-egui037-wasm-clippy-01 --task launcher-egui037 --kind build \
  --source /home/codex/work/CraftApps/egui037-migration-20261022/craft-launcher \
  --evidence /home/codex/work/ssd2/task-evidence/launcher-egui037 \
  -- cargo clippy -p artcraft-launcher-core -p artcraft-launcher-web \
     --target wasm32-unknown-unknown --locked -- -D warnings
```

Then `packaging/web/package.sh`, the Nix gate, and the native Windows/Linux workflows through the
same adapter. `cargo xtask ci` is the repository's combined gate, but the explicit `--locked`
commands above are the review evidence because `xtask ci` omits `--locked`.

Baseline, stock-0.37 control and candidate performance must use separate allocated
tasks/targets (for example `launcher-egui037-base`, `launcher-egui037-stock`, and
`launcher-egui037-cand`), never a shared target directory. Each command is a fresh `task.py`
job with durable evidence; no test/build is run directly.

### 7.2 Automated assertions

**Source and API invariants**

- No `WidgetType::`, `Id::new(` (egui root ID), `ui.id()` or `Context::root_ui` in production
  sources; no implicit `From<&str>`/`From<String>` `Id` construction (removed in 0.37).
- No `egui_glow` registry copy beside the patched family; no unpatched egui-family package in the
  accepted `--all-features`/lock audit.
- No `system_fonts`/fork-only API in app source; stock 0.37 contract build still compiles.
- Every production crate keeps its `#![deny(...)]` panic headers; no new `unwrap`/`expect`/`panic`.

**Lifecycle and multi-pass**

1. `ui_actions_are_sent_once_across_discarded_passes`: force `ctx.request_discard` for two passes,
   click Install/Update/Launch/launcher-banner UpdateLauncher, assert one `Command` and one
   launch-count increment.
2. `same_action_in_next_frame_is_sent_again`: click Install/Launch in frame 1, advance
   `cumulative_frame_nr` with a new `run_ui`, click the same action, assert a second command and a
   second launch increment (proves `flushed` clears).
3. `two_distinct_actions_in_second_pass_are_both_sent`: click A in pass 1 and B in pass 2; both
   delivered once.
4. `pending_action_survives_fonts_ready_early_return`: a click followed by the `fonts_ready` early
   return is flushed exactly once.
5. `logic_only_polls_and_updates_without_ui`: `run_logic` with a fake backend event; state changes,
   no panic, no `ui` call.
6. `hidden_job_finished_toast_appears_on_next_ui`: no toast while hidden; fresh toast on the next
   `run_ui`; no duplicate toast on a second pass.
7. `completed_multipass_then_hidden_ticks`: complete a multi-pass frame, then run repeated hidden
   logic ticks; fresh queue items are consumed once and no stale input is replayed.
8. `forced_hidden_ui_pass_handles_current_input`: request a hidden UI pass and assert current input
   is handled and a consumed queue item is not replayed; visibility is not used as the phase.
9. `buffered_input_restored_with_window`: hide, buffer events, restore, assert they are consumed
   once in the UI phase.
10. `background_completion_between_passes_is_not_lost_or_duplicated`: deliver a completion after
    pass 1 and before pass 2; the final state contains it once.
11. `completed_work_requests_no_extra_repaints`: 60 `run_logic` ticks after completion; only the
    shell's scheduled 60 s repaint remains.
12. `tray_loop_saves_changed_settings_once`: existing tests remain; add a no-window change and
    reopen check.

**Panels, geometry and input**

13. In a root `Ui` at 1180x1200, ppp 1.0: header rect height 48, footer 34, central starts at
    header.bottom and ends at footer.top; central clip is inside the window; no overlap. Load a
    0.36 `egui` memory blob first and assert the same first-frame fixed sizes without a
    migration; exact_size controls the result. Test the window entry separately.
14. At widths 720, 800, 1180, 1440, ppp 1.0/1.5/2.0: grid columns are the expected count, card
    rects share the 1 px hairline, and the table column widths are 220/176/150/104/206 where the
    width permits; no cell is clipped.
15. Settings and details cards stay inside the window: width clamp 480..1280, height clamp
    320..1000, lightbox at most 1400 points; dialog widths 420/440/480/880 etc. as designed.
16. Search field and folder field: exact height `CONTROL_H`, text baseline/centre, type/select/
    copy/paste, IME event path, undo one step, and selection survives a click on a non-text button
    (#8763/#8764) for both pointer and keyboard paths.
17. Details drawer: open/close slide, scrim click closes, Esc order (lightbox then drawer), arrow
    keys wrap, the currently focused drawer widget is not cleared by an unrelated pass, and
    reopening starts at scroll 0 and picture 0. Do not claim a trapped modal: the drawer is an
    `Area`, not a `Modal`. If focus containment becomes a requirement, add an explicit focus
    design (first focus target, Tab/Shift-Tab containment, restore on close, Esc precedence) as
    its own reviewed change.
18. Close/hide/minimize/maximize: existing unit tests plus a headless viewport-command assertion;
    on Wayland the window closes to the tray rather than hiding.

**Accessibility**

19. A strict harness for grid, table, Settings, install dialog, details and lightbox passes the
    automatic accessibility check; a scoped `.with_accessibility_check(false)` harness asserts
    `unnamed_widgets()` is empty and reports names.
20. Exact roles/names for the table in §4.7; ComboBoxes are `Role::ComboBox` with the visible
    label; checkboxes are `Role::CheckBox`; icon buttons are `Role::Button`; images that are
    clickable have a button/name; the actual combo button node is named.
21. Negative probe: remove the sort label association and assert the strict test fails; remove the
    Settings gear's name and assert it fails.

**Fonts and glyphs**

22. Every painted symbol in §4.6 passes `has_glyphs` for the family actually used; `⚙`/`⟳` are
    drawn, not font glyphs. Compare 0.36 default-emoji rendering and 0.37 painter icons with
    reviewed before/after images.
23. `FontData::bytes()` CJK test compiles; with a system CJK font, every named UI family draws the
    Chinese and Japanese catalogs; without one, the UI stays English and Settings says why. Add
    E14 fallback row-height/baseline comparisons against 0.36.2.
24. Deliberate negative fixture: a character no installed font has, rendered by a harness with
    `MissingGlyphPolicy::Panic`, must fail. Do not ship that fixture as a passing test.

**Persistence**

25. `Settings` round-trip: every field serialized and restored, including `auto_update`, install
    locations, hidden, launches, language, theme, check policy.
26. A 0.36-style `app.ron` with `window`, `egui`, `settings` loads the settings and preserves
    unknown keys on save; old window geometry is ignored once (documented); next save writes the
    new window schema; legacy panel state is ignored safely and fixed header/footer geometry is asserted (§4.5).
27. Search text and notes zoom do not appear in the saved settings; reopening resets them.
28. Install-folder override and "Use default" persist only after the install action, not on a
    browse dialog cancellation.

**Visual/pixel and mutation probes**

29. Four fixed Linux snapshots (grid dark, table light, Settings dark, details dark) with
    `with_render_cursor(false)`, `with_context_setup` installing fonts/theme/loaders, fixed size,
    ppp 1.0, theme, and `animation_time = 0`. Compare raw RGBA against reviewed fixtures. If the
    CI runner's software rasterizer differs within a run, use a documented tolerance (for example
    max channel delta <= 1 and <= 0.1% pixels differing) only after reviewing the diff; do not
    auto-update the fixture.
30. Geometry/AccessKit assertions run on Linux and Windows (and macOS when real evidence exists);
    pixel goldens are Linux-only because Windows/macOS rasterization differs.
31. Mutation probes: shift the footer by 1 point, change one hairline colour, reverse the table
    columns, remove the panel order, remove the action dedup, or drop `MouseWheelSource`; each must
    make its test fail.

### 7.3 Native and browser workflows

**Linux (native)**

- Build with `--locked`, run under X11 and Wayland (XWayland is not Wayland evidence), at 1x,
  1.5x and 2x scaling, light/dark/system theme.
- Check first frame, fonts, grid/table, search, install dialog and folder picker (`rfd` opens a
  native picker; headless tests cannot prove this), details drawer and lightbox, settings, notes
  zoom, tray menu, close-to-tray, reopen, focus return.
- Copy/paste and middle-click PRIMARY in search/folder through the W15P design on at least one
  wlroots/KWin compositor and one GNOME/Mutter compositor; record the compositor, supported
  capabilities, serial freshness, owner/seat changes, absent offers, self-paste and canceled/slow
  transfers. A regular-clipboard result is not PRIMARY evidence.
- Exercise the E21b/W27 IME surrounding-text trace with a real Vietnamese/predictive IME on
  Wayland: outgoing text stays in sync after delete+commit, deletion-only batches, preedit
  exclusion, field switch, focus loss and password/no-context clearing.
- Check the Linux portal theme monitor (`system_theme.rs`) still wins only when the preference is
  System; 0.37's `RawInput.system_theme` is unchanged.

**Windows (clone VM)**

- Build the actual task checkout with MSVC and `--locked` using the clone's helpers; use a
  separate target. Record the exact SHA, features and executable hash.
- Launch through the interactive scheduled-task helper; confirm the app is on the active console
  (session ID is not hard-coded). Capture QMP screenshots before input.
- Check D3D12/FXC startup and pipeline-creation time (#8736), custom title-bar drag/resize,
  minimize/maximize/close, tray, notifications, MSI/portable paths if in scope, copy/paste, IME,
  non-Latin shortcut layouts.
- The Windows row is a Basic Render Driver/CPU row; it is not a physical-GPU result.

**macOS**

- PR CI does not build macOS. Release CI builds the universal DMG but is not runtime evidence.
  The transparent full-size title bar, traffic lights, Spaces, reduced-motion AppKit call and
  notification identity remain an unresolved gate until real hardware/CI evidence is produced.
  Do not report them as passing from Linux kittest.

**Web**

- `cargo xtask wasm`, `cargo clippy` wasm and `packaging/web/package.sh`; record wasm size against
  the 24 MiB gate.
- Serve the zip and test in a real browser: catalog loads, download links select the right asset,
  search, grid/table, details images, window resize, keyboard navigation, copy/paste where the
  browser permits it, console errors. The 0.37 Canvas glyph rasterizer is not a substitute for
  native CJK font evidence.
- Browser automation is not present in the repository; this is a manual workflow until a browser
  harness is added.

## 8. Performance protocol

### 8.1 Baseline first

Before any production edit, add a test-only benchmark/golden harness with a version-neutral
measurement core plus version-gated 0.36.2/0.37 harness modules (the 0.37-only kittest APIs in
§3.3 cannot compile on 0.36.2). Compile and run the 0.36.2 module in a separate baseline worktree
at `9419d8977bc70f80373da8558356e4ba7387c7b2` and measure it first. Do not run the baseline after
changing the lock. The baseline/candidate pair is paired; add an equivalent stock-0.37 control for
every workload where stock and fork are comparable, so a fork regression can be attributed.

Record for every run:

- exact source SHA and `Cargo.lock` SHA-256;
- feature set (`default_fonts`, `wgpu`, `accesskit`, `wayland`, `x11`, `persistence`, `links`,
  `clipboard`, test `wgpu`/`snapshot`);
- Rust and Cargo versions, profile and `RUSTFLAGS`;
- OS image, CPU, memory, GPU/renderer/driver, `WGPU_BACKEND`, whether llvmpipe;
- viewport size, `pixels_per_point`, theme, font definitions and fixture hash;
- warmup count, timed iteration/frame/fresh-process counts, raw samples, median/p95/p99/min/max, peak RSS, idle repaint/CPU.

Host inspection (not a benchmark result): Intel Xeon E-2176G, 12 threads; the parent environment
reports an i915 render device at `/sys/class/drm/renderD128` (PCI 8086:3E96) and reportedly
`/dev/dri/renderD128`, but the worker sandbox may hide `/dev/dri`, no display/compositor is
inherited, and the backend/driver is unqualified. `renderD128` presence is availability evidence,
not a usable-GPU result. Qualify the selected adapter/backend/driver and compositor through managed
jobs. Historical llvmpipe measurements and the Windows VM's Microsoft Basic Render Driver are
software rows, separate from hardware results.

### 8.2 Operations

Use a deterministic fake backend and fixed `RawInput.screen_rect`, `time`, ppp and theme; install
fonts before the first pass; set `animation_time = 0`; no network, no image decode unless the
operation is image-specific.

Sample counts follow the suite protocol: at least **10 warmups and 30 timed iterations** for
steady/discrete operations, at least **1,000 measured frames** for any frame distribution whose
p99 is reported, and at least **20 fresh processes** for startup, with cold and warm caches reported
separately. A smaller exploratory run is allowed but must be labelled exploratory and cannot
supply acceptance p95/p99. Frame p99 is reported only from the pooled >=1,000 individual frames,
not from 30 Criterion sample means.

| ID | Operation | Samples and measurements |
| --- | --- | --- |
| P1 | `ui_grid`: 13 apps, 1180x1200, dark, all states (updates, downloads, paused, ready) | 10 warmup + >=1,000 measured frames; frame CPU median/p95/p99, shapes/vertices/indices, `textures_delta` bytes, requested repaint |
| P2 | `ui_table`: same state, 1180x1200, light | 10 warmup + >=1,000 measured frames; same |
| P3 | `ui_details`: open drawer with four preloaded 320x200 JPEGs, settled | 10 warmup + 30 timed complete UI passes; layout/first-frame time, image/texture bytes, shapes |
| P4 | `ui_settings`: open Settings at 1000x820, settled | 10 warmup + 30 timed complete UI passes; layout time, shapes, combo/scroll geometry |
| P5 | `logic_idle`: `run_logic` ticks with an empty backend | 1,000 ticks after startup; commands, repaint requests, CPU; assert 0 launcher-requested paints over 2 s after settle |
| P6 | `startup_first_frame`: construct `LauncherUi`, install fonts/loaders, first pass | >=20 fresh processes, cold and warm separated; time to first `FullOutput`, font install time, first-frame shapes |
| P7 | `search_keystroke`: type 20 characters into search | 10 warmup + 30 timed passes per character position; per-pass layout/tessellation time; also #8763/#8764 selection-then-click path |
| P8 | `tessellate_text`: fixed scene of 120 ASCII labels plus a mixed color-emoji control | 10 warmup + 30 timed `Context::tessellate` runs (2,000 calls each or an equivalent fixed count); instructions/wall time, vertex/index counts (#8731 probe) |
| P9 | `dx12_pipeline`: Windows `egui_wgpu::Renderer::new` with FXC | 10 warmup + 30 timed renderer creations, plus >=20 fresh app processes for first-frame startup; creation and first-frame time (#8736 probe) |
| P10 | `font_startup`: first missing-glyph lookup and font-install path with `system_fonts` off | 10 warmup + 30 timed lookups, plus cold/warm first-frame; record that `system_fonts` stays off (#8512 context, not a system-provider benchmark) |
| P11 | `wasm_size`: `packaging/web/package.sh` output | `.wasm` bytes, zip bytes, hard 24 MiB gate |

Use a small `benches/ui_hotpaths.rs` (Criterion or an equivalent fixed-sample harness) for P1-P8
and P10; P9 is a Windows guest measurement; P11 is a build artifact measurement. Keep raw samples
JSON/CSV, not only summaries. Do not call compile duration UI performance. A stock-0.37 control is
required for P1/P2/P6/P8 where the same fixture compiles against stock; fork-only behavior rows
state why no stock control exists.

### 8.3 Thresholds and noise policy

Correctness gates are hard and apply before performance:

| Metric | Threshold | Failure action |
| --- | --- | --- |
| Geometry/accessibility/persistence; reviewed pixel fixtures | exact geometry/names/values; reviewed pixel policy | fix; no golden update to hide a regression |
| Steady median (P1/P2/P3/P4/P7/P8/P10) | <= +5% vs paired baseline | profile; rerun; report |
| Steady p95 (same rows) | <= +10% | profile; rerun; report |
| Frame p99/max | <= +15%, **reporting/diagnostic** unless the median/p95 gates pass and no new stall appears | investigate; p99/max alone do not fail acceptance but a new >25% tail or a visible stall block sign-off |
| P6/P9 startup median/p95 | <= +5% / +10% vs >=20 fresh processes | profile; rerun; report cold and warm separately |
| Sub-millisecond metrics | relative increase must also exceed 0.05 ms or be corroborated by stable instruction/allocation counts | otherwise inconclusive; do not accept |
| P8 #8731 text tessellation | <= +5% median and +10% p95, or a reviewed fork patch restoring 0.36-like cost; mixed/color rows unchanged | #8731 is likely to fail; do not accept silently |
| P9 #8736 Windows FXC pipeline/first frame | <= +5% median / +10% p95, or a reviewed fork patch; no clamp-only shader change | #8736 is likely to fail; do not accept silently |
| P10 #8512 font startup | <= +5% / +10%, with `system_fonts` off; enabling it is a separate measured decision | report discovery vs first-missing-glyph separately |
| P5 idle repaint | 0 launcher-requested paints in 2 s after settle; only the 60 s schedule | fix lifecycle |
| Selection #8763/#8764 | selection survives a toolbar/context click and produces the intended action range | fix or backport with independent review |
| Hidden-parent #8748 | closed unmerged; do not claim its behavior. Test hidden root/occluded open-close and background completion without restoring the visible parent | fix locally if the launcher exposes a defect; no upstream-fix claim |
| Texture upload bytes / 100 frames | <= +5%, no repeated upload | inspect cache keys |
| Peak RSS | <= +8 MiB or +5%, whichever is reached first | report both |
| Galley/atlas cache | bounded, no monotonic growth over 10k frames | inspect cache key/language |
| Wasm size | <= +5% and under 24 MiB; otherwise explicit review | block if over the host limit |

Noise policy:

- Baseline, candidate and stock-0.37 control on the same host, OS image, toolchain, feature set,
  lock contents, viewport/DPI and font set. Alternate execution order to reduce thermal/cache bias.
- At least 10 warmup iterations (or 3 s, whichever is longer) followed by at least 30 timed
  iterations for steady/discrete work; >=1,000 individual frames for any reported p99; >=20 fresh
  processes for startup. Discard warmup.
- Pool only within the same measured distribution. Do not call a Criterion sample mean an
  individual frame time.
- If the coefficient of variation of medians is >3%, or another build/test/profiler competes for
  CPU, discard and rerun the pair. Do not widen tolerances after seeing a failure.
- Accept only when the paired median and p95 gates meet the table and the sub-millisecond floor is
  satisfied; report raw samples even on pass. p99/max are reported as diagnostics under the stated
  tail rule.
- A changed visual golden is never accepted because of a performance change. A compile-time
  number is not UI performance.
- llvmpipe and Windows Basic Render Driver are separate tables. A real-GPU row and a macOS row
  remain unresolved unless real evidence is obtained.

## 9. Reviewable PR stages, dependency DAG and rollback

```text
L0 baseline + version-gated test-only benchmark/golden harness (no production behavior)
  |
Reviewed published storytold egui/winit (W15P, E21b, required regression fixes)
  |
L1 atomic final-fork API/features/MSRV/lock/Nix + minimum test-green changes:
   drawn gear/refresh, strict accessible names, fixed panel geometry tests
  |
L2 lifecycle/action boundary, logic-only toasts, remaining accessibility/focus semantics
  |
L3 UI/geometry/accessibility/persistence/visual regression suite and mutation probes
  |
L4 final platform acceptance, isolated stock/fork control and final performance
  |
L5 documentation/release notes after all evidence and approval
```

L1 must be the first **green** stage: 0.37 kittest defaults panic on the `⚙`/`⟳` missing glyphs
and the unnamed ComboBoxes, so the drawn-glyph and minimum accessible-name fixes land here even
though they are behavior/visual changes. L2 adds the action boundary, lifecycle/toast work and the
remaining accessibility/focus design. L2 and L3 touch different files and could be prepared in
parallel, but land serially to avoid lock/ID churn. L1 cannot land before the reviewed final
storytold revisions exist. L4 qualifies L1-L3 on all required platforms; L5 is documentation-only.
Registry 0.37 is an isolated comparison workspace, never an intermediate production PR.

### L0: baseline harness and fixtures (test-only)

- Add a version-neutral benchmark/screenshot/geometry core plus separate 0.36.2 and 0.37
  harness modules behind `cfg`/feature gates; prove the 0.36.2 module compiles before use.
- Capture baseline performance on a separate allocated resource at `9419d89`.
- Do not change production behavior.
- Rollback: remove the test-only commit.

### L1: atomic final-fork API and feature compatibility

Files:

- `Cargo.toml` (workspace pins, MSRV), all package manifests that need feature changes.
- `crates/ui-egui/src/motion.rs` (`MouseWheelSource`), `cjk_font.rs` (`FontData::bytes`),
  `widgets.rs`/`details.rs`/`views.rs`/`bar.rs` (`Role`), `details.rs`/`dialogs.rs`/`app.rs`/
  `icons.rs`/`notes.rs`/`views.rs` (`Id::unique`/`scope_id`).
- `crates/ui-egui/Cargo.toml` (dev `default_fonts`), native `Cargo.toml` (`clipboard`), root
  `Cargo.toml` (`egui` with `monochrome_emoji_fonts`), and `ATTRIBUTION.md` for the bundled
  dependency fonts.
- `apps/artcraft-launcher/src/main.rs` (`system_font_fallback: false`); fixed header/footer
  geometry tests in the UI crate, without a PanelState migration.
- `crates/ui-egui/src/views.rs`, `bar.rs`, `widgets.rs` (painter-drawn gear/refresh),
  `theme.rs` (bundled emoji coverage and dynamic-text glyph fixture), and
  `settings_dialog.rs`/`views.rs` (minimum strict accessible names and the
  `.show_ui(...).response.labelled_by(...)` fix).
- Root whole-family/winit patches at the final approved reachable storytold revisions,
  `Cargo.lock` regenerated from that coherent graph and matching `flake.nix` output hashes.
- Test-only kittest 0.5/AccessKit 0.25 adaptation (version-gated; no 0.37-only API in the 0.36
  harness).

Include all fork API/feature adaptations needed for this first green stage. The larger lifecycle
refactor stays in L2 only where L1 preserves correct existing behavior. A separate stock-0.37
workspace supplies comparison evidence; it is not this production manifest or rollback target.

Acceptance: `fmt`, `clippy -D warnings`, `test --workspace --locked`, `xtask wasm`, wasm clippy and
web package; no deprecation warnings; one approved fork family/winit, native/wasm feature identity,
clean locked fetch and Nix hashes. Rollback restores the coherent 0.36 source/manifests/lock and
settings from `9419d89`, with the preserved baseline executable reopening the fixture.

### L2: lifecycle, action boundary and remaining accessibility

Files:

- `crates/ui-egui/src/app.rs` (UiAction queue, pending toasts, launch count at flush,
  `check_now` split, root-Ui invariant).
- `crates/ui-egui/src/details.rs`, `dialogs.rs`, `views.rs` (all UI calls through `act`,
  including the launcher-update banner; remaining role/name work).
- `crates/ui-egui/src/widgets.rs` (menu and icon-button accessible names).
- `apps/artcraft-launcher/src/main.rs` (any shell wording).
- Focused tests for each behavior, including the per-frame dedup reset, both-actions-in-pass-2,
  `fonts_ready` early return, hidden/forced-paint lifecycle and the scoped unnamed-widget harness.

No change to `Command`/`Event`/`Settings` schemas. Rollback: revert this commit; L1 remains valid.
Acceptance: multi-pass exactly-once across passes and frames, hidden toast, strict accessibility,
drawer focus contract, all existing UI tests.

### L3: regression suite and visual evidence

- Add committed Linux pixel fixtures (reviewed), exact geometry fixtures, accessibility role/name
  matrix, persistence/old-app.ron tests, mutation probes and native workflow scripts/evidence
  capture.
- No production change unless a test exposes a real bug; then fix it in L2 or call it out
  separately.
- Rollback: test-only. Acceptance: the full §7.2 matrix; deliberate negative probes fail as
  expected; no auto-golden updates.

### L4: platform and final performance acceptance

Files:

- Recheck the L1 whole-family/winit revisions, `Cargo.lock` and Nix hashes for drift. Any required
  dependency fix is a separately reviewed coherent update, not a stock-to-fork migration.
- Isolated registry-0.37 contract build (for stock compatibility) and fork contract build.
- Final baseline/candidate performance evidence, including P8/P9/P10 and wasm size, plus PRIMARY
  and IME surrounding-text native workflows.

Rollback: restore the last accepted coherent source/lock/settings and its preserved executable;
before the first accepted 0.37 stage that is the recorded 0.36 baseline. Rehearse reopening the
saved fixture. Acceptance: one family/one winit source, stock control, fork contract, Nix gate,
W15P PRIMARY evidence, E21b surrounding-text evidence, performance table or a documented,
user-approved exception.

### L5: documentation and release notes

- Update docs/README/version statements only if they mention the egui version; do not rewrite
  historical audit hashes.
- Release notes in the user's voice; no agent/model/tool attribution.
- Rollback: documentation-only.

Every stage needs a fresh adversarial review and explicit publication approval. No stage in this
plan authorizes a push or PR.

## 10. Risks, unresolved decisions and blocking acceptance

1. **Fork and winit provenance.** The candidates (`a4f9a17f`, winit `8b35388b`) are personal
   comparison inputs. Production manifests must pin the **final reviewed storytold egui/winit**
   including the fork P8 release (P2b, P7, P7b/E21b, P7c) and completed W15P/W27 lifecycle;
   the fork P8 name is distinct from this plan’s P8 performance workload. Use
   immutable full revisions after separate approval and publication; those SHAs are unknown now.
   A branch, tag or candidate SHA is never a production pin. This blocks L1.
2. **#8731 text tessellation.** The inspected candidate has the 0.37 color-glyph code, not the
   proposed per-row fix. The launcher is text-heavy and is likely to exceed the +5% text/median
   gate. The final storytold revision must either carry a reviewed fork patch (with identical mesh
   tests and provenance) or the user must grant an explicit waiver with the measured profile. Do
   not auto-waive.
3. **#8736 DX12/FXC pipeline.** The inspected candidate has `wrap_texel_coord`; the launcher forces
   FXC. The reported +40 ms startup is a real Windows row. The final revision must carry a fork
   patch or an explicit waiver.
4. **MSRV.** Raise 1.90 to 1.95 to match the dependency reality, update CI/package toolchains
   in L1 and document the new minimum in the release notes.
5. **Sparse checkout/assets.** The assigned checkout cannot build because `assets/` is omitted.
   Restore it or use a complete checkout; do not report a build from this planning worktree.
6. **Nix git hashes.** Git dependencies require `cargoLock.outputHashes`; the exact hashes and a
   `nix build` result are unknown until implementation. A Nix failure blocks L1 for the packaged
   Linux row.
7. **Window geometry reset.** 0.37 intentionally ignores old window settings once. This plan
   accepts a one-time default window; a product requirement to preserve the old position/size
   would need a separate migration and scale-factor decision.
8. **`⚙`/`⟳` and arbitrary emoji.** 0.36 rendered both controls through the default
   `emoji-icon-font`, and 0.37 removes that fallback from `default_fonts`; this is a new visual
   regression, not existing tofu. The current decision keeps painter-drawn glyphs for the fixed
   controls and enables `monochrome_emoji_fonts` (about 1 MiB on native and web) for arbitrary
   release-note/feed/alert text. The OFL/MIT dependency fonts are attributed in `ATTRIBUTION.md`;
   no system-font provider, CJK bundle or proprietary face is added. Measure the wasm/binary cost
   before release acceptance.
9. **System fonts/CJK web.** `system_fonts` stays off; web CJK stays English. Enabling either is a
   separate product decision with startup-cost and language-selection consequences.
10. **macOS and real GPU.** No native macOS or hardware-renderer acceptance has been run for this
    task. The parent sees an i915 render device, but the worker sandbox may hide `/dev/dri`, no
    compositor/display is inherited, and the backend/driver is unqualified. Qualify it through
    managed jobs; do not infer a hardware or macOS result from llvmpipe or the Windows Basic Render
    Driver.
11. **Browser workflow.** There is no browser automation; the real browser keyboard/IME/clipboard
    and download workflow is manual evidence. `cargo check --target wasm32` is compile-only.
12. **Kittest 0.5 / AccessKit 0.25 test API.** Some query helpers may need adaptation; this is
    test-only but must be completed in L1 before the first green workspace test, and the 0.36
    harness must stay version-gated.
13. **E4 PRIMARY / W15P.** PRIMARY is not accepted through arboard. The final winit revision must
    include W15P on the existing connection/event queue; without it, PRIMARY is a measured,
    explicitly accepted limitation, not a default.
14. **E21b/P7b outgoing surrounding text.** Candidate E21/W27 has no producer or adapter. The final
    egui/winit revisions must include E21b before IME acceptance; this app inherits the built-in
    `TextEdit` producer and must test post-delete/commit text, preedit exclusion and focus/password
    clearing. `None` placeholders are not a port.
15. **Performance and visual exceptions.** Any threshold failure, changed golden or known
    upstream regression needs profiling, a rerun, and explicit human review. A single noisy
    sample, a compile-time number or an auto-updated snapshot is not acceptance.

## 11. Source index

Primary source anchors used by this plan:

- `Cargo.toml:23-26` (egui pins; workspace dependency block begins at 21); `crates/ui-egui/Cargo.toml:15-16,36`;
  `apps/artcraft-launcher/Cargo.toml:23`; `apps/artcraft-launcher-web/Cargo.toml:25`.
- `crates/ui-egui/src/app.rs:103-174` (`LauncherUi`), `:177` (`new`), `:227` (`attach`), `:239`
  (`raw_input`), `:269` (`toast`), `:285` (`configure`), `:315` (`logic`), `:360` (`handle`),
  `:540` (`zoom_input`), `:564` (`ui`), `:604/609/614` (panels), `:649` (`visible_ids`), `:712`
  (`launch_app`), `:763` (`draw_toasts`).
- `crates/ui-egui/src/views.rs:50-57` (heights), `:64` (header), `:98` (toolbar), `:194` (search),
  `:238` (sort), `:253` (update all), `:294` (footer), `:384/402` (grid/card), `:522/541` (TableBuilder),
  `:732/738` (actions), `:83` (`⚙`).
- `crates/ui-egui/src/details.rs:91` (drawer), `:131` (area), `:273` (scrim), `:399` (lightbox),
  `:449` (image role), `:522/539/584` (roles).
- `crates/ui-egui/src/dialogs.rs:13` (dialogs), `:23/48` (modal IDs), `:109` (notes scroll),
  `:263-272` (folder field).
- `crates/ui-egui/src/settings_dialog.rs:15` (`row`), `:57` (settings scroll), `:124/149/197/226`
  (ComboBoxes), `:157-163/253-261` (checkboxes).
- `crates/ui-egui/src/theme.rs:169-232` (visuals/apply), `:298-335` (fonts), `:338-377` (CJK).
- `crates/ui-egui/src/cjk_font.rs:62-95` (load), `:101-120` (face index), `:350` (font bytes test).
- `crates/ui-egui/src/motion.rs:13` (apply), `:30-55` (wheel), `:69` (test wheel helper), `:132` (direct `Event::MouseWheel` in the leave-alone test).
- `crates/ui-egui/src/widgets.rs:24-31` (text), `:131/174/225/429` (IDs), `:485` (more menu),
  `widget_info` sites throughout.
- `crates/ui-egui/src/bar.rs:46` (view toggle), `:81` (`⟳`), `:86` (refresh role).
- `crates/ui-egui/src/notes.rs:70` (collapsing notes), `crates/ui-egui/src/icons.rs:37` (grey icon
  cache).
- `crates/ui-egui/src/settings.rs:84-140` (`Settings` fields), `crates/ui-egui/src/i18n/tests.rs`
  (catalog/glyph tests).
- `apps/artcraft-launcher/src/main.rs:36` (`fn main`), `:118` (`eframe::run_native` call), `:144`
  (`native_options`), `:190/225/236/258/280` (wgpu), `:333-391` (window/tray helpers), `:565`
  (`tick`), `:608/620` (tray loop), `:715-775` (`Shell` App impl).
- `apps/artcraft-launcher/src/settings_file.rs:24-83` (RON storage), `motion.rs` and
  `system_theme.rs` for platform settings.
- `apps/artcraft-launcher-web/src/web.rs:60-88` (language/start), `:112-127` (web App),
  `:168/315` (picture protocol).
- `crates/core/src/protocol.rs:16-60` (`Command`), `:226` (`Event::Screenshot`).
- Historical evidence paths are listed in §1.3 and are not acceptance.

Upstream/fork sources:

- egui 0.37.0 `CHANGELOG.md`; `crates/egui/src/context.rs` (`run_ui`, `run_pass`, `root_ui`,
  `run_logic`); `crates/egui/src/id.rs`, `id_salt.rs`, `ui.rs`; `crates/egui/src/data/input/event.rs`
  (`MouseWheelSource`); `crates/epaint/src/text/font_data.rs` (`Blob`); `crates/egui_kittest/src/`
  (`accessibility.rs`, `builder.rs`, `lib.rs`); `crates/egui-winit/src/window_settings.rs`;
  `crates/egui/src/containers/panel.rs` (parent-salted `Panel::id`).
- Candidate fork `PATCHES.md` E1-E21 plus the new E21b; winit W1-W27 plus W15P; candidate SHAs
  `a4f9a17f`/`8b35388b` are comparison inputs, not production pins.
- Lifecycle evidence: `/home/codex/work/ssd2/task-evidence/egui037-migration-20261022/lifecycle-source-audit.md`.
- IME evidence: `/home/codex/work/ssd2/task-evidence/egui037-migration-20261022/ime-surrounding-gap.md`.
- Upstream issues #8731, #8736, #8763, #8512, #8657, #8577, #8748, #8428; snapshots in
  `/home/codex/work/ssd2/task-evidence/egui037-migration-20261022/upstream-discussions.json`.
  #8764 is separately verified by `upstream-8764-fresh.json` as open/unmerged; it is not
  used as snapshot evidence.

This paragraph described the planning-only document and is retained as historical context: at
that time no claim in it was a fresh build, test, GUI or performance result. The current status is
at the top of this file, and the 2026-10-24 local implementation update below records the later
managed evidence. The first publication still requires fresh explicit human approval after the
exact contents and validation are shown.


## Local implementation update — 2026-10-23

The user clarified that reviewed plans remain local and implementation proceeds before publication. The earlier requirement to publish/land this plan first is superseded; pushes and PR publication still require explicit approval.

At the time of the 2026-10-23 refresh, canonical `storytold/craft-launcher` `main` resolved to `ce3b29f4f23bf958b457a55effae87127f5b5dd4`. The owned checkout was later fast-forwarded to `674b893f0e5dcaceb742ae22728b8bde8bd73911`, which is the current tested HEAD for this implementation; the `ce3b29f4` value is retained as historical provenance. Earlier baseline SHAs and line anchors above remain historical review evidence. The original refs remain reachable.

No migration API redesign required by this delta; preserve the merged packaging, test and chrono changes where present.

The complete static delta review and exact old/new object identities are retained in the task evidence: `canonical-diffs-refresh-20261023.json` and `overnight-consumer-review-20261023.md`. This amendment claims no new build, native GUI or performance acceptance.

## Historical local implementation update — 2026-10-24

The later jobs 20–22 and review v8 summarized at the start supersede the evidence below.

The working tree now contains the migration implementation and is no longer planning-only. Current
source and validation references:

- Checkout HEAD: `674b893f0e5dcaceb742ae22728b8bde8bd73911`.
- Managed tests: `egui037-launcher-candidate-20261024-upstream-tests-15` (exit 0; `artcraft-launcher-ui-egui` lib tests 60 passed, integration tests 67 passed, 0 failed).
- Managed lints/build: `egui037-launcher-candidate-20261024-upstream-native-lints-16` (exit 0; workspace clippy `-D warnings` and native `artcraft-launcher` build).
- Managed native GUI: `egui037-launcher-candidate-20261024-upstream-native-gui-17` and the session artifacts under `native-launcher-session-v3` (Settings at 1440x900 and 1000x700, Light theme, restart/theme persistence, normal quit, exit 0). The assessed source patch is `tested-source.patch` (SHA-256 `60e33f323344a7d33c6ced0eccf18d2c6013b46e6202c7136a7eb080d52d2cba`), the tested executable is `artcraft-launcher-tested` (SHA-256 `a94e0ea7022b43c5a7456aef485bfe67be7e6d1dcceaffeb2e2a515ce44ab5d1`), and `assessment.json` records `PASS` with the stated Wayland/software-Vulkan limitations.
- These jobs used `/home/codex/work/ssd2/task-evidence/egui037-migration-20261022/local-integration-config.toml`. F1 is therefore still open: the committed lockfile is not reproducible from the published checkout without an external patch file. The local pin architecture is unchanged in this repair; final immutable egui/winit pins and a lockfile generated by the ordinary locked workflow remain a publication gate.

The 2026-10-24 repair also enables egui's `monochrome_emoji_fonts` on native and web for arbitrary
release-note, feed and alert text; keeps the painter-drawn gear/refresh controls; attributes the
OFL `NotoEmoji-Regular` and MIT `emoji-icon-font` dependency assets; adds a bundled-stack emoji
coverage fixture; drives real UI call sites through repeated AccessKit clicks in the action-boundary
tests; removes duplicate accessible names from decorative hero icons; and adds per-surface
accessibility assertions. No CJK font or proprietary face is bundled. No Windows, macOS, web/wasm,
performance or real-screen-reader acceptance is claimed by this amendment.

### Review-tests-18 repair — 2026-10-24

The managed `review-tests-18` job ran after the F2–F5 repair and exited 1 with four failures in
`crates/ui-egui/tests/ui.rs` (65 passed, 4 failed):

- `clicking_a_card_opens_its_details_drawer` expected one `Role::Label` named `PhotoCraft`, but the
  open drawer has two legitimate wordmarks: the grid card's and the drawer's. The icon assertion
  remains "no `Role::Image` named `PhotoCraft`"; the label assertion now expects exactly those two
  wordmarks, so a restored decorative alt text or a third duplicate still fails.
- `changelog_shortcuts_zoom_only_the_notes_text` opened the first card's menu, but the grid's
  default order is name A–Z, so that was CadCraft, not PhotoCraft. The test now targets
  `More actions for PhotoCraft` explicitly and checks both that the Notes hero icon is not an
  image with the release title and that the title label appears once.
- `the_actual_update_button_queues_once_across_a_discarded_pass` and
  `the_actual_open_button_launches_once_across_a_discarded_pass` inserted the AccessKit
  `ActionRequest` with `Context::input_mut` after `begin_pass`; in egui 0.37 a request belongs to
  the `RawInput` seen by `begin_pass`. `run_ui` also spends raw events on its first pass and
  re-runs the second without them. The helper now starts a fresh visible frame with an empty
  priming `run_ui`, then drives one or two explicit `begin_pass`/`end_pass` passes that each carry
  the same request in `RawInput`; the passes share one `cumulative_frame_nr`, so the action
  boundary still has to deduplicate them. The next-frame call starts another fresh frame, so the
  same action is accepted again.

No production action-boundary semantics changed. These test repairs were subsequently verified by
managed jobs 20–22 and the mutation control recorded in the status above.
