#Requires -Version 7
<#
.SYNOPSIS
  Build ArtCraft Launcher for Windows under a name you choose.

.DESCRIPTION
  Run it without parameters (or double-click build-renamed.cmd) to open the builder window: click
  the app name to change it, tick the architectures and click Build. The build log streams into
  the window and is also saved in target\renamed\logs\. -Name builds without the window.

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

  Needs the same tools as package.ps1: Rust (MSVC toolchain + the target) and the Windows SDK.
  When `wix` isn't WiX v5, WiX 5.0.2 is installed into target\renamed\wix (needs the .NET SDK),
  and its UI and Util extensions are added when they are missing.

.PARAMETER Name
  The name users see. Letters, digits, spaces and simple punctuation, up to 60 characters; not
  allowed: " \ / : * ? < > | { } [ ] %

.PARAMETER Arch
  x64 (default), x86 and/or arm64, e.g. -Arch x64,arm64.

.EXAMPLE
  pwsh packaging/windows/build-renamed.ps1                          # opens the builder window
  pwsh packaging/windows/build-renamed.ps1 -Name 'My Launcher'
  pwsh packaging/windows/build-renamed.ps1 -Name 'Studio Hub' -Arch x64,arm64
#>
param(
  [string] $Name,
  [string[]] $Arch = @('x64'),
  # Used by the builder window to follow progress.
  [switch] $StatusMarkers
)
$ErrorActionPreference = 'Stop'
$ScriptPath = $PSCommandPath
$Original = 'ArtCraft Launcher'
$Pkg = 'artcraft-launcher'
$Arches = 'x64', 'x86', 'arm64'
$WixVersion = '5.0.2'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Renamed = Join-Path $Root 'target\renamed'
$Stage = Join-Path $Renamed 'src'
$Dist = if ($env:DIST) { $env:DIST } else { Join-Path $Root 'dist\renamed' }

# The same rules as xtask/src/bin/rename-launcher.rs, which checks the name again when it stages
# the copy; repeated here so the window can flag a bad name while it's being typed.
function Test-AppName([string] $Value) {
  if ([string]::IsNullOrWhiteSpace($Value)) { return 'The name is empty.' }
  if ($Value -ne $Value.Trim()) { return 'The name starts or ends with a space.' }
  if (@($Value.EnumerateRunes()).Count -gt 60) { return 'The name is longer than 60 characters.' }
  if ($Value -match '["\\/:*?<>|{}\[\]%\p{Cc}]') { return 'The name contains a character that is not allowed: " \ / : * ? < > | { } [ ] %' }
  if ((Get-ExeName $Value) -eq 'xtask') { return "That name would give the exe the same name as the workspace's xtask tool." }
  ''
}

function Get-ExeName([string] $Value) {
  if ($Value -ceq $Original) { return $Pkg }
  $m = ($Value -creplace '[^A-Za-z0-9]', '').ToLowerInvariant()
  if ($m) { $m } else { $Pkg }
}

function Get-WorkspaceVersion {
  $inPkg = $false
  foreach ($line in Get-Content (Join-Path $Root 'Cargo.toml')) {
    if ($line -match '^\s*\[') { $inPkg = ($line.Trim() -eq '[workspace.package]'); continue }
    if ($inPkg -and $line -match '^\s*version\s*=\s*"([^"]+)"') { return $Matches[1] }
  }
  throw 'could not read [workspace.package] version from Cargo.toml'
}

function Invoke-Native([string] $What, [scriptblock] $Block) {
  & $Block
  if ($LASTEXITCODE -ne 0) { throw "$What failed with exit code $LASTEXITCODE" }
}

