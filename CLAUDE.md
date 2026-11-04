Read [AGENTS.md](AGENTS.md) first: it is the guide for every agent and contributor working in this repository.

New workspace crates must include `[lints] workspace = true` in `Cargo.toml` to inherit
`[workspace.lints]`. Production crate roots must declare the required crate-level lint attributes
before modules or other crate items; crate-level `//!` documentation may precede them.

## CI workflows
- Workflows in `.github/workflows/` call the shared workflows in [`storytold/craft-common-actions`](https://github.com/storytold/craft-common-actions) at `@main`, with their inputs and secrets. No other jobs: if no shared workflow does what's needed, it doesn't go here.
- Don't add or change CI in a feature or fix PR. If the existing workflows can't build or test a change, link an existing craft-common-actions issue or PR, or open a new issue, or a PR only for a small fix that is safe for every app and does not increase CI time.
- Some workflow files here still run their own jobs. Don't add to them. Moving a job into craft-common-actions is its own task, never part of another PR, and the job stays here until a shared workflow runs it.
