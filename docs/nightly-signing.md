# Nightly artifact admission

New Nightly installs and updates require `craft-nightly/v2`. No unsigned v1 fallback
exists, even for debug/local-mirror builds. Existing copies, ownership records and
profiles are preserved. Open, Reveal and Uninstall retain their existing ownership
and isolation checks; they do not need a new release descriptor.

Each Linux `tar_gz` artifact requires a `signature` Download record: `name`, `url`,
`size`, `sha256`. Its exact name is `<archive name>.asc`, published uniquely in the
same immutable repository/tag with a checksum, at most 16 KiB. It must be ASCII-armored
OpenPGP detached binary signature over the final compressed archive. Unknown fields,
unsigned schemas, missing/duplicate/foreign signatures and binding mismatches refuse.

## Public trust provisioning

`crates/native/src/nightly_trust.rs` is source-owned. Defaults are unavailable and
refuse install/update. A reviewed source change must supply an independently trusted
Linux armored public key **and full primary-key fingerprint**, and a Windows publisher:
the **exact full native SignerCertificate.Subject** string, the root the signer's chain
must end at (its exact Subject **and the root certificate's SHA-256**), and the enhanced
key usages the signer certificate must carry. Never derive these from a feed, downloaded
release key, unsigned Launcher, environment variable or debug/local setting. Public
provisioning must include current revocation/expiry state; there is no release-selected
key retrieval or automatic trust-root refresh.

Linux is provisioned with the ArtCraft Linux Release Signing Key, the same key that signs
stable Linux releases, pinned by its primary fingerprint; its signing subkey makes the
signatures. A test checks a real signed stable release file against the configured signer.
Infra, who hold the key, handed over the public key and primary fingerprint, and the same
key is the `GPGKey` in getartcraft.com's artcraft-oci.flatpakrepo. The copy published with
PhotoCraft releases is vouched for only by its own signature, so it is not the provenance.

Windows is provisioned with PhotoCraft's Azure Trusted Signing (Microsoft ID Verified)
identity, read on 2026-11-23 from the embedded signature of `photocraft.exe` in PhotoCraft
v0.23.1's `photocraft-0.23.1-windows-x64-portable.zip` (archive SHA-256 `6317e764…60b1`,
executable SHA-256 `c118ea72…9165`), and confirmed by a Windows 11 reading of the same file:

- Subject `CN=Learning Machines Inc, O=Learning Machines Inc, L=Atlanta, S=Georgia, C=US`.
- Chain: the leaf, Microsoft ID Verified CS EOC CA 03, Microsoft ID Verified Code Signing
  PCA 2021, and the root Microsoft Identity Verification Root Certificate Authority 2020,
  SHA-256 `5367f20c…1270` (the same as Microsoft's published certificate).
- Enhanced key usages: Code Signing, `1.3.6.1.4.1.311.97.1.0`, and the identity's own
  `1.3.6.1.4.1.311.97.627281783.214399755.66947568.492752054`.

The leaf lasts three days and is renewed for each signing run, and the issuing CAs rotate,
so neither is pinned. The pin is the subject, Code Signing plus the identity EKU, and the
root by name and hash. A timestamped signature stays valid after its leaf expires.

Linux uses installed `/usr/bin/gpg`, no user options/keyrings or automatic retrieval.
A private temporary home imports only the pinned public material. Bounded subprocess
verification requires one valid binary SHA-2 signature from the full pinned primary
identity (including signing-subkey mapping), with no revoked/expired/error status.
A private checksum-bound archive copy is verified before unpack, and rechecked before
extraction and before any old files move. Missing tooling/trust or any error refuses.
No cryptographic packet parser or new dependency was added.

Windows preserves the strict flat inventory: only `photocraft.exe` plus the explicitly
permitted textual notices. The staged EXE is held without write/delete sharing through
verification and copy/commit. Existing verifysign is used with explicit native chain
revocation, not convenience defaults. A fixed non-elevated read-only system PowerShell
query requires Valid embedded Authenticode, a timestamp certificate and case-sensitive
full Subject equality. It also rebuilds the signer's chain from the signature's own
certificates (time validity ignored, revocation already checked) and reports the root's
Subject and SHA-256 and the leaf's key usages, which must match the pin. The query may
take up to 180 seconds (a cold run on a busy machine took 81); past that it is stopped
and the message says it timed out, not that the signature was refused. Open checks the
installed copy the same way. Its leaf SHA-256 must match the native verifier's actual leaf;
this binds the two checks, not a rotating certificate pin. A full DN is not inferred
from limited CN/O/OU/C fields. Native timestamp/signing-time policy is retained.
The query uses trusted built-in modules, no profile, path-as-data, and no downloaded
script/feed command. Unavailable/unknown/error evidence refuses before replacement.

macOS retains strict/deep code integrity, pinned Nightly bundle/team, notarization,
exact full version/architecture and existing assessment policy. The team is
`nightly_trust::MAC_TEAM`; new installs are admitted only while it is set and equals
stable PhotoCraft's Developer ID team, and the stable Mac checks (`codesign`, then
`spctl --assess` at `/usr/sbin/spctl`) run before the copy is swapped in and on every Open.

