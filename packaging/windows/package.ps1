<#
.SYNOPSIS
  Build, sign and package ArtCraft Launcher for Windows.

.DESCRIPTION
  Produces, in $env:DIST (default: dist/release):
    artcraft-launcher-<version>-windows-<arch>.msi            per-user installer with a setup wizard (WiX v5)
    artcraft-launcher-<version>-windows-<arch>-portable.zip   artcraft-launcher.exe + portable.txt

  The binary links the C runtime statically (+crt-static), so neither the MSI nor the portable
  zip needs the Visual C++ redistributable. Signing is delegated to sign.ps1 (skipped with a
  warning when no signing secrets are set).

  Needs: Rust (MSVC toolchain + the target), the Windows SDK (rc.exe, signtool.exe),
  and WiX v5: dotnet tool install --global wix --version 5.0.2
             wix extension add -g WixToolset.UI.wixext/5.0.2 WixToolset.Util.wixext/5.0.2

.EXAMPLE
  pwsh packaging/windows/package.ps1 -Arch x64
  pwsh packaging/windows/package.ps1 -Arch x86 -SkipBuild
  pwsh packaging/windows/package.ps1 -Arch arm64     # cross-compiled; needs the MSVC ARM64 build tools
#>
param(
  [ValidateSet('x64', 'x86', 'arm64')] [string] $Arch = 'x64',
  [switch] $SkipBuild
)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path

# ---- app-specific values ----------------------------------------------------------------------
$Pkg = 'artcraft-launcher'          # cargo package and binary
$AppName = 'ArtCraft Launcher'      # display name

function Invoke-Native([string] $What, [scriptblock] $Block) {
  Write-Output "==> $What"
  & $Block
  if ($LASTEXITCODE -ne 0) { throw "$What failed with exit code $LASTEXITCODE" }
}

# The version lives in one place: [workspace.package] version in the root Cargo.toml.
$Version = $env:ARTCRAFT_LAUNCHER_VERSION
if (-not $Version) {
  $inPkg = $false
  foreach ($line in Get-Content (Join-Path $Root 'Cargo.toml')) {
    if ($line -match '^\s*\[') { $inPkg = ($line.Trim() -eq '[workspace.package]'); continue }
    if ($inPkg -and $line -match '^\s*version\s*=\s*"([^"]+)"') { $Version = $Matches[1]; break }
  }
}
if (-not $Version) { throw 'could not read [workspace.package] version from Cargo.toml' }
# MSI ProductVersion is numeric (major.minor.build); pre-release tags are dropped there.
$MsiVersion = ($Version -split '-')[0]

$Target = switch ($Arch) { 'x64' { 'x86_64-pc-windows-msvc' } 'x86' { 'i686-pc-windows-msvc' } 'arm64' { 'aarch64-pc-windows-msvc' } }
$Dist = if ($env:DIST) { $env:DIST } else { Join-Path $Root 'dist\release' }
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root 'target' }
New-Item -ItemType Directory -Force -Path $Dist | Out-Null

if (-not $env:ARTCRAFT_LAUNCHER_BUILD_SHA) { $env:ARTCRAFT_LAUNCHER_BUILD_SHA = (git -C $Root rev-parse HEAD 2>$null) }
if (-not $env:ARTCRAFT_LAUNCHER_BUILD_DATE) { $env:ARTCRAFT_LAUNCHER_BUILD_DATE = (Get-Date).ToUniversalTime().ToString('yyyy-MM-dd') }

Write-Output "$AppName $Version for Windows $Arch ($Target)"

if (-not $SkipBuild) {
  # Static CRT: no VC++ redistributable needed. Scoped to the target so host build scripts and
  # proc-macros are unaffected.
  $flagVar = 'CARGO_TARGET_' + ($Target.ToUpper() -replace '-', '_') + '_RUSTFLAGS'
  [Environment]::SetEnvironmentVariable($flagVar, '-C target-feature=+crt-static')
  # Fail the build (rather than warn) if the icon/VERSIONINFO can't be embedded.
  $env:ARTCRAFT_LAUNCHER_REQUIRE_WINRES = '1'
  Invoke-Native "cargo build ($Target)" { cargo build --release --locked -p $Pkg --target $Target }
}

