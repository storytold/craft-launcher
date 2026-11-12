<#
.SYNOPSIS
  Build non-executable installer metadata fixtures; never install them.
#>
param(
  [Parameter(Mandatory = $true)] [string] $OutputDirectory,
  [ValidateSet('x64','x86','arm64')] [string[]] $Architectures = @('x64','x86','arm64'),
  # Exercise the command-capture and mutation-result rules without WiX, COM or an MSI.
  [switch] $SelfTestOnly
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$policy = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'install-policy.json') | ConvertFrom-Json
# One definition of the fixture build's extensions, localization and defines, used by the positive
# build and by every mutant build. A mutant built with fewer defines would fail for a reason that has
# nothing to do with the guard, and would then look "caught".
$extensions = @($policy.extensions | ForEach-Object { '-ext'; "WixToolset.$_.wixext" })
$locations = @($policy.localization | ForEach-Object { '-loc'; Join-Path $PSScriptRoot $_ })
function Get-FixtureBuildArguments([string] $Stage) {
  return @(
    '-d', 'Version=0.0.1',
    '-d', 'DisplayVersion=0.0.1-metadata-test',
    '-d', "BinDir=$Stage",
    '-d', "IconPath=$(Join-Path $root $policy.icon)",
    '-d', "DocsDir=$(Join-Path $Stage 'docs')",
    '-d', "ModelsDir=$(Join-Path $Stage 'models')",
    '-d', "AppNoticesDir=$(Join-Path $Stage 'notices')",
    '-d', 'HasCraftFontLicences=1',
    '-d', 'HasCli=1',
    '-d', "BannerBmp=$(Join-Path $Stage 'banner.bmp')",
    '-d', "DialogBmp=$(Join-Path $Stage 'dialog.bmp')",
    '-d', "UiBannerBmp=$(Join-Path $Stage 'banner.bmp')",
    '-d', "UiDialogBmp=$(Join-Path $Stage 'dialog.bmp')",
    '-d', "LicenseRtf=$(Join-Path $Stage 'licence.rtf')"
  )
}
function Invoke-CapturedCommand {
  # Run a native command and return its merged stdout/stderr text and its exit code, as one object.
  # Windows PowerShell 5.1 turns a native command's redirected stderr line into an ErrorRecord, and
  # under $ErrorActionPreference = 'Stop' that is terminating, so the capture runs with 'Continue'
  # and the caller's preference is restored in finally. The exit code is read in the same scope as
  # the call and returned explicitly: nothing depends on $LASTEXITCODE surviving into another scope.
  param(
    [Parameter(Mandatory = $true)] [string] $Name,
    [Parameter(Mandatory = $true)] [scriptblock] $Command
  )
  $previous = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  try {
    # The engine sets $LASTEXITCODE in the global scope, but an unqualified read from here is resolved
    # dynamically through this function's callers, so a caller that has its own $LASTEXITCODE (the
    # self-test does, on purpose) would win. Read the global one. Resetting it to $null first also
    # means a scriptblock that never reaches its native command cannot look like a success.
    $global:LASTEXITCODE = $null
    $output = (& $Command 2>&1 | Out-String)
    $code = $global:LASTEXITCODE
    if ($null -eq $code) {
      throw "Harness: '$Name' ran no native command, or reported no exit code; refusing to treat that as success."
    }
    return [pscustomobject]@{ Output = [string]$output; ExitCode = [int]$code }
  }
  finally {
    $ErrorActionPreference = $previous
  }
}
function Assert-ToolAvailable {
  # Fail with the tool's name, before any result is interpreted, if the packaging runner lacks it.
  param(
    [Parameter(Mandatory = $true)] [string] $Name,
    [Parameter(Mandatory = $true)] [string] $Why
  )
  $command = Get-Command -Name $Name -CommandType Application -ErrorAction SilentlyContinue
  if ($null -eq $command) {
    throw "Harness: '$Name' is not on PATH, so $Why cannot be checked. Install it on the packaging runner; this is not a test result."
  }
  return $command.Source
}
function Get-MutantBuildOutcome {
  # The mutation rule, kept pure so it can be self-tested without building anything: a mutant must
  # build, and a refusal is only a catch when that case names one specific error code.
  param(
    [Parameter(Mandatory = $true)] [string] $Name,
    [Parameter(Mandatory = $true)] [string] $Why,
    [Parameter(Mandatory = $true)] [AllowNull()] [object] $Build,
    [AllowNull()] [string] $RefusalMatch
  )
  if ($null -eq $Build -or 'ExitCode' -notin @($Build.PSObject.Properties.Name)) {
    throw "Harness: mutation '$Name' has no captured build result to judge."
  }
  if ($Build.ExitCode -eq 0) { return 'built' }
  if ($RefusalMatch -and ($Build.Output -match $RefusalMatch)) { return 'refused' }
  throw "Mutation '$Name' did not build, so it proves nothing about the guard: $Why. build said: $($Build.Output.Trim())"
}
function Assert-MutationCaught {
  # The checker rule, likewise pure: it must fail, and it must fail for the rule the case names.
  param(
    [Parameter(Mandatory = $true)] [string] $Name,
    [Parameter(Mandatory = $true)] [string] $Expect,
    [Parameter(Mandatory = $true)] [AllowNull()] [object] $Check
  )
  if ($null -eq $Check -or 'ExitCode' -notin @($Check.PSObject.Properties.Name)) {
    throw "Harness: mutation '$Name' has no captured checker result to judge."
  }
  if ($Check.ExitCode -eq 0) {
    throw "Mutation '$Name' was not caught by check-install-policy.ps1."
  }
  if ($Check.Output -notmatch $Expect) {
    throw "Mutation '$Name' was rejected, but not by the rule it exists to prove (expected '$Expect'). check-install-policy.ps1 said: $($Check.Output.Trim())"
  }
}
function Invoke-HarnessSelfTest {
  # The harness's own rules, tested without WiX, an MSI, COM or an install: the child commands here
  # only write a line to stderr and choose an exit code. If any of this fails, no mutation result
  # may be believed, so it runs before anything is built.
  $stderrNonZero = Invoke-CapturedCommand -Name 'self-test-stderr-nonzero' -Command {
    & $shell -NoProfile -Command 'Write-Error "harness self test"; exit 3'
  }
  if ($stderrNonZero.ExitCode -ne 3) { throw "Harness self test: a command that wrote to stderr and exited 3 reported $($stderrNonZero.ExitCode)." }
  if ($stderrNonZero.Output -notmatch 'harness self test') { throw 'Harness self test: the stderr text of a failed command was not captured.' }
  $stderrZero = Invoke-CapturedCommand -Name 'self-test-stderr-zero' -Command {
    & $shell -NoProfile -Command 'Write-Error "a warning only"; exit 0'
  }
  if ($stderrZero.ExitCode -ne 0) { throw "Harness self test: a warning on stderr changed the exit code to $($stderrZero.ExitCode)." }
  if ($stderrZero.Output -notmatch 'a warning only') { throw 'Harness self test: a stderr warning was lost.' }
  # Negative control for the lookup above: a caller-local $LASTEXITCODE must not be returned.
  $LASTEXITCODE = 7
  $fresh = Invoke-CapturedCommand -Name 'self-test-exit-code-scope' -Command { & $shell -NoProfile -Command 'exit 0' }
  if ($fresh.ExitCode -ne 0) { throw "Harness self test: the captured exit code came from somewhere other than the command (got $($fresh.ExitCode), stale caller value was 7)." }
  # A scriptblock that never reaches a native command must be refused, not reported as success.
  $threw = $false
  try { [void](Invoke-CapturedCommand -Name 'self-test-no-native' -Command { $null = 1 }) }
  catch { $threw = $true }
  if (-not $threw) { throw 'Harness self test: a scriptblock that ran no native command was reported as a success.' }
  # An exception from the tool runner (a bad binding, an unloadable scriptblock, a missing binary that
  # slips past the presence check) must reach the caller: the helper has no catch, so no MSI result can
  # be interpreted after a runner failure.
  $threw = $false
  try { [void](Invoke-CapturedCommand -Name 'self-test-runner-exception' -Command { throw 'tool runner blew up' }) }
  catch { $threw = $true }
  if (-not $threw) { throw 'Harness self test: an exception from the tool runner was swallowed and the capture looked like a result.' }
  if ((Get-MutantBuildOutcome -Name 'self-test-built' -Why 'self test' -Build ([pscustomobject]@{ Output = ''; ExitCode = 0 })) -ne 'built') {
    throw 'Harness self test: a successful build was not accepted.'
  }
  foreach ($case in @(
      @{ Output = 'error WIX0001: something unrelated'; Refusal = $null },
      @{ Output = 'error WIX0001: something unrelated'; Refusal = 'ICE64' })) {
    $threw = $false
    try {
      [void](Get-MutantBuildOutcome -Name 'self-test-not-built' -Why 'self test' -Build ([pscustomobject]@{ Output = $case.Output; ExitCode = 1 }) -RefusalMatch $case.Refusal)
    }
    catch { $threw = $true }
    if (-not $threw) { throw 'Harness self test: an arbitrary build failure was accepted as a catch.' }
  }
  if ((Get-MutantBuildOutcome -Name 'self-test-refused' -Why 'self test' -Build ([pscustomobject]@{ Output = 'error WIX0204: duplicate directory'; ExitCode = 1 }) -RefusalMatch 'WIX0204') -ne 'refused') {
    throw 'Harness self test: a refusal matching the named code was not recognised.'
  }
  foreach ($check in @(
      @{ Output = 'PASS something'; ExitCode = 0 },
      @{ Output = 'Cleanup policy: a rule this case does not name'; ExitCode = 1 })) {
    $threw = $false
    try {
      Assert-MutationCaught -Name 'self-test-check' -Expect 'no uninstall row removes' -Check ([pscustomobject]@{ Output = $check.Output; ExitCode = $check.ExitCode })
    }
    catch { $threw = $true }
    if (-not $threw) { throw 'Harness self test: a checker result was accepted although it was not the named rejection.' }
  }
  Assert-MutationCaught -Name 'self-test-check' -Expect 'no uninstall row removes' -Check ([pscustomobject]@{ Output = 'Cleanup policy: no uninstall row removes CRAFT'; ExitCode = 1 })
  $threw = $false
  try { [void](Assert-ToolAvailable -Name 'artcraft-launcher-no-such-tool' -Why 'the harness self test') }
  catch { $threw = $true }
  if (-not $threw) { throw 'Harness self test: a missing tool was not refused by name.' }
  Write-Output 'PASS harness self test: stderr capture on both exit paths, captured global exit-code scope, a no-native command and a tool-runner exception both refused, build and refusal rules, named checker rejection, missing-tool refusal.'
}
$shell = (Get-Process -Id $PID).Path
Invoke-HarnessSelfTest
if ($SelfTestOnly) { return }
$wix = Assert-ToolAvailable -Name 'wix' -Why 'the installer metadata gate'
[void](New-Item -ItemType Directory -Force -Path $OutputDirectory)
$output = (Resolve-Path -LiteralPath $OutputDirectory).Path
Add-Type -AssemblyName System.Drawing
foreach ($arch in $Architectures) {
  $stage = Join-Path $output $arch
  [void](New-Item -ItemType Directory -Force -Path $stage)
  foreach ($name in $policy.payloads) {
    $destination = Join-Path $stage $name
    [void](New-Item -ItemType Directory -Force -Path ([IO.Path]::GetDirectoryName($destination)))
    Set-Content -LiteralPath $destination -Encoding Ascii -Value 'NON-EXECUTABLE MSI METADATA TEST FIXTURE'
  }
  foreach ($folder in @('docs','models','notices')) {
    $path = Join-Path $stage $folder
    [void](New-Item -ItemType Directory -Force -Path $path)
    Set-Content -LiteralPath (Join-Path $path "$folder-fixture.txt") -Value 'Metadata test fixture, not a user file or model.'
  }
  foreach ($model in @('metadata-one.rten','metadata-two.rten')) {
    Set-Content -LiteralPath (Join-Path (Join-Path $stage 'models') $model) -Encoding Ascii -Value 'NON-EXECUTABLE MODEL METADATA FIXTURE'
  }
  foreach ($spec in @(@('banner.bmp',493,58),@('dialog.bmp',493,312))) {
    $bitmap = [Drawing.Bitmap]::new($spec[1],$spec[2],[Drawing.Imaging.PixelFormat]::Format24bppRgb)
    try { $bitmap.Save((Join-Path $stage $spec[0]),[Drawing.Imaging.ImageFormat]::Bmp) }
    finally { $bitmap.Dispose() }
  }
  Set-Content -LiteralPath (Join-Path $stage 'licence.rtf') -Encoding Ascii -Value '{\rtf1 Metadata test fixture.}'
  $sources = @($policy.sources | ForEach-Object { Join-Path $PSScriptRoot $_ })
  $msi = Join-Path $stage "$($policy.id)-NOT-INSTALLABLE.msi"
  $defines = Get-FixtureBuildArguments $stage
  & $wix build @sources -arch $arch @extensions -culture en-US @defines -o $msi @locations
  if ($LASTEXITCODE -ne 0) { throw "WiX validation failed for $arch (exit $LASTEXITCODE)." }
  & $shell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'check-install-policy.ps1') -MsiPath $msi
  if ($LASTEXITCODE -ne 0) { throw "Compiled installer policy failed for $arch (exit $LASTEXITCODE)." }
  Write-Output "PASS $arch metadata-only package. This is not an executable installer."
}

