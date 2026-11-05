# Manual Windows MSI batch acceptance test

Use a disposable Windows 10/11 x64 virtual machine with UAC enabled, internet access, and a
normal unelevated desktop session. Take a snapshot first. Do not run the launcher as
Administrator. This is the test that can verify the interactive UAC count; Linux checks and
hosted Windows jobs cannot.

## Build the development launcher

From a clean checkout of the feature branch on Windows with the MSVC Rust toolchain, Windows SDK,
WiX 5.0.2, and the project's required WiX extensions installed, run:

```powershell
pwsh packaging/windows/package.ps1 -Arch x64
```

With no signing secrets configured, the supported script leaves the launcher binary, MSI, and
portable ZIP unsigned and emits a warning. Do not add a development signing certificate to the
production trust policy or change the signing script. Use only
`dist/release/artcraft-launcher-<version>-windows-x64-portable.zip` for this manual run; the
portable ZIP includes `artcraft-launcher.exe`, `portable.txt`, notices, licenses, and attribution.
It uses the statically linked C runtime and needs no separate VC++ runtime. Fonts and the icon are
embedded in the executable. Record the branch commit and SHA-256 of the executable before copying
the ZIP into the VM.

Windows SmartScreen or local policy may warn about an unsigned development binary. Do not disable
SmartScreen, antivirus, UAC, or signature checks. If local policy does not permit launching the
unsigned test executable in a disposable VM, stop and use an approved development execution
policy or build on that VM from source. This does not change the acceptance policy for app MSI
packages.

## Install official older versions

Download these official x64 packages and the `SHA256SUMS.txt` file from each release:

