# Windows installer UI

`installer-ui.wxs` is generated from the shared `craft-installer` crate in
[craft-libs](https://github.com/storytold/craft-libs/tree/main/crates/installer).
`package.ps1` compiles it with this app's package source. The common UI provides progress,
repair/remove, and explicit install/uninstall/repair success, cancellation and failure screens.

Regenerate or check from a sibling craft-libs checkout:

```sh
cargo run --locked -p craft-installer -- --write --app craft-launcher ../craft-launcher/packaging/windows/installer-ui.wxs
cargo run --locked -p craft-installer -- --check --app craft-launcher ../craft-launcher/packaging/windows/installer-ui.wxs
```

Edit the shared templates/profile and regenerate; avoid editing this snapshot by hand. The
profile retains this app's existing folder, shortcut and launch options; defaults, persistence,
components, associations and signing stay in the app-owned source. Release builds require no
sibling checkout, published crate or additional runtime dependency. `/qn`, `/qb` and `/passive`
remain unattended and do not launch the app.

Source/profile checks and native fixture MSI linking/UI table checks passed for x64, x86 and
ARM64. All 14 profiles passed full UI install, missing-file repair and cached uninstall, and
x86 silent install/removal. Folder upgrades, desktop choices and all six launch profiles also
passed their Windows fixture checks. See the
[Windows verification record](../../../craft-libs/docs/windows-installer-verification.md) for
interactive results, scope validation and remaining acceptance checks with real release payloads.
The shared crate includes reusable linking, full UI, option/upgrade/launch and x86 test runners.
Windows Settings now uses a separate MSI-owned entry whose uninstall command selects full UI,
including the completion screen. Its quiet command and direct `/qn`, `/qb`, `/passive` commands
remain unattended. The main feature references `CraftInstallerSettings`; the shared snapshot
owns this registration and hides the native MSI entry while retaining MSI repair/upgrade APIs.

The Settings display name appends `(Craft Apps)` for compatibility with older Launchers; the
hidden native MSI name stays unchanged. Existing ARP Modify policy is retained explicitly.
