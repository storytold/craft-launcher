# Proposal: LiteCADViewer as a lightweight CAD companion

Status: proposed community contribution; requires maintainer review. This document
neither adds an official Craft App nor authorizes a new download source.

## Contributor and provenance

- Maintainer: [Dennis-pan (@Dennis-pan-x)](https://github.com/Dennis-pan-x).
- Source: [Dennis-pan-x/LiteCADViewer](https://github.com/Dennis-pan-x/LiteCADViewer).
- The repository was created on 2026-10-19 as a fork of `storytold/cadcraft`.
  Its default branch is now a standalone viewer; original history and the
  CADCraft upstream branch remain available. This is a new community project.
- Windows previews are unsigned portable ZIPs. SHA-256 checksums provide
  integrity checks, not independent publisher authentication.
- [Provenance](https://github.com/Dennis-pan-x/LiteCADViewer/blob/main/docs/provenance.md)
  and [licenses](https://github.com/Dennis-pan-x/LiteCADViewer/blob/main/THIRD-PARTY-LICENSES.txt)
  preserve the original CADCraft authorship. The workspace uses MIT OR Apache-2.0;
  unmodified acadrust retains its MPL-2.0 notices.

## Why a separate viewer

For someone reviewing a drawing, the useful first action is opening the file and
panning or zooming, without loading editing commands and constraint solving.
LiteCADViewer reuses nine CADCraft core crates for reading and rendering, with a
small native Rust/egui frontend. It does not include the CADCraft editing engine,
constraint solver, editor UI or MCP service. It has no source-file save command.

The current preview provides DWG/DXF viewing, pan and zoom, fit view, layer
visibility, measurement and PDF export. It uses GPU geometry rendering
with a CPU fallback. The interface and project documentation are English.
Its original cyan peregrine-falcon icon is separately licensed; it does not
reuse the ArtCraft logo or the CADCraft application icon.

## Review the existing application

- [Windows x64 previews and SHA256SUMS.txt](https://github.com/Dennis-pan-x/LiteCADViewer/releases/latest).
- [User guide](https://github.com/Dennis-pan-x/LiteCADViewer/blob/main/docs/usage.md).
- [Validation and limitations](https://github.com/Dennis-pan-x/LiteCADViewer/blob/main/docs/validation.md).
- [Native screenshot](https://github.com/Dennis-pan-x/LiteCADViewer/blob/main/docs/images/viewer.png).
- [Existing CADCraft viewer code contribution](https://github.com/storytold/cadcraft/pull/1606).
- [Launcher discussion](https://github.com/storytold/craft-launcher/issues/623).

The synthetic DWG checks use files produced by CADCraft's own converter. They do
not establish independent DWG compatibility. SHX/big fonts, proxy/custom objects,
complex layouts and 3D need independent drawing checks. Published runtime
acceptance currently covers Windows x64 only. Navigation timings on one computer
are not a general performance or startup-speed guarantee.

## Decision requested from maintainers

Please consider whether a lightweight read-only CAD companion belongs in the
community roadmap, and whether the preferred home is CADCraft or a separate app.
The existing implementation and its original icon are available for review.

The launcher's current policy permits installation from `storytold/` repositories
only. This proposal preserves that boundary: no catalog entry, download-host
exception, installer execution or installation path is introduced. A listing
must not point the current installer at the community ZIP.

If maintainers want to adopt the application, follow-up work would need their
explicit choice of repository ownership, approved distribution, package and
asset naming, release-checksum verification, update/uninstall behavior, and
platform support. The public identity must distinguish a community proposal
from an accepted official Craft App until that decision is made.
