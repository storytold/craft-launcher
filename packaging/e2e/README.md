# Release candidate qualification

These tests install the **packaged ArtCraft Launcher**, execute that installed binary, and use
its ordinary native installer to manage real pinned FigCraft releases. They do not build an
unsigned substitute, enable `local-mirror`, stub an installer, or disable verification. A green
build alone is not a qualification result.

The accompanying `craft-common-actions` change must land before this repository enables
`launcher-qualification: true`. Keep this work separate from the small macOS verifier hotfix.
It is not a prerequisite for landing that fix. Neither branch has to publish a release to test
the qualification implementation.

## Execution and cost

PRs retain the existing Linux/Windows Rust tests and add a seconds-long Python evidence-contract
check through the shared workflow. No extra macOS builds on PRs. Full qualification follows the
existing release candidate build, including manually dispatched release workflow runs. It uses
the same run's uploaded packages, with no redundant Rust builds and no signing secrets in test
jobs. Seven standard hosted runners exercise native architectures; Windows x86 runs as a real
32-bit process on the x64 runner. The macOS universal package runs on both Intel and ARM.
The candidate version must match the workspace version compiled into the binary. A packaging-only
version override that relabels different binary contents intentionally fails qualification.

Manual dispatch defaults to `create-draft: false`: it builds, qualifies and retains evidence but
does not create/edit a GitHub release or tag. The read-only `verify` job still compares the
candidate bytes and all reports in this mode. A deliberate release-branch push or explicit
`create-draft: true` dispatch permits draft creation only after the same checks succeed. Nothing
in this suite publishes a draft. Do not use a release-branch push to stage a qualification-only run.

| Runner target | Launcher packages installed, executed, removed | App lifecycle formats |
| --- | --- | --- |
| macOS ARM64, x64 | DMG → copied `.app` | DMG |
| Windows x64, x86, ARM64 | MSI and portable ZIP | MSI and portable ZIP |
| Linux x86_64, aarch64 | DEB, AppImage, tar.gz | DEB and AppImage |

