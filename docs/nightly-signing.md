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
Linux armored public key **and full primary-key fingerprint**, and Windows **exact full
native SignerCertificate.Subject** string. Never derive these from a feed, downloaded
release key, unsigned Launcher, environment variable or debug/local setting. Public
provisioning must include current revocation/expiry state; there is no release-selected
key retrieval or automatic trust-root refresh.

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
full Subject equality. Its leaf SHA-256 must match the native verifier's actual leaf;
this binds the two checks, not a rotating certificate pin. A full DN is not inferred
from limited CN/O/OU/C fields. Native timestamp/signing-time policy is retained.
The query uses trusted built-in modules, no profile, path-as-data, and no downloaded
script/feed command. Unavailable/unknown/error evidence refuses before replacement.

macOS retains strict/deep code integrity, pinned Nightly bundle/team, notarization,
exact full version/architecture and existing assessment policy.

## Evidence limits and fixtures

Synthetic descriptors/archives and disposable generated OpenPGP test keys are authored
fixtures, not production trust. Tests cover exact bytes, primary/subkey binding and
fail-before-swap preservation. Test keys never populate production inputs. Genuine
Windows signing, macOS signing/notarization, production-key Linux and live-feed admission
remain separate acceptance checks. Mock native metadata and disposable-key success do
not prove them. The external frozen unsigned diagnostic fixture remains unchanged.
