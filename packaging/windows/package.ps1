<#
.SYNOPSIS
  Build, sign and package ArtCraft Launcher for Windows.

.DESCRIPTION
  Produces, in $env:DIST (default: dist/release):
    artcraft-launcher-<version>-windows-<arch>.msi            per-user installer with a setup wizard (WiX v5)
    artcraft-launcher-<version>-windows-<arch>-portable.zip   artcraft-launcher.exe + portable.txt
    artcraft-launcher-updater-<version>-windows-<arch>.msi    per-machine ArtCraft Launcher Update Service
                                                              (Windows service + command line), opt-in

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
$Updater = 'artcraft-launcher-updater'  # the update service and its command line: package and binary

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
  Invoke-Native "cargo build ($Target)" { cargo build --release --locked -p $Pkg -p $Updater --target $Target }
}

$Bin = Join-Path $TargetDir "$Target\release"
$Exe = "$Pkg.exe"
$UpdaterExe = "$Updater.exe"

# Check the PE headers before packaging. Machine (COFF header) must match -Arch, so an x64 build
# can never ship labelled arm64. Subsystem (optional header) must be 2 = Windows GUI for the
# launcher, so no console window opens with it, and 3 = console for the updater, whose command
# line prints to the terminal it's run from (a service has no window either way).
function Get-PeHeader([string] $Path) {
  $bytes = [System.IO.File]::ReadAllBytes($Path)
  $pe = [BitConverter]::ToInt32($bytes, 0x3C)
  return @{ Machine = [BitConverter]::ToUInt16($bytes, $pe + 4); Subsystem = [BitConverter]::ToUInt16($bytes, $pe + 0x5C) }
}
$Machine = switch ($Arch) { 'x64' { 0x8664 } 'x86' { 0x14C } 'arm64' { 0xAA64 } }
foreach ($check in @(@($Exe, 2, 'Windows GUI'), @($UpdaterExe, 3, 'console'))) {
  $h = Get-PeHeader (Join-Path $Bin $check[0])
  if ($h.Machine -ne $Machine) { throw "$($check[0]) is for machine 0x$('{0:X}' -f $h.Machine), expected 0x$('{0:X}' -f $Machine) ($Arch)" }
  if ($h.Subsystem -ne $check[1]) { throw "$($check[0]) has PE subsystem $($h.Subsystem), expected $($check[1]) ($($check[2]))" }
  Write-Output "ok $($check[0]): $Arch, PE subsystem $($h.Subsystem)"
}

$Stage = Join-Path $TargetDir "windows-package\$Arch"
Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
Copy-Item (Join-Path $Bin $Exe) $Stage
Copy-Item (Join-Path $Bin $UpdaterExe) $Stage

& (Join-Path $PSScriptRoot 'sign.ps1') (Join-Path $Stage $Exe) (Join-Path $Stage $UpdaterExe)