The exact targets, filenames and operation sequence are in `contract.py`. Runner labels follow
[GitHub's hosted runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
No schedule is added. Test jobs have a 40-minute deadline and retain small diagnostic logs for
14 days; they do not upload installed payloads or credentials. Fixture transfers dominate test
cost. Do not cache installed state across jobs or let failed tests silently become optional.

## What is exercised

Each app operation starts a new installed-launcher process. It reads persisted installation
records and detects the actual installation again. The flow includes:

- Fresh launcher installation, version and build-SHA/architecture checks, native GUI initialization
  and multiple UI passes, close and reopen, launcher removal and platform registration checks.
  macOS preserves the package's copy metadata, checks codesign/stapler and, when enabled, performs
  a real Gatekeeper assessment of the installed launcher bundle.
- FigCraft 0.12.1 installation, executable version probing, cancellation with partial-file removal,
  real checksum rejection before the OS installer, pause/resume, update to 0.13.1 and persisted detection.
- Older-version rejection, corrupt cached-artifact recovery, same-version reinstall, app launch,
  running-app update/removal protection for formats the launcher itself manages, uninstall twice,
  reinstall after removal and final cleanup. dpkg owns running-process policy for Debian packages;
  that format tests launch/teardown without asserting the portable install guard.
- User-document sentinels throughout, and unrecorded personal files **inside** Windows portable
  application installations surviving update and removal. AppImage integration files must disappear
  when the app is removed. The native app installer is called with the same transactional record
  commit callback as the desktop backend.
- Launcher updater replacement and helper restart on macOS `.app`, Windows portable ZIP and Linux
  AppImage. A real published launcher 0.9.0 installation is the destination; the candidate's code
  runs its production updater. The exact candidate package enters the ordinary SHA-verified cache
  because it is not published yet. Existing signature, version, Windows helper ownership and
  publisher checks stay enabled. The existing pretend-version developer switch models the old
  installation while executing the candidate's updater. The test observes changed executable bytes,
  a restarted process, candidate version/build SHA/architecture and preserved settings sentinels.

`--qualify-install`, `--qualify-ui` and `--qualify-self-update` are explicit qualification entry
points in the ordinary binary, not a special release build. They accept only catalog-pinned
fixtures/package kinds and fresh owned folders within `RUNNER_TEMP`. Guard variables prevent
accidental workstation use; they are not security boundaries. Never spoof them on a personal
computer. System DEBs and MSI registrations, macOS Launch Services, and Windows Known Folders
require a truly disposable runner account/VM in addition to temporary homes and destinations.
Launch Services can give the started app the runner account's actual home; the VM is the isolation
boundary in that case. Cleanup uses the real uninstall path; failed cleanup fails qualification.
The external runner also checks a surviving process at the exact installed app executable path;
production's running-app detection alone is not the launch evidence. Process inventories stay
in memory and are not included in uploaded logs.

## Release gate and artifact identity

The shared `release.yml` waits for the entire qualification matrix to **succeed** before its
draft job runs. Failure, cancellation and a skipped qualification job cannot satisfy the gate.
The draft action downloads the same run's evidence and, before any GitHub release mutation,
executes `verify_release.py`. It requires every target and every ordered lifecycle step, cleanup,
GUI startup and self-update evidence, including each saved native operation result, both GUI-start
records per applicable format and the native updater's scheduling result. A green summary with
missing or inconsistent underlying results fails. Reports must match the current commit, run ID and version,
and their SHA-256 hashes must match the exact files being uploaded. No rebuild or artifact
replacement is permitted between qualification and upload. `launcher-qualification.json` is
included with the release assets so the evidence and limits travel with the candidate.
Every launcher package, including the Linux tarball, also saves a native identity result tied to
that package's name/hash. A same-version stale binary or wrong architecture cannot qualify merely
by printing a plausible `--version` line.

GitHub's artifact digest mismatch is currently a warning, so the gate performs an explicit
failing comparison rather than relying on the download action alone. See
[GitHub's artifact documentation](https://docs.github.com/en/actions/tutorials/store-and-share-data).
This gates this repository's workflow. It cannot stop a maintainer manually publishing an
unqualified release through the UI/API; no repository protections or access settings are changed.
These reports are assertions by trusted repository code running in Actions, not cryptographic
attestations against a malicious workflow or malicious source commit. The gate protects against
missing, failed, stale, inconsistent or replaced candidates under that trust model.

## Staging order

1. Review and land the opt-in shared-workflow interfaces in `craft-common-actions`. Existing app
   callers retain their defaults; no launcher qualification is activated by that change alone.
2. Integrate the separate verifier hotfix and this launcher branch, resolving any version/base
   drift. The shared interfaces must already exist because this repository follows shared `@main`.
3. After authorization, manually dispatch the launcher workflow on the exact reviewed commit with
   `create-draft: false`. Inspect all seven jobs and `launcher-candidate-evidence`; missing or failed
   jobs are blockers. No hosted execution has been established by local contract tests.
4. After successful staging, an authorized release-branch push or explicit draft-enabled run
   builds and qualifies its own candidate once, then uploads those exact bytes. Evidence from the
   staging run cannot qualify a rebuild or a different run. Cross-run promotion is not implemented.

## Fixtures

`fixtures/figcraft-0.12.1.sha256`, `figcraft-0.13.1.sha256` and `launcher-0.9.0.sha256` are the
unmodified published `SHA256SUMS.txt` manifests from the corresponding `storytold` GitHub releases.
These fixture sources are existing catalog apps, not a new download source. Update pins deliberately
and review the new versions, hashes and complete architecture/format set. Unavailable/mutated
fixtures fail; there is no fallback to `latest`, no retry with weaker trust and no success-by-skip.

## Gatekeeper-disabled qualification

Standard macOS jobs assert `spctl --status` is enabled. The runner has an explicit
`--gatekeeper disabled` assertion for a **separately provisioned disposable VM** whose status is
already disabled. It never executes `spctl --global-disable`, edits a trust database, or removes
quarantine. A disabled-policy run produces separate evidence and cannot replace the standard
enabled-policy gate. Recent macOS versions may require interactive approval for policy changes;
provision a disposable image with that choice already made. Do not change a personal Mac to run it.
The status assertion accepts exit 0 or 1 and requires the exact expected stdout; disabled policy
can return exit 1. A real subprocess regression tests output retention through the Python command
wrapper. Unknown output still fails, and native installs use the production verifier unchanged.

The verifier unit tests belong to the separate verifier change. Their simulated status cases are
useful regression tests, but must not be reported as actual disabled-policy OS execution. The
standard hosted matrix does not yet supply such an image. An operator can register an **ephemeral,
single-job macOS VM** runner, explicitly set `ARTCRAFT_QUALIFICATION_DISPOSABLE_VM=1` in that job,
download the same run's `release-macos` artifact and invoke `runner.py --gatekeeper disabled` with
the matching version/source SHA/run ID and target. The ordinary `GITHUB_ACTIONS` and
`RUNNER_ENVIRONMENT=self-hosted` values must come from Actions. Destroy the VM after the job.
The opt-in flag does not establish VM isolation itself; review the runner/image contract before
using it. No such runner is provisioned by this change. Never set that flag on a personal machine
or persistent shared runner. Retain this report separately from enabled-policy gate reports.

## Local verification and limits

```sh
python3 -m unittest discover -s packaging/e2e -p 'test_*.py' -v
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo xtask wasm
actionlint
shellcheck packaging/*.sh packaging/*/*.sh packaging/linux/AppRun
```

The Python tests test the evidence gate; synthetic reports in those unit tests are **not** OS
installation evidence. A hosted candidate run is still required after integration. This suite
reduces regression risk; it cannot ensure the launcher is perfect.

The native tests establish fixture selection and local path/activation invariants. Positive
installer/launch claims come only from `runner.py` executing the installed candidate on the target
OS. Negative checksum data, deliberate cache corruption and pause/cancel requests are controlled
fault injection through production code, not substitutes for positive installation. The updater's
candidate release metadata/cache is constructed by the harness; discovery and fresh updater
transfer are not exercised. Data sentinels prove file preservation, not that the UI reloads every
setting. GUI callbacks prove native startup, not pixel correctness or full UI interaction.

Remaining coverage boundaries are explicit in every release evidence summary: full accessibility
or pointer-driven UI install/update dialogs, updater release discovery/fresh network transfer,
MSI/DEB/Flatpak self-update and privilege migration, actual disabled-Gatekeeper OS execution,
and lifecycle execution of RPM/Flatpak/Arch packages, RISC-V, FreeBSD and Web. Those formats retain
their existing build/static gates. Adding them requires real package-manager/sandbox runners,
not extracting archives and calling that an installation. The app launch assertion proves the
production launch path starts a surviving process; it does not prove all functions of FigCraft.
