# ADD Launcher: plan

Approved by Daniel Dobos (ADD Individual Solutions Ltd) on 2026-11-28. Turns this fork of
`storytold/craft-launcher` (Apache-2.0) into **ADD Launcher**: one window that installs, updates
and starts ADD Craft and the ADD apps (rebranded forks of storytold's Craft apps under
`DanielPierre2023`). Decision: **no code signature for now** (option B); every download is checked
against its release's `SHA256SUMS.txt`, as the upstream launcher already does for community apps.
A Microsoft Artifact Signing certificate for ADD Individual Solutions Ltd may follow later.

Reference fork already done: `DanielPierre2023/craft-anything` (ADD Craft), PR #1. Follow the same
legal rules: keep LICENSE-*, CONTRIBUTORS and original copyright; add "modifications Copyright (c)
2026 ADD Individual Solutions Ltd"; NOTICE and README state the fork and its changes and that it is
not made or endorsed by the ArtCraft Team; remove the ArtCraft name, logo and "A" mark
(`docs/brand/`) and anything from `storytold/craft-launcher-assets` (no licence).

## Facts found in the code (2026-11-28, upstream 8ae67ab)

- `crates/core/build.rs` downloads `https://getartcraft.com/launcher-catalog.toml` (+ `.sig`,
  Ed25519, key `crates/core/assets/launcher-catalog-public-key.pem`) and the icons **at build
  time** and generates the app list from it. The private key is storytold's.
- Runtime: `crates/core/src/hosted_catalog.rs` fetches the same signed catalog; announcements use
  the same key (`docs/announcements.md`). Nightly builds use storytold's signers
  (`crates/native/src/nightly_trust.rs`, `nightly_signing.rs`).
- Source policy: `crates/core/src/catalog.rs` maps ids to `storytold/<id>`; `hosted_catalog.rs`
  requires `repo == storytold/<id>`; `nightly.rs` requires `storytold/`. Community apps
  (`AppKind::Community`) may live in another repo, pinned in the catalog.
- Downloads: GitHub Releases of the app's repo, verified against `SHA256SUMS.txt`
  (`docs/install-sources.md`). Windows installs need no publisher signature for community apps;
  macOS needs `mac_team` (ignore macOS for now).
- Details-drawer pictures come from `storytold/craft-launcher-assets` (unlicensed): remove.
- Version choices ("pins") are shared with the apps through `craft-update` in `craft-libs`
  (`crates/update/src/pins.rs`, `Pins::default_path`). ADD apps and ADD Launcher must use their own
  pins file, not ArtCraft Launcher's: change the path in `DanielPierre2023/craft-libs` and point the
  apps at that rev.
- User-Agent is `ArtCraft-Launcher/<version>`.

## Work, in order

1. **Rebrand** (rename script as in ADD Craft): ArtCraft Launcher → ADD Launcher,
   `artcraft-launcher` → `add-launcher`, `artcraft_launcher` → `add_launcher`, bundle id →
   `com.addindividualsolutions.add-launcher`, User-Agent `ADD-Launcher/<version>`, own config and
   data folders, new MSI UpgradeCode and Manufacturer "ADD Individual Solutions Ltd". Icon: the ADD
   brand mark (navy #0504AA, gold #E8A731, Fraunces glyphs as paths; see
   `DanielPierre2023/craft-anything` `assets/app-icon/README.md`), regenerated for the launcher's
   icon and tray sizes.
2. **Own app list, no network at build**: replace the build-time download with a checked-in
   `crates/core/catalog.toml` in the same schema, listing ADD Craft (`craft-anything` repo, id
   `add-craft`) and, as they get rebranded, `add-photo`, `add-vector`, `add-film`, `add-light`,
   `add-pdf`, `add-design`, `add-effect`, `add-fig`, `add-word`, `add-sound`, `add-deck`, `add-cad`,
   `add-grid`, `add-texture`, `add-material`, `add-ani`, `add-auto`, each with repo
   `DanielPierre2023/<repo>`, publisher "ADD Individual Solutions Ltd", ADD icons bundled locally
   (hash-checked), no screenshots. Apps not yet released show as "not available yet".
3. **Policy**: allow `DanielPierre2023/` where upstream requires `storytold/`; keep the
   SHA256SUMS check mandatory; keep the "pinned repo" rule (no arbitrary sources).
4. **Switch off** the hosted signed catalog, announcements and the Nightly channel (no keys of
   ours), with clear UI wording; nothing contacts getartcraft.com. Keep the code paths compilable
   so the weekly upstream sync still applies.
5. **Pins**: own pins file path for ADD Launcher and, via `craft-libs`, for every ADD app.
6. **Romanian**: add `ro` to the launcher's i18n (all catalogs marked `complete_menus` must have
   every row; formal address, ș/ț), same method as ADD Craft (numbered translation, placeholder
   check).
7. **Release step for every ADD app** (start with ADD Craft): on push to `main`, if the tag
   `v<workspace version>` does not exist, build the Windows MSI and portable zip, write
   `SHA256SUMS.txt`, and publish a GitHub Release with them (Daniel approved automatic releases on
   2026-11-28). Asset names must match what the launcher expects
   (`<id>-<version>-windows-x64.msi` and `<id>-<version>-windows-x64-portable.zip`, see `crates/core/src/platform.rs`).
8. **Build workflow** for the launcher like ADD Craft's (`.github/workflows/build.yml`: Windows
   MSI + Linux tests, no storytold actions or secrets) plus its own release step.
9. **Checks**: cargo cannot run in the agent workspace; GitHub Actions is the check. Never skip or
   weaken tests; adapt tests that pin storytold repos/ids to the ADD ones.

## After this

- Weekly upstream sync (scheduled task, Mondays 06:41 Berlin) covers this repo once its NOTICE
  marks it as the ADD fork; record the synced upstream commit in `tools/add-sync/UPSTREAM_COMMIT`.
- Then rebrand the 17 apps one per session (skill `rebrand-craft-fork`), each with Romanian, a
  release step and an entry in this launcher's catalog.