# ---- MSI ---------------------------------------------------------------------------------------
# The setup wizard's art, drawn here (System.Drawing) in getartcraft.com's style so the MSI shows
# ArtCraft rather than WiX's stock bitmaps. Both must be 24-bit BMPs:
#   - dialog, 493x312: the Welcome and Finish pages' backdrop. Their text sits on the right, so
#     that stays white; the left panel is ours: the blue ArtCraft mark, the white wordmark, a mono
#     "Launcher" label and the Craft apps' icons in a hairline grid.
#   - banner, 493x58: across the top of the inner pages. The page title is drawn over its left
#     side, so that stays white; the right end is a dark cell with the mark.
# The ArtCraft mark and wordmark are drawn as supplied (docs/brand/LICENSE-brand.txt).
function New-InstallerArt([string] $BannerOut, [string] $DialogOut) {
  Add-Type -AssemblyName System.Drawing
  $ink = [System.Drawing.ColorTranslator]::FromHtml('#121316')
  $accent = [System.Drawing.ColorTranslator]::FromHtml('#2d81ff')
  $muted = [System.Drawing.Color]::FromArgb(153, 242, 241, 238)
  $line = [System.Drawing.Color]::FromArgb(38, 242, 241, 238)
  # The Craft App Suite, in the launcher's order (crates/core/src/catalog.rs).
  $apps = 'photocraft', 'vectorcraft', 'filmcraft', 'lightcraft', 'pdfcraft', 'effectcraft',
    'designcraft', 'figcraft', 'wordcraft', 'soundcraft', 'deckcraft', 'cadcraft', 'gridcraft'
  $mark = [System.Drawing.Image]::FromFile((Join-Path $Root 'assets\app-icon\hicolor\256x256\apps\ai.storyteller.artcraft-launcher.png'))
  $wordmark = [System.Drawing.Image]::FromFile((Join-Path $Root 'docs\brand\artcraft-logo-white.png'))
  $icons = $apps | ForEach-Object { [System.Drawing.Image]::FromFile((Join-Path $Root "assets\app-icons\$_.png")) }
  # The site's label face; GDI+ draws a variable font at its default instance (Regular).
  $fonts = New-Object System.Drawing.Text.PrivateFontCollection
  $fonts.AddFontFile((Join-Path $Root 'assets\fonts\GeistMono-Variable.ttf'))
  $labelFont = New-Object System.Drawing.Font $fonts.Families[0], 9, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
  try {
    foreach ($spec in @(@($BannerOut, 493, 58), @($DialogOut, 493, 312))) {
      $bmp = New-Object System.Drawing.Bitmap $spec[1], $spec[2], ([System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
      $g = [System.Drawing.Graphics]::FromImage($bmp)
      $panel = New-Object System.Drawing.SolidBrush $ink
      $stripe = New-Object System.Drawing.SolidBrush $accent
      try {
        $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
        $g.Clear([System.Drawing.Color]::White)
        if ($spec[2] -eq 58) {
          $g.FillRectangle($panel, 405, 0, 88, 58)
          $g.FillRectangle($stripe, 405, 0, 2, 58)
          $g.DrawImage($mark, 431, 11, 36, 36)
        } else {
          $g.FillRectangle($panel, 0, 0, 164, 312)
          $g.FillRectangle($stripe, 162, 0, 2, 312)
          $g.DrawImage($mark, 16, 18, 44, 44)
          $g.DrawImage($wordmark, 22, 74, 120, [int](120 * $wordmark.Height / $wordmark.Width))
          # "LAUNCHER" in mono capitals, spaced like the site's labels (0.15 em).
          $brush = New-Object System.Drawing.SolidBrush $muted
          $x = 22.0
          foreach ($ch in 'LAUNCHER'.ToCharArray()) {
            $g.DrawString([string]$ch, $labelFont, $brush, $x, 94, [System.Drawing.StringFormat]::GenericTypographic)
            $x += $g.MeasureString([string]$ch, $labelFont, 100, [System.Drawing.StringFormat]::GenericTypographic).Width + 1.35
          }
          $brush.Dispose()
          # The apps, three to a row, in 36 px cells divided by hairlines (five rows fit above 312).
          $pen = New-Object System.Drawing.Pen $line, 1
          $left = 22; $top = 118; $cell = 36
          for ($i = 0; $i -lt $icons.Count; $i++) {
            $cx = $left + ($i % 3) * $cell
            $cy = $top + [math]::Floor($i / 3) * $cell
            $g.DrawRectangle($pen, $cx, $cy, $cell, $cell)
            $g.DrawImage($icons[$i], $cx + 5, $cy + 5, $cell - 10, $cell - 10)
          }
          $pen.Dispose()
        }
      } finally { $panel.Dispose(); $stripe.Dispose(); $g.Dispose() }
      $bmp.Save($spec[0], [System.Drawing.Imaging.ImageFormat]::Bmp)
      $bmp.Dispose()
    }
  } finally {
    $labelFont.Dispose(); $fonts.Dispose(); $mark.Dispose(); $wordmark.Dispose()
    $icons | ForEach-Object { $_.Dispose() }
  }
}
$Banner = Join-Path $Stage 'installer-banner.bmp'
$Dialog = Join-Path $Stage 'installer-dialog.bmp'
New-InstallerArt $Banner $Dialog

$Msi = Join-Path $Dist "$Pkg-$Version-windows-$Arch.msi"
& (Join-Path $PSScriptRoot 'check-icons.ps1')
Invoke-Native 'wix build' {
  wix build (Join-Path $PSScriptRoot "$Pkg.wxs") -arch $Arch `
    -ext WixToolset.UI.wixext -ext WixToolset.Util.wixext `
    -culture en-US -loc (Join-Path $PSScriptRoot "$Pkg.en-us.wxl") `
    -d "Version=$MsiVersion" -d "DisplayVersion=$Version" -d "BinDir=$Stage" -d "IconPath=$(Join-Path $Root "assets\app-icon\$Pkg.ico")" `
    -d "BannerBmp=$Banner" -d "DialogBmp=$Dialog" `
    -o $Msi
}
Invoke-Native 'MSI shortcut icon validation (ICE50)' {
  wix msi validate $Msi -ice ICE50 -intermediateFolder (Join-Path $Stage 'msi-validation')
}
# wix writes its debug symbols (.wixpdb) next to the MSI; keep them out of the release assets.
Remove-Item -Force -ErrorAction SilentlyContinue ([IO.Path]::ChangeExtension($Msi, '.wixpdb'))
& (Join-Path $PSScriptRoot 'sign.ps1') $Msi

# ---- the update service's MSI ------------------------------------------------------------------
# Per-machine, no wizard: the ArtCraft Launcher Update Service (a LocalSystem Windows service) and
# its command line, which an administrator installs once so that users' updates need no UAC
# prompt. Its own update checks this file's Authenticode signature, product name and version
# (crates/native/src/msi.rs), so it is signed like everything else.
$UpdaterMsi = Join-Path $Dist "$Updater-$Version-windows-$Arch.msi"
Invoke-Native 'wix build (update service)' {
  wix build (Join-Path $PSScriptRoot "$Updater.wxs") -arch $Arch -ext WixToolset.Util.wixext `
    -d "Version=$MsiVersion" -d "DisplayVersion=$Version" -d "BinDir=$Stage" -d "IconPath=$(Join-Path $Root "assets\app-icon\$Pkg.ico")" `
    -o $UpdaterMsi
}
Invoke-Native 'MSI validation (update service)' {
  wix msi validate $UpdaterMsi -intermediateFolder (Join-Path $Stage 'updater-msi-validation')
}
Remove-Item -Force -ErrorAction SilentlyContinue ([IO.Path]::ChangeExtension($UpdaterMsi, '.wixpdb'))
& (Join-Path $PSScriptRoot 'sign.ps1') $UpdaterMsi

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

Get-Item $Msi, $Zip, $UpdaterMsi | Format-Table Name, Length