| App | Starting package | Expected update |
| --- | --- | --- |
| PhotoCraft | [v0.13.0 release](https://github.com/storytold/photocraft/releases/tag/v0.13.0), `photocraft-0.13.0-windows-x64.msi` | [v0.20.2](https://github.com/storytold/photocraft/releases/tag/v0.20.2) or newer |
| VectorCraft | [v0.15.0 release](https://github.com/storytold/vectorcraft/releases/tag/v0.15.0), `vectorcraft-0.15.0-windows-x64.msi` | [v0.21.1](https://github.com/storytold/vectorcraft/releases/tag/v0.21.1) or newer |

Before opening either MSI, compare its SHA-256 with the exact entry in that release's published
checksum file and confirm its signature in a normal PowerShell session:

```powershell
Get-FileHash .\photocraft-0.13.0-windows-x64.msi -Algorithm SHA256
Get-AuthenticodeSignature .\photocraft-0.13.0-windows-x64.msi |
  Format-List Status,SignerCertificate
Get-FileHash .\vectorcraft-0.15.0-windows-x64.msi -Algorithm SHA256
Get-AuthenticodeSignature .\vectorcraft-0.15.0-windows-x64.msi |
  Format-List Status,SignerCertificate
```

Require a checksum match, `Status : Valid`, and signer `Learning Machines Inc`. The signature
certificate's exact inspected subject, issuer, and thumbprint are recorded in
[windows-msi-identities.json](windows-msi-identities.json). Stop if the release no longer has
these files or if either security check differs. Install both MSIs normally with their default
per-machine scope, accepting their ordinary setup UAC requests. These older packages are the
legacy `ALLUSERS=1` form. Confirm each app launches, close both apps, and check their registered
versions and per-machine installer contexts before continuing. These initial install prompts are
separate from the update test.

## Run the batch test

1. Extract the whole portable ZIP into a writable folder in the VM. Keep `portable.txt` beside
   `artcraft-launcher.exe`; the launcher stores its profile and download cache under
   `ArtCraftLauncherData` beside itself.
2. Before launching, record the UAC settings without changing them:

   ```powershell
   Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System' |
     Select-Object EnableLUA,ConsentPromptBehaviorAdmin,ConsentPromptBehaviorUser
   ```

   `EnableLUA` must be `1`. A launcher or requesting PowerShell process already running elevated
   will not cause a second approval. Administrator policy `ConsentPromptBehaviorAdmin=0` elevates
   without prompting; value `5` prompts only for non-Windows binaries and may not show a prompt for
   Windows PowerShell. To test an interactive approval, use an unelevated session with a policy
   that prompts for consent/credentials (for an administrator, value `2` or `4`). Do not disable
   UAC. If policy is domain-managed, record it and do not override it.
3. Start `artcraft-launcher.exe` normally from the standard user desktop. Verify it identifies
   PhotoCraft v0.13.0 and VectorCraft v0.15.0 and offers the current dual-scope releases.
4. Choose **Update all**. Let both downloads complete and watch the verification/install phases.
   Expect exactly one interactive UAC approval for Windows PowerShell, followed by the two
   Windows Installer progress windows. The v0.20.2 PhotoCraft and v0.21.1 VectorCraft MSI
   Property tables declare `ALLUSERS=2` and `MSIINSTALLPERUSER=1` (dual-scope, per-user default).
   The elevated batch must explicitly invoke each with `ALLUSERS=1 MSIINSTALLPERUSER=` (empty)
   to keep the already-verified machine context, matching the package's machine-scope selection.
   The launcher itself must remain unelevated. Each MSI
   progress window has a Cancel button; cancelling one item should be reported for that app while
   the batch proceeds to the other authorized item.
5. Confirm the launcher reports each app's individual result, detects PhotoCraft v0.20.2 and
   VectorCraft v0.21.1 (or the newer selected versions), and both apps launch. Check the Windows
   uninstall/install records for the new `DisplayVersion` values. Confirm each product remains
   registered per-machine, with no new per-user registration or second copy under the user's
   profile. The launcher must not silently migrate a per-user installation into the machine scope;
   per-user updates should remain on the unelevated route.
6. If an item is rejected before Windows Installer starts, inspect
   `%SystemRoot%\System32\ArtCraft-MSI-<session>\diagnostics.jsonl` from an administrator session.
   It records the validation stage, exception types, HRESULTs, actionable reason, and UAC context.
   MSI verbose logs (`<index>.log`) exist only after validation succeeds and Windows Installer is
   launched. The diagnostics and logs are administrator-owned and retained; remove them only from
   an administrator session or discard the VM.

To exercise real signed-package validation without installing the apps, set the folder containing
the two downloaded MSIs and run the opt-in Windows test from a normal PowerShell session in the
checkout. It retrieves the published checksum files, validates Authenticode with Windows, and reads
the actual MSI database using the production reflection declarations:

```powershell
$env:ARTCRAFT_TEST_OFFICIAL_MSI_DIR = (Resolve-Path .\official-msis).Path
cargo test --locked -p artcraft-launcher-native validates_documented_official_msi_packages_with_windows_signature_and_database_apis -- --ignored --nocapture
```

The broader current-release check covers the x64 MSI releases recorded in
[windows-current-msi-releases.json](windows-current-msi-releases.json), including FigCraft and
LightCraft. The manifest was checked against each official repository's latest release and
`SHA256SUMS.txt` on 2026-11-05. Download those exact MSI assets into one folder, then run this
non-installing production-policy test from the same unelevated checkout:

```powershell
$env:ARTCRAFT_TEST_CURRENT_MSI_DIR = (Resolve-Path .\current-official-msis).Path
cargo test --locked -p artcraft-launcher-native validates_current_official_x64_msi_releases_with_windows_security_apis -- --ignored --nocapture
```

It checks the local files against the recorded published hashes and independently has the
production policy fetch the official release checksum again, then verifies Authenticode,
certificate Organization, app/release identity, and MSI database properties. It deliberately
does not install anything. Refresh the manifest when testing later releases; a checksum or
metadata incompatibility must fail the test rather than relaxing the production policy.

## Repeatable negative cases

Restore the snapshot between cases and record which step and result you observed:

- Decline the batch's UAC prompt. Every eligible machine MSI should be reported cancelled and
  neither package should be installed.
- Cancel one Windows Installer progress window. That app should report cancellation; the other
  package should still complete.
- Leave an app running before its MSI starts. Its update should be refused and reported while the
  other package continues.
- Start an uninstall or individual MSI operation while a batch is downloading or installing.
  Launcher-owned Windows Installer operations should serialize through the shared queue; they
  must not overlap or produce error 1618.
- Cancel a download or close the launcher while one package download is still pending. A failed
  download should not prevent a valid ready package from installing.
- Install a per-user copy of a dual-scope app and verify its update stays per-user and requires no
  unnecessary elevation. Try a mixed per-user/per-machine Update all action and confirm only the eligible
  machine-wide ArtCraft packages join the elevated session.
- Use a separate snapshot to verify that a custom install location with an unsafe ACL or a
  reparse point fails closed.

Do not treat a successful Windows compilation, unit test, pre-elevated runner, or MSI-policy test
as proof of the single interactive UAC approval. Record the Windows edition/build, launcher commit
and hash, MSI versions/hashes/signature status, prompt count, per-app result, MSI exit/log status,
and any unavailable case. Restore or delete the VM after testing.
