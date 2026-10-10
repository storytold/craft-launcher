<#
.SYNOPSIS
  Build ArtCraft Launcher for Windows under a name you choose.

.DESCRIPTION
  Produces, in $env:DIST (default: dist/renamed):
    <file-name>-<version>-windows-<arch>.msi            per-user installer with the setup wizard
    <file-name>-<version>-windows-<arch>-portable.zip   <file-name>.exe + portable.txt

  <file-name> is the new name in lower case without spaces or symbols ("My Launcher" ->
  mylauncher), and is also the exe's name.

  The repository is never changed. xtask/src/bin/rename-launcher.rs copies it to
  target/renamed/src and, in that copy, replaces "ArtCraft Launcher" with the new name in the
  app's text (window title, tray menu, Settings, notifications, messages, the exe's version info,
  every UI language) and in the installer (product name, install folder, Start menu and desktop
  shortcuts, wizard pages). The copy's own package.ps1 then builds it, so the installers are made
  exactly as the release's are. Build output is cached in target/renamed/target, so later runs
  only recompile what changed.

  Kept as they are, so the renamed launcher still works with what the official one installed:
  the app id (ai.storyteller.artcraft-launcher, used for notifications and the taskbar), the
  registry key that records the install folder, and the MSI's upgrade code (installing one
  replaces the other).

  A renamed build doesn't update itself: the official release would replace it and bring the
  original name back. Run this script again on a newer checkout to update it.

  The icon, the setup wizard's art and the header logo are the ArtCraft marks, which are not open
  source (docs/brand/LICENSE-brand.txt). They stay in a renamed build for your own use; if you
  share or publish one, replace them with your own first.

  Needs the same tools as package.ps1: Rust (MSVC toolchain + the target), the Windows SDK and
  WiX v5 with its UI and Util extensions.

.PARAMETER Name
  The name users see. Letters, digits, spaces and simple punctuation, up to 60 characters; not
  allowed: " \ / : * ? < > | { } [ ] %

.PARAMETER Arch
  x64 (default), x86 or arm64.

.EXAMPLE
  pwsh packaging/windows/build-renamed.ps1 -Name 'My Launcher'
  pwsh packaging/windows/build-renamed.ps1 -Name 'Studio Hub' -Arch arm64
#>
param(
  [Parameter(Mandatory)] [string] $Name,
  [ValidateSet('x64', 'x86', 'arm64')] [string] $Arch = 'x64'
)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Renamed = Join-Path $Root 'target\renamed'
$Stage = Join-Path $Renamed 'src'

$saved = @{ CARGO_TARGET_DIR = $env:CARGO_TARGET_DIR; DIST = $env:DIST }
Push-Location $Root
try {
  Write-Output "==> staging a copy named '$Name' in $Stage"
  cargo run --quiet --locked -p xtask --bin rename-launcher -- $Stage $Name
  if ($LASTEXITCODE -ne 0) { throw "rename-launcher failed with exit code $LASTEXITCODE" }

  # A target dir of its own, so the renamed build and the normal one don't keep rebuilding each other.
  $env:CARGO_TARGET_DIR = Join-Path $Renamed 'target'
  if (-not $env:DIST) { $env:DIST = Join-Path $Root 'dist\renamed' }
  Set-Location $Stage
  & (Join-Path $Stage 'packaging\windows\package.ps1') -Arch $Arch
} finally {
  Pop-Location
  $env:CARGO_TARGET_DIR = $saved.CARGO_TARGET_DIR
  $env:DIST = $saved.DIST
}