$Bin = Join-Path $TargetDir "$Target\release"
$Exe = "$Pkg.exe"

# Check the PE header before packaging. Machine (COFF header) must match -Arch, so an x64 build
# can never ship labelled arm64. Subsystem (optional header) must be 2 = Windows GUI, so no
# console window opens with the launcher.
function Get-PeHeader([string] $Path) {
  $bytes = [System.IO.File]::ReadAllBytes($Path)
  $pe = [BitConverter]::ToInt32($bytes, 0x3C)
  return @{ Machine = [BitConverter]::ToUInt16($bytes, $pe + 4); Subsystem = [BitConverter]::ToUInt16($bytes, $pe + 0x5C) }
}
$Machine = switch ($Arch) { 'x64' { 0x8664 } 'x86' { 0x14C } 'arm64' { 0xAA64 } }
$h = Get-PeHeader (Join-Path $Bin $Exe)
if ($h.Machine -ne $Machine) { throw "$Exe is for machine 0x$('{0:X}' -f $h.Machine), expected 0x$('{0:X}' -f $Machine) ($Arch)" }
if ($h.Subsystem -ne 2) { throw "$Exe has PE subsystem $($h.Subsystem), expected 2 (Windows GUI)" }
Write-Output "ok ${Exe}: $Arch, PE subsystem $($h.Subsystem)"

$Stage = Join-Path $TargetDir "windows-package\$Arch"
Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
Copy-Item (Join-Path $Bin $Exe) $Stage

& (Join-Path $PSScriptRoot 'sign.ps1') (Join-Path $Stage $Exe)

# ---- MSI ---------------------------------------------------------------------------------------
$Msi = Join-Path $Dist "$Pkg-$Version-windows-$Arch.msi"
& (Join-Path $PSScriptRoot 'check-icons.ps1')
Invoke-Native 'wix build' {
  wix build (Join-Path $PSScriptRoot "$Pkg.wxs") (Join-Path $PSScriptRoot 'installer-ui.wxs') -arch $Arch `
    -ext WixToolset.UI.wixext -ext WixToolset.Util.wixext `
    -culture en-US -loc (Join-Path $PSScriptRoot "$Pkg.en-us.wxl") `
    -d "Version=$MsiVersion" -d "DisplayVersion=$Version" -d "BinDir=$Stage" -d "IconPath=$(Join-Path $Root "assets\app-icon\$Pkg.ico")" `
    -o $Msi
}
Invoke-Native 'MSI shortcut icon validation (ICE50)' {
  wix msi validate $Msi -ice ICE50 -intermediateFolder (Join-Path $Stage 'msi-validation')
}
# wix writes its debug symbols (.wixpdb) next to the MSI; keep them out of the release assets.
Remove-Item -Force -ErrorAction SilentlyContinue ([IO.Path]::ChangeExtension($Msi, '.wixpdb'))
& (Join-Path $PSScriptRoot 'sign.ps1') $Msi

# ---- portable zip ------------------------------------------------------------------------------
$Portable = Join-Path $TargetDir "windows-package\$Pkg-$Version-windows-$Arch-portable"
Remove-Item -Recurse -Force $Portable -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Portable | Out-Null
Copy-Item (Join-Path $Stage $Exe) $Portable
foreach ($f in 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'NOTICE', 'ATTRIBUTION.md') {
  $p = Join-Path $Root $f
  if (Test-Path $p) { Copy-Item $p $Portable }
}
# portable.txt beside artcraft-launcher.exe switches on portable mode: settings and the download
# cache go to ArtCraftLauncherData\ next to the exe instead of %APPDATA%.
Copy-Item (Join-Path $PSScriptRoot 'portable.txt') $Portable
$Zip = Join-Path $Dist "$Pkg-$Version-windows-$Arch-portable.zip"
Remove-Item -Force $Zip -ErrorAction SilentlyContinue
Compress-Archive -Path $Portable -DestinationPath $Zip

Get-Item $Msi, $Zip | Format-Table Name, Length
