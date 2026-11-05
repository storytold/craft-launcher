# Proposal: List LiteCADViewer as a community app

Please consider LiteCADViewer for the Community section in ArtCraft Launcher,
following the existing [SolidCraft community entry](https://github.com/storytold/craft-launcher/blob/main/crates/core/src/catalog.rs).
It gives people a lightweight way to open and review 2D DWG/DXF drawings on
Windows without starting the full CAD editor.

## Try it on Windows

- [LiteCADViewer 0.1.3 Windows x64 preview and SHA-256 checksums](https://github.com/Dennis-pan-x/LiteCADViewer/releases/tag/LiteCADViewer-v0.1.3).
  Extract the portable ZIP and run `LiteCADViewer.exe`.
- [Source](https://github.com/Dennis-pan-x/LiteCADViewer),
  [user guide](https://github.com/Dennis-pan-x/LiteCADViewer/blob/main/docs/usage.md),
  and [validation record](https://github.com/Dennis-pan-x/LiteCADViewer/blob/main/docs/validation.md).
- [Viewer screenshot](https://github.com/Dennis-pan-x/LiteCADViewer/blob/main/docs/images/viewer.png).

The preview supports 2D viewing, pan/zoom/fit, layers, model/layout selection,
distance measurement, and PDF export. GPU rendering has a CPU fallback. It
does not edit or save source drawings.

## What changed in 0.1.3

On one Windows x64 system with an RTX 5060 (DirectX 12), the packaged
0.1.3 preview took 16.6 ms median per update across 30 near-fit pan/zoom
steps on a roughly 30 MB DWG, compared with 544 ms in the old-build run.
The drawing canvas matched pixel for pixel, and the source hash stayed
unchanged. The drawing is private and is not distributed.

Packaged DWG parsing took 3.7 seconds versus 5.2 seconds in the old run.
Time to first view varied: 6.1 seconds on the locally installed optimization
build and 9.3 seconds in the packaged run, versus 8.2 seconds in the old run.

The speedup comes from GPU depth ordering for opaque geometry, preserving
painter order for transparency, and avoiding intermediate DXF tag copies.
These measurements apply to the tested drawing and camera path, not all DWGs.
Native acceptance covers Windows x64. SHX/big fonts, proxy objects, complex
layouts and 3D still need independent compatibility checks.

## Publisher and provenance

[Dennis-pan (@Dennis-pan-x)](https://github.com/Dennis-pan-x) maintains the
repository, created on 2026-10-19 as a CADCraft fork. The viewer retains eight
CADCraft-derived core crates and uses the engine-free `render-wgpu` backend
extracted during viewer development. Its standalone frontend, icon, and
releases are maintained separately from CADCraft. The full editor, editing
engine, constraint solver and MCP service are not included.

The project and its original peregrine icon use MIT OR Apache-2.0; upstream
notices are retained. Unmodified acadrust remains MPL-2.0. See the
[provenance](https://github.com/Dennis-pan-x/LiteCADViewer/blob/main/docs/provenance.md)
and [third-party notices](https://github.com/Dennis-pan-x/LiteCADViewer/blob/main/THIRD-PARTY-LICENSES.txt).
The Windows preview is unsigned; checksums verify download integrity and do
not authenticate the publisher.

## Decision requested

Would maintainers approve LiteCADViewer as a separate community app, with
Windows x64 as the initial scope? A link to the standalone preview could
give users a download route while installer requirements are decided.
If launcher-managed installation is preferred, please specify the expected
Windows package, signing, asset naming, and update/uninstall requirements.

This PR only documents the proposal. A catalog row or a pinned download
source would follow maintainer approval. It does not change the launcher's
trust rules, official suite, or installer behavior.

Related: [Launcher issue #623](https://github.com/storytold/craft-launcher/issues/623)
and [CADCraft viewer component PR #1606](https://github.com/storytold/cadcraft/pull/1606).
