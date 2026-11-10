# Proposal: an ArtCraft-owned wgpu fork for OCLP Macs

ArtCraft Launcher currently disables wgpu's internal indirect-call validation on
Intel Macs to avoid Metal compiler failures. This proposal uses compiler
workarounds in wgpu instead, restoring the normal validation settings.

## Prototype dependency

The workspace patches `wgpu` to this immutable commit:

- Repository: <https://github.com/ITJamie/wgpu-fork>
- Branch: `codex/metal-msl-intel-v30.0.1`
- Commit: `21c08e23135ba27ed1c6b68177ecc20c0d076dd9`
- Upstream proposals: <https://github.com/gfx-rs/wgpu/pull/10725> and
  <https://github.com/gfx-rs/wgpu/pull/10731>

The release backport is based on upstream tag `v30.0.1`. It probes
compiler-supported Metal Shading Language versions,
works around the Intel workgroup attribute compiler failure using the pipeline
descriptor, and emits runtime-sized storage buffers as element pointers on Intel.
Internal wgpu indirect-call validation and shader bounds checks remain enabled.
The workspace resolves wgpu 30.0.1 from this fork instead of the registry,
preserving the released API used by the existing egui fork. No egui fork change
is needed.

## Proposed organization ownership

A maintainer should create a `storytold/wgpu` fork and preserve the backport
branch and commit above. Creating that repository is an organization decision;
this proposal does not assume it already exists.

After the commit is available in the organization fork, change the patch URL in
`Cargo.toml` to `https://github.com/storytold/wgpu`, retain the reviewed `rev`,
and regenerate `Cargo.lock`. Keep a full commit pin rather than a moving branch.
No launcher-specific changes need to be added to the wgpu fork.

Maintain the fork as a temporary compatibility dependency. Follow both upstream
PRs, review upstream updates, and remove the patch once an upstream release
contains both fixes and the OCLP hardware checks pass against that release.

## Build and hardware validation

```sh
cargo build --locked -p artcraft-launcher
cargo xtask ci
```

Run the desktop binary from a logged-in graphical session. Test both Intel Iris
Pro and AMD Radeon R9 M370X on the OCLP Mac, and check ordinary Apple hardware
before adopting the fork for releases. Confirm that the window renders, resizes,
and reopens from the tray. Building successfully alone does not prove rendering.

```sh
WGPU_BACKEND=metal WGPU_VALIDATION_INDIRECT_CALL=1 \
  MTL_DEBUG_LAYER=0 MTL_SHADER_VALIDATION=0 \
  cargo run --locked -p artcraft-launcher
```

The patched Metal framework on the test OCLP Mac crashes with external Metal API
or shader validation enabled. These diagnostic environment settings disable
those external layers, not wgpu's internal indirect-call validation. They do not
establish that every OCLP configuration is supported. Do not disable validation
globally in production code as a substitute for a compiler workaround.

Use `WGPU_POWER_PREF=low` to prefer Intel Iris Pro and `WGPU_POWER_PREF=high`
to prefer the AMD GPU on the test Mac. Confirm the selected adapter in debug logs.

## Recorded prototype results

On a MacBookPro11,5 with macOS 15.8.1 through OCLP, the locked desktop build
completed. `cargo xtask ci` passed formatting, Clippy, workspace tests, and both
WebAssembly checks. The contributor visually confirmed the launcher UI in both the
high-power and low-power runs; debug logs confirmed Intel Iris Pro selection in
the latter. Both runs kept internal indirect-call validation enabled and used
the external-layer environment settings above. Portable test data was isolated
from the installed launcher's data. These were startup/UI smoke checks, not a
full install/update, resize, or tray lifecycle test.

The debug runs took about 35–45 seconds to reach swapchain creation. Startup
performance remains a limitation of this prototype.

The backport's full wgpu suite on Apple M4 Max ran 1,010 tests: 1,002 passed,
eight failed, and six were skipped. The failures were:

- `subgroup_operations`: an expected-failure mismatch.
- `all_passthrough_shaders_binary`, `all_passthrough_shaders_source`, and
  `metallib_passthrough_shader`: missing DXC or offline Metal tools.
- `binding_array_storage_buffers`, `partial_binding_array_storage_buffers`,
  `validate_capabilities`, and the `Computepass Encoding` benchmark: untriaged
  binding-array/capability failures.

Affected-crate Clippy passed using the release's Rust 1.93 toolchain. The new
MSL controls and compute-workgroup regression tests passed in that full run.
This is not a clean full-suite pass or a claim that the fork is release-ready.

The dependency patch applies to the shared desktop and web dependency graph.
The existing `cargo xtask wasm` check must continue to pass; other desktop
platforms still need their normal CI coverage before merging.
