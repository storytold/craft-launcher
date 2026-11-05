# Windows MSI batch security design

`Update all` is a single explicit operation. On Windows the UI sends one `InstallAll` command;
automatic app updates use that same route. The UI tracks app operations as soon as a command is
sent, so automatic detection and a manual click cannot queue duplicate work while the first
progress event is still in transit. The native backend reserves a slot for every selected app
synchronously before starting any worker. Eligible official, per-machine MSI jobs join that batch
after downloading. Each download is checked against its published SHA-256 before it reaches this
code. Failed, cancelled, per-user, portable, and community app jobs release their slots without
authorizing an elevated install.

The launcher GUI is never elevated. The MSI batch uses the Windows PowerShell host from
`GetSystemDirectory`; it starts unelevated and asks Windows to run that same OS executable through
`Start-Process -Verb RunAs`. The elevated process receives a fixed policy embedded in the Rust
binary and base64-encoded request data. No launcher executable, DLL, script file, or user-owned
module is loaded as administrator. The application does not change UAC settings. One PowerShell
session stages and verifies the eligible packages and invokes the system `msiexec.exe`
sequentially, so `msiexec` inherits that session's authorization rather than requesting elevation
for each MSI. The MSI `/passive` progress window retains Windows Installer's own Cancel button.
Declining UAC cancels the pending batch.

Windows decides whether a `RunAs` request displays a consent UI. Launching the launcher or its
requesting PowerShell process already elevated can suppress a second prompt. UAC policy can also
allow elevation without visible consent (for example, administrator elevation without prompting,
or a policy that prompts only for non-Windows binaries while the target is Windows PowerShell).
The protected `diagnostics.jsonl` records the requesting process's observed elevation, the elevated
child's administrator status, `EnableLUA`, and administrator/standard-user prompt policy values.
Those observations distinguish app launch behavior from machine policy; they do not claim that a
user saw or accepted a prompt. The manual acceptance test must use an unelevated session and a
policy that asks for consent or credentials.