# package.ps1 needs WiX v5 with the UI and Util extensions. A different (or no) global `wix` gets
# WiX 5 installed beside the renamed build instead, first on PATH for this run only.
function Use-Wix5 {
  $wix = Get-Command wix -ErrorAction SilentlyContinue
  $found = if ($wix) { "$(& wix --version)".Trim() } else { '' }
  if ($found -notlike '5.*') {
    $tools = Join-Path $Renamed 'wix'
    if (-not (Test-Path (Join-Path $tools 'wix.exe'))) {
      if (-not (Get-Command dotnet -ErrorAction SilentlyContinue)) {
        throw 'The packaging needs WiX v5, and installing it needs the .NET SDK: https://dotnet.microsoft.com/download'
      }
      Write-Output "==> installing WiX $WixVersion in $tools ($(if ($found) { "found WiX $found" } else { 'no WiX found' }); the packaging needs v5)"
      Invoke-Native "dotnet tool install wix $WixVersion" { dotnet tool install wix --version $WixVersion --tool-path $tools }
    }
    $env:PATH = "$tools;$env:PATH"
  }
  $have = @(& wix extension list -g)
  foreach ($ext in 'WixToolset.UI.wixext', 'WixToolset.Util.wixext') {
    if (-not ($have -match "^$([regex]::Escape($ext)) $([regex]::Escape($WixVersion))$")) {
      Invoke-Native "wix extension add $ext" { wix extension add -g "$ext/$WixVersion" }
    }
  }
}

function Write-Status([string] $State, [string] $Value) {
  if ($StatusMarkers) { Write-Host "@@STATUS $State $Value" }
}

function Invoke-RenamedBuild([string] $NewName, [string[]] $Targets) {
  $saved = @{ CARGO_TARGET_DIR = $env:CARGO_TARGET_DIR; DIST = $env:DIST; PATH = $env:PATH }
  Push-Location $Root
  try {
    Use-Wix5
    Write-Output "==> staging a copy named '$NewName' in $Stage"
    Invoke-Native 'rename-launcher' { cargo run --quiet --locked -p xtask --bin rename-launcher -- $Stage $NewName }

    # A target dir of its own, so the renamed build and the normal one don't keep rebuilding each other.
    $env:CARGO_TARGET_DIR = Join-Path $Renamed 'target'
    $env:DIST = $Dist
    Set-Location $Stage
    foreach ($a in $Targets) {
      Write-Status building $a
      & (Join-Path $Stage 'packaging\windows\package.ps1') -Arch $a
      Write-Status done $a
    }
  } finally {
    Pop-Location
    $env:CARGO_TARGET_DIR = $saved.CARGO_TARGET_DIR
    $env:DIST = $saved.DIST
    $env:PATH = $saved.PATH
  }
}

