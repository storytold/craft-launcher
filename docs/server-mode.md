# Optional ArtCraft Launcher server

The default launcher builds need no server. The opt-in server hosts PhotoCraft, WordCraft,
GridCraft and CADCraft web builds with browser-side editing. A supported editor build must
expose its protocol v1 file-provider API. The server does not patch editor bundles.

## Build and configure

On a build machine with Rust, the WASM target and Trunk installed:

```sh
cargo build --locked --release -p artcraft-launcher-server --features server
cd apps/artcraft-launcher-web
trunk build --release --locked --cargo-profile wasm-release --features server
```

Transfer the native server binary for the host platform and the complete launcher site from
`dist/web/`. The site still uses the launcher's normal download catalog. Keep the site,
project data and editor artifacts in separate directories. The editor package entry point
must be `index.html` with relative asset URLs.

The server exits without starting if `enabled` is absent or false. An enabled example:

```json
{
  "enabled": true,
  "server": {
    "listen": "127.0.0.1:8767",
    "origin": "https://craft.example.com",
    "launcher_root": "launcher",
    "data_root": "projects",
    "artifact_root": "editor-releases",
    "worker_token_file": "secrets/pc-worker-token",
    "per_user_bytes": 1073741824,
    "total_file_bytes": 4294967296,
    "auth": {
      "issuer": "https://identity.example.com",
      "client_id": "artcraft-launcher",
      "client_secret_file": "secrets/oidc-client-secret",
      "allowed_subjects": ["editor-user-subject", "operator-subject"],
      "admin_subjects": ["operator-subject"],
      "idle_minutes": 60
    }
  }
}
```

Paths are relative to the configuration file unless absolute. Create the directories first
and use a dedicated service account. On Unix, restrict the data and editor release roots
to mode `0700` and secret files to `0600`. The worker token must be independently generated
and have at least 32 URL-safe characters. Keep both secrets outside the served roots.
The configured identity provider must support OIDC discovery and authorization code with PKCE.
Register exactly `https://craft.example.com/api/server/v1/auth/callback` with that provider.
Allowlist the provider's stable subject IDs. A verified email may appear in the account panel.
The storage identity comes from the verified issuer and subject.

Run `artcraft-launcher-server --config /path/to/config.json` behind an HTTPS reverse proxy
at the configured origin. Route that dedicated origin to the loopback listener. The proxy
must support requests of at least 96 MiB for artifact uploads and 80 MiB plus framing for
project saves. Keep the origin consistent across the launcher, API and editor frames.
Restrict access to the loopback listener and never log OIDC callback query strings, session
cookies or authorization headers. Keep user files and login/API responses out of shared caches.

## Files and sessions

Sign in through the cloud control. The launcher offers **Open** on cards with an active
hosted build. The floating control can be moved by dragging it or using its arrow keys.
It shows files for the signed-in user and lets that user open them or download a version.
Editor **Save** and **Save As** offer cloud storage or the device when the provider is connected.
Each editor continues to show local Recents separately from the provider's Recents.

Provider saves write exact native bytes and an immutable version in one SQLite transaction.
A document opened from the provider keeps that version as its basis for the next save.
If another session saved a newer version, reopen it or use **Save As** for a separate copy.
Project versions count toward both quotas. Files are limited to 80 MiB and each Recents
list to 100 entries. CADCraft accepts single-file DXF. Drawings with modeled external
references require a different package contract.

The server expires inactive sessions according to `idle_minutes`. Editing activity renews
the idle window and every session has a 24-hour absolute lifetime. The account panel
can end the server session immediately. After sign-out or expiry, export unsaved work
from the existing editor before navigating away. A fresh sign-in opens a new provider
connection. The server keeps sessions in memory, so a restart requires another sign-in.

## Editor updates

An administrator queues an app, an exact Git commit and its tree under **Updates**.
A paired worker on the build machine claims one job and supplies a manifest and the static
editor files. The server checks the source identity against the queued request and verifies
the size and SHA-256 of every file before the build becomes **Ready**. Use **Activate** to
switch future launches, or **Roll back** on the active build to restore its predecessor.
An already-open editor remains on its own build URL. The VM only verifies and serves
artifacts. The desktop installer, updater and signed release catalog use their existing paths.

Build the worker binary on the PC with `cargo build --locked --release
-p artcraft-launcher-server --features server --bin artcraft-launcher-pc-worker`.
An example worker configuration for one editor:

```json
{
  "origin": "https://craft.example.com",
  "token_file": "secrets/pc-worker-token",
  "scratch_root": "editor-builds",
  "git_program": "/usr/bin/git",
  "apps": {
    "gridcraft": {
      "repo": "/src/gridcraft",
      "allowed_ref": "refs/remotes/origin/main",
      "build": {
        "program": "/usr/bin/bash",
        "args": ["packaging/web/package.sh"],
        "output": "dist/web"
      }
    }
  }
}
```

Configure one reviewed recipe per editor. Its output path points to the complete static site,
including any boot code, split WASM parts and relative assets. Fetch the allowed Git ref on
the PC before running the worker. The worker verifies that the commit belongs to that ref and
matches the requested tree. It builds from a detached checkout and checks the source again
before upload. The server provides no executable commands or executable paths. The worker
runs only the local program and arguments from the operator's configuration.

Run `artcraft-launcher-pc-worker --config /path/to/worker.json` once for a queued job. One
worker uses a given scratch directory at a time. Configure build caches and the toolchain
on the build machine. Diagnostic tails are kept in the job's local `build.log`. If the worker
crashes, confirm its build is stopped before removing a stale `worker.lock` or starting
another invocation. The server reports an expired claim as interrupted. A staged build
requires operator activation even when the worker completed successfully.

Install only editor artifacts that the operator trusts to run on the launcher origin.
The bridge requires a same-origin editor frame. Code served at that origin shares access
to the browser APIs available to the signed-in user, including the file-provider capability.
Use a dedicated origin and review any scripts or custom assets before including them.
Verified checksums prove that the files received match the worker's manifest. Source and
build authorization are enforced separately by the operator's worker configuration.

Retained artifacts are limited to 64 manifests, 8 GiB total, 512 MiB per site and
96 MiB per file. The server preserves published build URLs. Keep backups of the project
database and artifact state. Stop the server or take a consistent SQLite backup before
copying databases. A restore needs the corresponding artifact files, not just the release index.

## Development validation

```sh
cargo test --locked -p artcraft-launcher-server --features server
cargo clippy --locked -p artcraft-launcher-server --features server --all-targets -- -D warnings
cargo check --locked -p artcraft-launcher-web --features server --target wasm32-unknown-unknown
cargo xtask ci
```

Validate a deployment on a separate origin before moving existing users. With actual supported
editor builds, sign in, save a native document, reopen it from external Recents and download
the stored version. Repeat for all four apps. Check the device destination, a version conflict,
a second user's library, sign-out, activation and rollback.