## Evidence limits and fixtures

Synthetic descriptors/archives and disposable generated OpenPGP test keys are authored
fixtures, not production trust. Tests cover exact bytes, primary/subkey binding and
fail-before-swap preservation. Test keys never populate production inputs. Genuine
Windows signing, macOS signing/notarization, production-key Linux and live-feed admission
remain separate acceptance checks. Mock native metadata and disposable-key success do
not prove them. The external frozen unsigned diagnostic fixture remains unchanged.

## Required Windows operational fixture and CI gap

The native test `actual_windows_signed_fixture_verifies_metadata_and_is_held_until_released`
uses the independently pinned external reference in
`crates/native/tests/fixtures/windows-signed-reference.json`, not an assumption that
stock Windows files have embedded signatures. Many are catalog-signed and correctly
fail production admission. The reference is never executed by this test.

Provision the original official PowerShell v7.7.0 Windows x64 ZIP from the manifest,
verify its archive SHA-256 before extracting root `pwsh.exe`, and retain upstream
MIT licence/notices with the external resource. The test checks that entry's exact
size/hash, full subject, actual native chain/revocation trust, embedded timestamped
metadata and same-leaf binding before exercising the actual hold/release behavior.
Do not substitute another installed PowerShell or vendor the binary here.

Set `CRAFT_NIGHTLY_SIGNED_FIXTURE` to the absolute executable path and
`CRAFT_NIGHTLY_SIGNED_FIXTURE_REQUIRED=1` in the required Windows run. Required unset
or empty permits an absent-resource SKIP reason printed directly to the run output;
`1` fails when missing, and every other value fails. Any configured bad, corrupt or
untrusted fixture fails even when required is unset. This input is **cfg(test) only**;
production signer configuration is still source-owned and unavailable by default.

Plain libtest counts a test function returning after a resource SKIP as `ok`; that
count is **not operational coverage**. The explicit SKIP line must be retained, and
only the required=1 run with no SKIP can establish this native path was exercised.
The Windows CI hook now runs `packaging/windows/prepare-signed-test-fixture.ps1`
and its small data-only regression script before the existing Clippy/tests. The shared
gate runs that hook in bash: it normalizes
the fixture directory with `cygpath -aw`, exports the exact native Windows file path
and required=1 **before** setup, and passes the same directory explicitly to the
script. Setup failure stops the command chain. No child-environment or GITHUB_ENV
propagation is assumed. Local `cargo test`/`cargo xtask ci` still use the resource
contract above; they do not download the reference automatically.

The setup streams at most the pinned archive length with a five-minute cancellation
bound, verifies its exact length/hash before ZIP parsing, rejects duplicate/case-aliased
entries, and extracts only the exact root executable plus `LICENSE.txt` and
`ThirdPartyNotices.txt`. It checks executable size/hash, preserves notices unchanged,
refuses an existing final destination, and publishes its own staging directory only
on success. The fixture is never executed; no secrets, latest endpoint, broad unpack,
binary vendoring or skip fallback is involved.

Data-only regressions run with `pwsh -NoProfile -File
packaging/windows/test-signed-test-fixture.ps1 -OutputDirectory <owned scratch>`.
The CI hook is source-wired, but its **hosted Windows execution remains pending an
approved push**. Local script checks and the separately accepted private required
native run do not constitute a hosted CI pass or shipping-ready release acceptance.