# ---------------------------------------------------------------------------------------------
# Builder window
# ---------------------------------------------------------------------------------------------
function Show-BuilderGui {
  Add-Type -AssemblyName System.Windows.Forms, System.Drawing
  if (-not ('RenamedBuildRun' -as [type])) {
    # Runs the build as a hidden child process (this script with -Name) and queues its output
    # lines for the window, which reads them on a timer.
    Add-Type -TypeDefinition @'
using System.Collections.Concurrent;
using System.Diagnostics;
using System.Text;
using System.Threading;

public sealed class RenamedBuildRun
{
    public readonly ConcurrentQueue<string> Lines = new ConcurrentQueue<string>();
    Process proc;
    int openStreams = 2;

    public void Start(string file, string[] args, string workDir)
    {
        var psi = new ProcessStartInfo(file)
        {
            UseShellExecute = false, CreateNoWindow = true, WorkingDirectory = workDir,
            RedirectStandardOutput = true, RedirectStandardError = true,
            StandardOutputEncoding = Encoding.UTF8, StandardErrorEncoding = Encoding.UTF8,
        };
        foreach (var a in args) psi.ArgumentList.Add(a);
        proc = new Process { StartInfo = psi };
        proc.OutputDataReceived += (s, e) => { if (e.Data == null) Interlocked.Decrement(ref openStreams); else Lines.Enqueue(e.Data); };
        proc.ErrorDataReceived += (s, e) => { if (e.Data == null) Interlocked.Decrement(ref openStreams); else Lines.Enqueue(e.Data); };
        proc.Start();
        proc.BeginOutputReadLine();
        proc.BeginErrorReadLine();
    }

    public bool Done { get { return proc == null || (Volatile.Read(ref openStreams) <= 0 && proc.HasExited); } }
    public int ExitCode { get { return proc == null ? -1 : proc.ExitCode; } }

    // Stops the build and everything it started (cargo, rustc, the linker, wix).
    public void Kill()
    {
        try { if (proc != null && !proc.HasExited) proc.Kill(true); } catch (System.InvalidOperationException) { }
    }
}
'@
  }
  [System.Windows.Forms.Application]::EnableVisualStyles()
  try { [void][System.Windows.Forms.Application]::SetHighDpiMode([System.Windows.Forms.HighDpiMode]::PerMonitorV2) } catch [InvalidOperationException] { }

  $version = Get-WorkspaceVersion
  $ui = @{ Building = $false; Cancelled = $false; HasError = $false; Run = $null; LogWriter = $null; Started = $null; Total = 0; Finished = 0 }
  $font = [System.Drawing.Font]::new('Segoe UI', 9.75)
  $green = [System.Drawing.Color]::FromArgb(0, 128, 0)
  $red = [System.Drawing.Color]::FromArgb(196, 0, 0)
  $blue = [System.Drawing.Color]::FromArgb(0, 90, 180)

  $form = [System.Windows.Forms.Form]@{ Text = 'ArtCraft Launcher builder'; Font = $font; StartPosition = 'CenterScreen'; AutoScaleMode = 'None' }
  # Fonts follow the display scaling on their own; pixel sizes are scaled here.
  $scale = $form.DeviceDpi / 96.0
  $px = { param([double] $v) [int][math]::Round($v * $scale) }
  $form.ClientSize = [System.Drawing.Size]::new((& $px 960), (& $px 640))
  $form.MinimumSize = [System.Drawing.Size]::new((& $px 760), (& $px 480))

  $layout = [System.Windows.Forms.TableLayoutPanel]@{ Dock = 'Fill'; ColumnCount = 1; RowCount = 7; Padding = [System.Windows.Forms.Padding]::new((& $px 12), (& $px 10), (& $px 12), (& $px 6)) }
  $rowHeight = & $px 30
  $headerHeight = & $px 28
  [void]$layout.RowStyles.Add([System.Windows.Forms.RowStyle]::new('AutoSize'))
  [void]$layout.RowStyles.Add([System.Windows.Forms.RowStyle]::new('Absolute', $headerHeight + $rowHeight + 4))
  [void]$layout.RowStyles.Add([System.Windows.Forms.RowStyle]::new('AutoSize'))
  [void]$layout.RowStyles.Add([System.Windows.Forms.RowStyle]::new('AutoSize'))
  [void]$layout.RowStyles.Add([System.Windows.Forms.RowStyle]::new('AutoSize'))
  [void]$layout.RowStyles.Add([System.Windows.Forms.RowStyle]::new('Percent', 100))
  [void]$layout.RowStyles.Add([System.Windows.Forms.RowStyle]::new('AutoSize'))

  $intro = [System.Windows.Forms.Label]@{
    AutoSize = $true; Margin = [System.Windows.Forms.Padding]::new(0, 0, 0, (& $px 8))
    Text = "Click the app name to change it, tick the architectures and click Build.`n" +
           "The name is used in the app, its installer, the Start menu, its install folder and the exe file name.`n" +
           "Installers are saved in: $Dist"
  }
  $layout.Controls.Add($intro, 0, 0)

  # ---- the app: one row, the name cell is the one to click
  $grid = [System.Windows.Forms.DataGridView]@{
    Dock = 'Fill'; AutoGenerateColumns = $false; AllowUserToAddRows = $false; AllowUserToDeleteRows = $false
    AllowUserToResizeRows = $false; RowHeadersVisible = $false; SelectionMode = 'CellSelect'; MultiSelect = $false
    EditMode = 'EditOnEnter'; AutoSizeColumnsMode = 'Fill'; BackgroundColor = [System.Drawing.SystemColors]::Window
    BorderStyle = 'FixedSingle'; ShowCellToolTips = $true; ScrollBars = 'None'; AccessibleName = 'App to build'
    ColumnHeadersHeightSizeMode = 'DisableResizing'; ColumnHeadersHeight = $headerHeight
  }
  $grid.RowTemplate.Height = $rowHeight
  $grid.DefaultCellStyle.SelectionBackColor = [System.Drawing.Color]::FromArgb(204, 228, 247)
  $grid.DefaultCellStyle.SelectionForeColor = [System.Drawing.SystemColors]::ControlText
  $grid.DefaultCellStyle.Padding = [System.Windows.Forms.Padding]::new((& $px 4), 0, (& $px 4), 0)
  $colProject = [System.Windows.Forms.DataGridViewTextBoxColumn]@{ Name = 'Project'; HeaderText = 'Project'; FillWeight = 18; ReadOnly = $true }
  $colName = [System.Windows.Forms.DataGridViewTextBoxColumn]@{ Name = 'AppName'; HeaderText = 'App name (click to rename)'; FillWeight = 34; MaxInputLength = 60; ToolTipText = 'Click the name to change it.' }
  $colExe = [System.Windows.Forms.DataGridViewTextBoxColumn]@{ Name = 'Exe'; HeaderText = 'Exe file'; FillWeight = 20; ReadOnly = $true }
  $colVersion = [System.Windows.Forms.DataGridViewTextBoxColumn]@{ Name = 'Version'; HeaderText = 'Version'; FillWeight = 10; ReadOnly = $true }
  $colStatus = [System.Windows.Forms.DataGridViewTextBoxColumn]@{ Name = 'Status'; HeaderText = 'Status'; FillWeight = 18; ReadOnly = $true }
  $grid.Columns.AddRange([System.Windows.Forms.DataGridViewColumn[]]@($colProject, $colName, $colExe, $colVersion, $colStatus))
  foreach ($c in $colProject, $colExe, $colVersion, $colStatus) {
    $c.SortMode = 'NotSortable'
    $c.DefaultCellStyle.ForeColor = [System.Drawing.SystemColors]::GrayText
    $c.DefaultCellStyle.BackColor = [System.Drawing.SystemColors]::Control
  }
  $colName.SortMode = 'NotSortable'
  $colName.DefaultCellStyle.Font = [System.Drawing.Font]::new($font, [System.Drawing.FontStyle]::Bold)
  $colName.DefaultCellStyle.BackColor = [System.Drawing.SystemColors]::Window
  [void]$grid.Rows.Add($Pkg, $Original, "$Pkg.exe", $version, '')
  $row = $grid.Rows[0]
  $nameCell = $row.Cells['AppName']
  $nameCell.ToolTipText = "Click to change the name. Original name: $Original"
  $layout.Controls.Add($grid, 0, 1)

  # ---- architectures
  $archRow = [System.Windows.Forms.FlowLayoutPanel]@{ AutoSize = $true; WrapContents = $false; Margin = [System.Windows.Forms.Padding]::new(0, (& $px 8), 0, 0) }
  $archLabel = [System.Windows.Forms.Label]@{ Text = 'Build for:'; AutoSize = $true; Margin = [System.Windows.Forms.Padding]::new(0, (& $px 5), (& $px 8), 0) }
  $archRow.Controls.Add($archLabel)
  $archBoxes = [ordered]@{}
  foreach ($a in $Arches) {
    $box = [System.Windows.Forms.CheckBox]@{ Text = $a; AutoSize = $true; Checked = ($a -eq 'x64'); Margin = [System.Windows.Forms.Padding]::new(0, (& $px 3), (& $px 14), 0) }
    $archBoxes[$a] = $box
    $archRow.Controls.Add($box)
  }
  $tips = [System.Windows.Forms.ToolTip]::new()
  $tips.SetToolTip($archBoxes['arm64'], 'Cross-compiled: needs the MSVC ARM64 build tools.')
  $layout.Controls.Add($archRow, 0, 2)

  # ---- buttons
  $buttons = [System.Windows.Forms.TableLayoutPanel]@{ Dock = 'Fill'; AutoSize = $true; ColumnCount = 2; RowCount = 1; Margin = [System.Windows.Forms.Padding]::new(0, (& $px 8), 0, (& $px 4)) }
  [void]$buttons.ColumnStyles.Add([System.Windows.Forms.ColumnStyle]::new('Percent', 100))
  [void]$buttons.ColumnStyles.Add([System.Windows.Forms.ColumnStyle]::new('AutoSize'))
  $left = [System.Windows.Forms.FlowLayoutPanel]@{ AutoSize = $true; WrapContents = $false; Margin = [System.Windows.Forms.Padding]::new(0) }
  $right = [System.Windows.Forms.FlowLayoutPanel]@{ AutoSize = $true; WrapContents = $false; Margin = [System.Windows.Forms.Padding]::new(0); Anchor = 'Right' }
  $newButton = {
    param([string] $Text)
    [System.Windows.Forms.Button]@{ Text = $Text; AutoSize = $true; MinimumSize = [System.Drawing.Size]::new((& $px 96), (& $px 32)); Padding = [System.Windows.Forms.Padding]::new((& $px 8), 0, (& $px 8), 0) }
  }
  $btnReset = & $newButton '&Reset name'
  $btnOpen = & $newButton '&Open installers folder'
  $btnCancel = & $newButton '&Cancel build'
  $btnBuild = & $newButton '&Build'
  $btnBuild.Font = [System.Drawing.Font]::new($font, [System.Drawing.FontStyle]::Bold)
  $btnCancel.Enabled = $false
  $left.Controls.Add($btnReset)
  $right.Controls.AddRange([System.Windows.Forms.Control[]]@($btnOpen, $btnCancel, $btnBuild))
  $buttons.Controls.Add($left, 0, 0)
  $buttons.Controls.Add($right, 1, 0)
  $layout.Controls.Add($buttons, 0, 3)

  # ---- log
  $logLabel = [System.Windows.Forms.Label]@{ Text = 'Build log'; AutoSize = $true; Margin = [System.Windows.Forms.Padding]::new(0, (& $px 6), 0, (& $px 2)) }
  $layout.Controls.Add($logLabel, 0, 4)
  $log = [System.Windows.Forms.TextBox]@{
    Dock = 'Fill'; Multiline = $true; ReadOnly = $true; ScrollBars = 'Both'; WordWrap = $false; MaxLength = 0
    Font = [System.Drawing.Font]::new('Consolas', 9.5); BackColor = [System.Drawing.Color]::FromArgb(30, 30, 30)
    ForeColor = [System.Drawing.Color]::FromArgb(220, 220, 220); AccessibleName = 'Build log'
  }
  $layout.Controls.Add($log, 0, 5)

  # ---- status line
  $statusRow = [System.Windows.Forms.TableLayoutPanel]@{ Dock = 'Fill'; AutoSize = $true; ColumnCount = 2; RowCount = 1; Margin = [System.Windows.Forms.Padding]::new(0, (& $px 6), 0, 0) }
  [void]$statusRow.ColumnStyles.Add([System.Windows.Forms.ColumnStyle]::new('Percent', 100))
  [void]$statusRow.ColumnStyles.Add([System.Windows.Forms.ColumnStyle]::new('AutoSize'))
  $statusText = [System.Windows.Forms.Label]@{ AutoSize = $true; Anchor = 'Left' }
  $progress = [System.Windows.Forms.ProgressBar]@{ Style = 'Marquee'; Visible = $false; MarqueeAnimationSpeed = 30; Size = [System.Drawing.Size]::new((& $px 180), (& $px 16)); Anchor = 'Right' }
  $statusRow.Controls.Add($statusText, 0, 0)
  $statusRow.Controls.Add($progress, 1, 0)
  $layout.Controls.Add($statusRow, 0, 6)

  $form.Controls.Add($layout)

  # ---- behaviour
  $checkedArches = { @($archBoxes.Keys | Where-Object { $archBoxes[$_].Checked }) }
  $setStatusCell = {
    param([string] $Text, [System.Drawing.Color] $Color)
    $row.Cells['Status'].Value = $Text
    $row.Cells['Status'].Style.ForeColor = $Color
  }

  $updateButtons = {
    $n = (& $checkedArches).Count
    $btnBuild.Enabled = -not $ui.Building -and $n -gt 0 -and -not $ui.HasError
    $btnCancel.Enabled = $ui.Building
    $btnReset.Enabled = -not $ui.Building
    foreach ($box in $archBoxes.Values) { $box.Enabled = -not $ui.Building }
    if (-not $ui.Building) {
      $statusText.Text = if ($ui.HasError) { 'Fix the name first.' }
        elseif ($n -eq 0) { 'Tick at least one architecture.' }
        else { "Click Build to make the installers for $($nameCell.Value)." }
    }
  }

  $validate = {
    $value = [string]$nameCell.Value
    $err = Test-AppName $value
    $row.Cells['Exe'].Value = if ($err) { '' } else { "$(Get-ExeName $value).exe" }
    $nameCell.ErrorText = $err
    $ui.HasError = [bool]$err
    & $updateButtons
  }

  $setBuilding = {
    param([bool] $On)
    $ui.Building = $On
    $colName.ReadOnly = $On
    $progress.Visible = $On
    & $updateButtons
  }

  $finish = {
    $timer.Stop()
    $exitCode = $ui.Run.ExitCode
    if ($ui.LogWriter) { $ui.LogWriter.Dispose(); $ui.LogWriter = $null }
    $minutes = [math]::Round(((Get-Date) - $ui.Started).TotalMinutes, 1)
    & $setBuilding $false
    if ($ui.Cancelled) {
      & $setStatusCell 'Cancelled' ([System.Drawing.SystemColors]::GrayText)
      $statusText.Text = 'Build cancelled.'
    } elseif ($exitCode -eq 0) {
      & $setStatusCell "Done ($minutes min)" $green
      $statusText.Text = "Done. Installers are in $Dist"
    } else {
      & $setStatusCell 'Failed (see log)' $red
      $statusText.Text = 'The build failed. See the log.'
    }
  }

  $timer = [System.Windows.Forms.Timer]@{ Interval = 200 }
  $timer.Add_Tick({
    if (-not $ui.Run) { return }
    $sb = [System.Text.StringBuilder]::new()
    $line = $null; $count = 0
    while ($count -lt 5000 -and $ui.Run.Lines.TryDequeue([ref]$line)) {
      $count++
      if ($line -match '^@@STATUS (\S+) (\S+)$') {
        if ($Matches[1] -eq 'building') {
          & $setStatusCell "Building $($Matches[2])..." $blue
          $statusText.Text = "Building $($nameCell.Value) for $($Matches[2]) ($($ui.Finished + 1) of $($ui.Total))..."
        } else { $ui.Finished++ }
        continue
      }
      [void]$sb.AppendLine($line)
      if ($ui.LogWriter) { $ui.LogWriter.WriteLine($line) }
    }
    if ($sb.Length -gt 0) {
      if ($log.TextLength -gt 4000000) { $log.Text = $log.Text.Substring($log.TextLength - 2000000) }
      $log.AppendText($sb.ToString())
    }
    if ($ui.Run.Done -and $ui.Run.Lines.IsEmpty) { & $finish }
  })

  $grid.Add_CellEndEdit({
    $trimmed = ([string]$nameCell.Value).Trim()
    if ($nameCell.Value -cne $trimmed) { $nameCell.Value = $trimmed }
  })
  $grid.Add_CellValueChanged({
    param($s, $e)
    if ($e.RowIndex -eq 0 -and $e.ColumnIndex -eq $colName.Index) { & $validate }
  })
  foreach ($box in $archBoxes.Values) { $box.Add_CheckedChanged({ & $updateButtons }) }

  $btnReset.Add_Click({
    [void]$grid.EndEdit()
    $nameCell.Value = $Original
    & $validate
  })
  $btnOpen.Add_Click({
    New-Item -ItemType Directory -Force -Path $Dist | Out-Null
    Invoke-Item $Dist
  })
  $btnCancel.Add_Click({
    if (-not $ui.Building) { return }
    $ui.Cancelled = $true
    $statusText.Text = 'Stopping the build...'
    $ui.Run.Kill()
  })
  $btnBuild.Add_Click({
    [void]$grid.EndEdit()
    & $validate
    $targets = & $checkedArches
    if ($ui.Building -or $ui.HasError -or $targets.Count -eq 0) { return }

    $logDir = Join-Path $Renamed 'logs'
    New-Item -ItemType Directory -Force -Path $logDir | Out-Null
    $logFile = Join-Path $logDir ("build-{0:yyyyMMdd-HHmmss}.log" -f (Get-Date))
    $ui.LogWriter = [System.IO.StreamWriter]::new($logFile, $false, [System.Text.UTF8Encoding]::new($false))
    $ui.LogWriter.AutoFlush = $true
    $log.Clear()
    $log.AppendText("Log file: $logFile`r`n")

    $ui.Run = [RenamedBuildRun]::new()
    $ui.Cancelled = $false; $ui.Started = Get-Date; $ui.Total = $targets.Count; $ui.Finished = 0
    $childArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $ScriptPath, '-Name', [string]$nameCell.Value, '-Arch', ($targets -join ','), '-StatusMarkers')
    try { $ui.Run.Start([Environment]::ProcessPath, $childArgs, $Root) }
    catch {
      $ui.LogWriter.Dispose(); $ui.LogWriter = $null
      [void][System.Windows.Forms.MessageBox]::Show($form, "Could not start the build: $($_.Exception.Message)", $form.Text, 'OK', 'Error')
      return
    }
    & $setBuilding $true
    & $setStatusCell 'Starting...' $blue
    $statusText.Text = 'Starting the build...'
    $timer.Start()
  })

  $form.Add_FormClosing({
    param($s, $e)
    if (-not $ui.Building) { return }
    $answer = [System.Windows.Forms.MessageBox]::Show($form, 'A build is running. Stop it and close?', $form.Text, 'YesNo', 'Warning')
    if ($answer -ne 'Yes') { $e.Cancel = $true; return }
    $ui.Cancelled = $true
    $ui.Run.Kill()
    $timer.Stop()
    if ($ui.LogWriter) { $ui.LogWriter.Dispose(); $ui.LogWriter = $null }
  })

  $form.Add_Shown({ $grid.ClearSelection() })
  & $validate
  [System.Windows.Forms.Application]::Run($form)
  $timer.Dispose()
  $form.Dispose()
}

# ---------------------------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------------------------
if (-not $PSBoundParameters.ContainsKey('Name')) {
  Show-BuilderGui
  return
}

if ([Console]::IsOutputRedirected) { $PSStyle.OutputRendering = 'PlainText' }
$err = Test-AppName $Name
if ($err) { throw $err }
# pwsh -File passes "-Arch x64,arm64" as one string.
$targets = @($Arch -split ',' | ForEach-Object Trim | Where-Object { $_ } | Select-Object -Unique)
foreach ($a in $targets) { if ($a -notin $Arches) { throw "Unknown architecture '$a'. Use $($Arches -join ', ')." } }
if ($targets.Count -eq 0) { throw 'No architecture to build.' }
Invoke-RenamedBuild $Name $targets