This addresses the central objections in [upstream PR #68](https://github.com/storytold/craft-launcher/pull/68):
the old proposal elevated the per-user launcher, accepted caller-selected package paths and
hashes, and trusted results in a user-writable cache. It also addresses the reviewer's concern
about an administrator-credential prompt installing into another user's profile by admitting
only packages whose MSI database declares `ALLUSERS=1` and no effective `MSIINSTALLPERUSER`.
`ALLUSERS=1` on the `msiexec` command line is not used as a substitute for inspecting package
metadata.

## Authorization and protected files

The elevated request contains only the compiled catalog app ID, release tag, asset name, caller
checksum, and optional install destination. It carries no authoritative repository, URL, source
path, command, transform, extra `msiexec` argument, or log path. The elevated policy resolves
the app against the compiled official catalog, checks that the tag and MSI filename match that
app and version, downloads that repository/tag's `SHA256SUMS.txt` independently over validated
HTTPS, and requires the caller checksum to equal the one published there. Redirects are limited
to GitHub's release asset hosts, responses are size-capped, and the checksum entry must be unique.

MSI bytes come through a short-lived loopback-only server from already-open read-only file
handles. The source path never crosses the privilege boundary. The elevated PowerShell client
does not use a proxy, credentials, or redirects. It copies the bounded stream into a newly
created protected directory under the Windows system directory. SYSTEM and Administrators own
that directory and its files; ordinary Users have read access only where needed for results.
The listener uses nonblocking accept polling, but every accepted socket is explicitly returned to
blocking mode. Request-header reads and response writes have separate bounded timeouts. The source
loops over partial writes until the advertised file length is sent, treats premature local EOF or
an incomplete transfer as an I/O failure, and logs the random session, item index, expected and
transferred byte counts, and underlying socket/file error without logging paths. The elevated
PowerShell reader independently requires all `Content-Length` bytes before hashing or validating
the staged MSI; a stream failure has no direct-path fallback.
The staged MSI is re-hashed, checked for a valid Windows Authenticode signature, and held against
write/delete sharing while inspected and installed. The only executed installer is the system
`msiexec.exe`.

Signer identity and MSI-authored metadata are separate checks. Windows must report the
Authenticode status as `Valid` (including its platform chain and timestamp validation), and the
certificate simple name must exactly equal `Learning Machines Inc`. The certificate Organization
is independently extracted from the subject using the absolute system `crypt32.dll` export
`CertGetNameStringW`, `CERT_NAME_ATTR_TYPE` (3), and attribute OID `2.5.4.10`. The OID pointer is
allocated as ANSI data and released in a `finally` block; the API's required length is queried
first, capped at 1024 characters, and checked again after filling a bounded `StringBuilder`.
Missing, empty, oversized, or inconsistent values fail closed. The extracted Organization must
exactly equal `Learning Machines Inc`; text in the common name or another subject field cannot
satisfy this check. This is separate from the MSI database, which must independently report
Manufacturer `Learning Machines LLC`, an allowed app ProductName, the authorized release
ProductVersion, a platform matching the x64/x86/ARM64 filename, `ALLUSERS=1`, and
`MSIINSTALLPERUSER` absent or zero. The MSI database is read through the absolute system
`msi.dll` using in-memory P/Invoke declarations, avoiding elevated COM activation and a
user-writable temporary assembly. Reflection names each native export explicitly.
`MsiSummaryInfoGetPropertyW` uses its complete seven-argument signature, including `FILETIME*`
(passed as null for PID_TEMPLATE); the previous declaration omitted that argument and could
misalign the native call before MSI logging or installation began. `MsiOpenDatabaseW` and the
Crypt32 call receive CLR values through the shared typed reflection helper. It unwraps non-null
PowerShell adapters with `$value.PSObject.BaseObject`, checks each runtime type against the
reflected signature, and copies native by-reference outputs back to the caller's array. This
handles `PSObject` values retained inside PowerShell-created `object[]` arrays instead of relying
on a string cast alone.

The full subject, issuer, serial, thumbprint, published hash, and inspected MSI properties for
the four documented PhotoCraft and VectorCraft packages are in
[windows-msi-identities.json](windows-msi-identities.json). Those certificate values record the
inspected signing certificate; they are not a permanent leaf-certificate pin. Signer rotation
still requires a Windows-valid chain and the exact publisher identity. No certificate is
installed or added as a trust anchor.

Custom install destinations must be local, contain no reparse points, and have a safe leaf name.
The nearest existing directory must prevent unprivileged creation/replacement; ancestors must
prevent deletion, ACL, and owner changes. Unsafe destinations fail closed. The elevated process
checks whether each app is running immediately before installing it. Protected diagnostics, MSI
logs and results live beside the staged package under the OS system directory. `diagnostics.jsonl`
is created with a SYSTEM/Administrators-only ACL and records the validation stage, outer and root
exception types, both HRESULTs, and the root exception's actionable reason. The UI still receives
only fixed result codes and generic sanitized validation errors. The parent reads only this
administrator-owned result path after the elevated child exits; it never trusts a user-cache file
for privileged results. Success is only reported after normal installation detection sees the
advertised version.

The batch uses the same RAII Windows Installer queue as ordinary MSI installs, MSI removals, and
downgrade sessions. Self-update waits for all app jobs before replacing the launcher. Jobs can be
cancelled while downloading or waiting for the shared queue; declining UAC cancels the batch; and
each `/passive` MSI can be cancelled through its Windows Installer progress window. An MSI failure
is reported for that app while later authorized packages continue. Reboot-required codes are
preserved as successful installer outcomes, with `/norestart` preventing an automatic reboot.

## Scope and alternatives

Only ArtCraft's official `storytold/` releases enter the new batched elevated path. Approved
community apps retain their existing single-package installer path; this change does not infer a
publisher from a URL or broaden the privileged publisher policy. Per-user MSI updates keep their
existing non-elevated scope and do not join the elevated package set. Portable ZIP installs do
not use MSI batching. Consequently, one consent operation applies to eligible official,
machine-wide MSI updates in one `Update all` action when the launcher is unelevated and Windows
policy is configured to prompt; a separately handled community install may still use its existing
permission flow. Windows policy may suppress visible UI, as described above.

The native install log records the selected package format, detected MSI scope, official/community
classification, whether the package met the official machine-wide policy, whether it joined a
reserved batch slot, and an explicit fallback reason. It does not log user paths, package contents,
or checksum values as part of this routing decision. This makes per-user, community, non-MSI,
downgrade, and individual-install routes distinguishable without broadening the trust policy.

The approaches in [PR #311](https://github.com/storytold/craft-launcher/pull/311) and
[PR #345](https://github.com/storytold/craft-launcher/pull/345) were also reviewed. PR #311's
updater-service architecture adds a persistent privileged component; this implementation uses
the OS PowerShell host for a one-shot, policy-limited session instead. PR #345 adds portable
application installation and is not an elevation mechanism; the existing portable install path
remains outside MSI batching. The implementation reuses the current per-user/per-machine scope
detection, shared installer queue, Windows downgrade path, and catalog rather than replacing
those systems.

## Validation and remaining risks

`cargo xtask ci` is the repository gate. The existing Windows MSVC CI path should also compile
and run Windows-specific unit tests; repository policy prohibits adding a new CI workflow in a
feature/fix PR. Linux checks and cross-compilation cannot exercise UAC, Windows certificate-chain
validation, MSI Installer APIs under Windows, process races, or an actual MSI install. A Windows
runner that is already elevated can test protected staging and the production policy but does not
prove that an interactive UAC prompt appears exactly once. The manual acceptance steps are in
[windows-vm-test.md](windows-vm-test.md).

Windows-native tests exercise system PowerShell's reflection-generated MSI exports and argument
counts, call the real `MsiOpenDatabaseW` export, and cover scope/identity policy, signature
rejection, held-file sharing, and loopback staging without installing an app MSI. An opt-in ignored
test validates the documented PhotoCraft v0.13.0 and VectorCraft v0.15.0 release files using the
published checksums, the Windows Authenticode chain/publisher, and real MSI database APIs; run it
with `ARTCRAFT_TEST_OFFICIAL_MSI_DIR` set to the directory containing those files. The System32
staging test reports a skip from a local shell that is not already elevated; the hosted Windows
runner supplies the admin context and no test requests UAC.

An unsigned development launcher is safe to use for this test because the launcher is not the
elevated process or a trust input. The elevated path still independently requires the official
published hash, a Windows-valid Learning Machines Inc signature, and the strict MSI identity and
scope policy. Build and run it only in a disposable VM. Do not enable a development bypass or
weaken production signing rules. The supported `packaging/windows/package.ps1 -Arch x64` recipe
leaves outputs unsigned when signing secrets are absent and reports that fact; use its portable
ZIP for manual testing.

The publisher, GitHub release publication, TLS roots, Windows system binaries, and local
Administrators remain trust anchors. A malicious official release or trusted MSI custom action
can execute publisher code; this policy is not a sandbox. Another administrator can alter the
protected result directory or the operating system and is outside the standard-user threat
model. PowerShell execution policy, disabled networking, unavailable certificates, and
enterprise controls cause failure rather than a fallback that skips verification. Staging logs
are retained under the system directory for diagnosis and can be removed by an administrator or
by discarding the test VM.