# #584: a cleanup guard that has only ever been seen to pass proves nothing. Each case below mutates
# the shipped authoring, builds the same non-installable metadata fixture with the same sources,
# extensions, localization and defines as the positive build, and must then be rejected by
# check-install-policy.ps1 with the message the case names. A mutant that merely fails to build
# proves nothing about the guard, so that is a test failure, not a catch; only a case carrying a
# RefusalMatch may be caught by WiX refusing it, and then only on that specific error code. Nothing
# is installed, run or signed, no payload of a product is involved, and the cases run once, for the
# first architecture, to keep the packaging job bounded; the RemoveFile table is the same for every
# architecture.
$authoringPath = Join-Path $PSScriptRoot $policy.sources[0]
$authoring = Get-Content -Raw -LiteralPath $authoringPath
$nl = if ($authoring.Contains("`r`n")) { "`r`n" } else { "`n" }
$suiteRow = '        <RemoveFolder Id="RemoveSuiteFolder" Directory="CraftSuiteFolder" On="uninstall" />'
$appRow = '        <RemoveFolder Id="RemoveInstallFolder" Directory="INSTALLFOLDER" On="uninstall" />'
$mutations = @(
  @{
    Name = 'same-relative-path-under-another-root'
    Why = 'a matching Programs/ArtCraft/Launcher suffix under DesktopFolder is not the owned LocalAppDataFolder path'
    Expect = 'not one of the folders this MSI creates'
    Pairs = @(
      @{ From = '    <StandardDirectory Id="DesktopFolder" />'; To = '    <StandardDirectory Id="DesktopFolder"><Directory Id="OtherPrograms" Name="Programs"><Directory Id="OtherSuite" Name="ArtCraft"><Directory Id="OtherInstall" Name="Launcher" /></Directory></Directory></StandardDirectory>' },
      @{ From = $suiteRow; To = $suiteRow + $nl + '        <RemoveFolder Id="RemoveOtherRoot" Directory="OtherInstall" On="uninstall" />' }
    )
  },
  @{
    Name = 'shared-programs-row'
    Why = 'the #584 row returns, so uninstall would remove the shared %LOCALAPPDATA%\Programs folder'
    Expect = 'not one of the folders this MSI creates'
    Pairs = @(@{ From = $suiteRow; To = $suiteRow + $nl + '        <RemoveFolder Id="RemoveLocalProgramsFolder" Directory="LocalProgramsFolder" On="uninstall" />' })
  },
  @{
    Name = 'shared-programs-alias'
    Why = 'a second Directory row resolves to the same shared root, so matching on the id alone would miss it'
    Expect = 'not one of the folders this MSI creates'
    # WiX may refuse this mutant itself (two sibling Directory rows with the same DefaultDir name),
    # and a refusal is not evidence about the guard. It is therefore NOT accepted by default: the
    # case fails and reports the build log, and a reviewer who wants the refusal accepted adds ONE
    # specific code here as a deliberate change (for example RefusalMatch = 'WIX0204'), never a
    # pattern that matches any ICE or WIX error.
    Pairs = @(
      @{ From = '    <StandardDirectory Id="LocalAppDataFolder">'; To = '    <StandardDirectory Id="LocalAppDataFolder">' + $nl + '      <Directory Id="SharedProgramsAlias" Name="Programs" />' },
      @{ From = $suiteRow; To = $suiteRow + $nl + '        <RemoveFolder Id="RemoveSharedProgramsAlias" Directory="SharedProgramsAlias" On="uninstall" />' }
    )
  },
  @{
    Name = 'install-time-cleanup'
    Why = 'On="both" would delete the user''s files during an update, not only on uninstall'
    Expect = 'must run on uninstall only'
    Pairs = @(@{ From = '<RemoveFolder Id="RemoveSuiteFolder" Directory="CraftSuiteFolder" On="uninstall" />'; To = '<RemoveFolder Id="RemoveSuiteFolder" Directory="CraftSuiteFolder" On="both" />' })
  },
  @{
    Name = 'desktop-folder-cleanup'
    Why = 'a shared folder that is not ours must be refused even when it is not the Programs root, so the rule is not a one-name blocklist'
    Expect = 'not one of the folders this MSI creates'
    Pairs = @(@{ From = $suiteRow; To = $suiteRow + $nl + '        <RemoveFolder Id="RemoveDesktopFolderCleanup" Directory="DesktopFolder" On="uninstall" />' })
  },
  @{
    Name = 'alias-setter-action'
    Why = 'a second property action writes the install folder itself and points it at the shared Programs root; the remembered-path tests alone would never see it'
    Expect = 'may write the install folder'
    Pairs = @(@{ From = '    <SetProperty Id="INSTALLFOLDER" Action="CraftRestoreLauncherFolder"'; To = '    <SetProperty Id="INSTALLFOLDER" Action="CraftRetargetInstallFolder" Value="[LocalProgramsFolder]" Before="InstallInitialize" Sequence="execute" />' + $nl + '    <SetProperty Id="INSTALLFOLDER" Action="CraftRestoreLauncherFolder"' })
  },
  @{
    Name = 'file-pattern-cleanup'
    Why = 'a RemoveFile row with a file name deletes files rather than an empty folder, and portable mode keeps user data beside the exe'
    Expect = 'rather than an empty folder'
    Pairs = @(@{ From = $suiteRow; To = $suiteRow + $nl + '        <RemoveFile Id="RemoveLauncherFiles" Directory="INSTALLFOLDER" Name="*" On="uninstall" />' })
  },
  @{
    Name = 'retargeted-restore-action'
    Why = 'pointing the remembered-folder restore at the shared Programs root is a runtime alias that no compiled path check would see'
    Expect = 'may write the install folder'
    Pairs = @(@{ From = 'Value="[PREVIOUSINSTALLFOLDER]"'; To = 'Value="[LocalProgramsFolder]"' })
  },
  @{
    Name = 'dropped-app-row'
    Why = 'silently dropping the launcher folder cleanup leaves the install folder behind for good'
    Expect = 'no uninstall row removes'
    Pairs = @(@{ From = $appRow + $nl; To = '' })
  },
  @{
    Name = 'dropped-suite-row'
    Why = 'silently dropping the suite folder cleanup leaves the ArtCraft folder behind for good'
    Expect = 'no uninstall row removes'
    Pairs = @(@{ From = $suiteRow + $nl; To = '' })
  }
)
$mutationArch = $Architectures[0]
$mutationStage = Join-Path $output $mutationArch
# Precondition: the unmutated fixture on this stage must still pass, or a mutant being rejected would
# prove nothing about the guard.
$positiveMsi = Join-Path $mutationStage "$($policy.id)-NOT-INSTALLABLE.msi"
if (-not (Test-Path -LiteralPath $positiveMsi)) {
  throw "The positive fixture for $mutationArch is missing; the mutation cases would prove nothing without it."
}
$positive = Invoke-CapturedCommand -Name "positive-$mutationArch" -Command {
  & $shell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'check-install-policy.ps1') -MsiPath $positiveMsi
}
if ($positive.ExitCode -ne 0) {
  throw "The unmutated fixture no longer passes check-install-policy.ps1 for $mutationArch; fix that before reading any mutation result: $($positive.Output.Trim())"
}
foreach ($mutation in $mutations) {
  $caseDir = Join-Path $output "mutation-$($mutation.Name)"
  [void](New-Item -ItemType Directory -Force -Path $caseDir)
  $mutantText = $authoring
  foreach ($pair in $mutation.Pairs) {
    if (-not $mutantText.Contains($pair.From)) {
      throw "Mutation '$($mutation.Name)' no longer matches the authoring it mutates; update the case instead of skipping it."
    }
    $mutantText = $mutantText.Replace($pair.From, $pair.To)
  }
  $mutantSource = Join-Path $caseDir 'artcraft-launcher.mutant.wxs'
  [IO.File]::WriteAllText($mutantSource, $mutantText, [System.Text.UTF8Encoding]::new($false))
  $mutantMsi = Join-Path $caseDir "$($policy.id)-NOT-INSTALLABLE-$($mutation.Name).msi"
  # The mutated copy replaces the first source; every other source, extension, localization and
  # define stays exactly as the positive build used them, so a failure can only come from the
  # mutation itself.
  $mutantSources = @($policy.sources | ForEach-Object { Join-Path $PSScriptRoot $_ })
  if ($mutantSources.Count -lt 1) { throw 'install-policy.json lists no WiX source to mutate.' }
  $mutantSources[0] = $mutantSource
  $mutantDefines = Get-FixtureBuildArguments $mutationStage
  $build = Invoke-CapturedCommand -Name "build-$($mutation.Name)" -Command {
    & $wix build @mutantSources -arch $mutationArch @extensions -culture en-US @mutantDefines -o $mutantMsi @locations
  }
  $refusal = $null
  if ($mutation.ContainsKey('RefusalMatch')) { $refusal = $mutation.RefusalMatch }
  if ((Get-MutantBuildOutcome -Name $mutation.Name -Why $mutation.Why -Build $build -RefusalMatch $refusal) -eq 'refused') {
    Write-Output "CAUGHT (WiX refused it, $($mutation.Name)): $($mutation.Why)"
    continue
  }
  $check = Invoke-CapturedCommand -Name "check-$($mutation.Name)" -Command {
    & $shell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'check-install-policy.ps1') -MsiPath $mutantMsi
  }
  Assert-MutationCaught -Name $mutation.Name -Expect $mutation.Expect -Check $check
  Write-Output "CAUGHT (compiled-table check, $($mutation.Name)): $($mutation.Why)"
}

# Every negative control above must return nonzero. Those captured child codes are expected,
# not this script's result; PowerShell CI otherwise inherits the final rejected mutant's code.
Write-Output "PASS installer metadata: $($Architectures.Count) architectures; $($mutations.Count) negative controls."
exit 0
