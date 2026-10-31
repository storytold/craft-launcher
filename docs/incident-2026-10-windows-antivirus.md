# Incident: antivirus flagged ArtCraft Launcher during Update all on Windows

Status (2026-10-31): the Update all route is reverted on `main` (#1077) but not yet released. The
same elevated PowerShell route is still used by all-users downgrades, and no test has confirmed
that the revert stops the detection. See [Still open](#still-open).

## Summary

Since v0.10.0, updating two or more all-users apps together normally ran installers through a hidden,
elevated PowerShell with a Base64 script. Users soon reported antivirus flags during Update all.
Kaspersky's report in #1045 shows a behavioural `PDM:Trojan.Win32.Generic` detection. It killed
and deleted `artcraft-launcher.exe` and removed its shortcut and registry entry, and the reporter
had to reinstall ArtCraft Launcher and every app.

Nothing found so far suggests malicious intent: this code only ran `msiexec` on packages checked
against their release checksums, and Windows still showed a permission prompt. The route is treated
as the cause because the evidence below points to it, not because the detection has been reproduced.

## What triggered it

#974 added one permission prompt for an Update all of all-users apps. For two or more apps, unless
the batch was too long or PowerShell could not start (each app then got its own `msiexec` prompt),
ArtCraft Launcher:

1. started a hidden `powershell.exe`;
2. asked for elevation with `Start-Process -Verb RunAs -WindowStyle Hidden`;
3. ran a script passed as `-EncodedCommand`;
4. ran `msiexec` from that script on packages in a user-writable folder.

Each step had a reason, but together they match a common malware pattern. A single app kept going
straight to `msiexec`, without PowerShell. All-users downgrades (`46cffe6`, in #1007) reuse the
same elevated script code.

## Timeline (UTC)

| When | What |
|---|---|
| Oct 29 04:05 | #974 merged |
| Oct 29 19:47 | v0.10.0 released, the first release with this route |
| Oct 29 22:41 | #1027: antivirus flags Update all, but updating one at a time is fine |
| Oct 30 06:54 | #1042: antivirus removes files during Update all |
| Oct 30 09:27 | #1045: Kaspersky reports a Trojan during Update all |
| Oct 31 03:48 | v0.11.0 published, still with this route; later revoked |
| Oct 31 05:31 | #1077 merged, reverting #974 |

## Evidence

Pointing to this route:

- The Update all reports began about three hours after v0.10.0 was released.
- #1027 separates the two routes: one at a time works, and Update all gets flagged.
- The #1045 detection is behavioural, so it depends on what ArtCraft Launcher does when it runs.

Not established:

- The detection has not been reproduced on a machine under our control, before or after the revert.
- The reports in #1027 and #1042 do not name a detection, so they may not share the #1045 cause.

The Avast `FileRepMalware` flag in #251 predates this route and is a separate reputation flag.

## File scans didn't detect this

On 2026-10-31, no VirusTotal engine flagged these builds, and Kaspersky's engine showed
`undetected` on all of them:

| Build | Has the route | Malicious / suspicious | Source |
|---|---|---|---|
| v0.9.0 MSI | No | 0 / 0 | Existing report |
| v0.9.0 zip | No | 0 / 0 | Existing report |
| v0.10.0 MSI | Yes | 0 / 0 | Existing report |
| v0.11.0 MSI | Yes | 0 / 0 | Existing report |
| v0.11.0 zip | Yes | 0 / 0 | New scan |

VirusTotal's sandbox run of the v0.11.0 MSI shows no PowerShell, because the route needs two or
more outdated all-users apps and someone to accept the prompt. These clean scans therefore say
little about this kind of detection, and neither our Windows CI nor these VirusTotal results flagged
the route.

## What changed

- #1077 reverts #974, so each all-users update runs `msiexec` directly again. This brings back one
  permission prompt per app.
- v0.11.0 was revoked, and its release page points users to v0.10.0. v0.10.0 also contains this
  route, so its users stay exposed until a release with #1077 ships.
- #1076, an alternative that also removed the downgrade route, was closed in favour of #1077.
- #1082: `msi_context.rs` ran a hidden, non-elevated PowerShell lookup on MSI updates. It now
  reads Windows Installer's own registration keys for this account and the machine instead, so no
  update starts PowerShell.

## Still open

- **All-users downgrades:** `downgrade_elevated` still uses the same elevated PowerShell script,
  and the command strings are still in the binary. Anyone downgrading an all-users app could trigger
  the same detection.
- **Release:** no release includes #1077 yet.
- **Confirmation:** test on a Windows VM with Kaspersky and behaviour detection on. Run Update all
  with two or more outdated all-users apps on an affected build, then on a build with #1077. The
  test only counts if the affected build reproduces the flag.
- **Affected users:** the revert does not repair installs the antivirus has already damaged. The
  "isn't where it was installed" errors are tracked in #993.

## For next time

- Don't start hidden, elevated or encoded PowerShell from ArtCraft Launcher. Prefer one prompt per app
  to a script that behaves like malware. Bringing back a single prompt needs a different design,
  such as a signed helper.
- Before releasing a change to how Windows installs run, try it on a machine with behavioural
  antivirus on. File scans and CI didn't show this problem.
