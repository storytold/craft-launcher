<#
.SYNOPSIS
  Check the compiled installation policy without installing any product.
.DESCRIPTION
  Uses Windows Installer's safe session (OpenPackage option 1). Only property-setting custom
  actions are exercised. No AppSearch, install, repair, removal or file-writing action runs.
#>
param([Parameter(Mandatory = $true)] [string] $MsiPath)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$policy = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'install-policy.json') | ConvertFrom-Json
function Read-Rows($Database, [string] $Sql, [int] $Columns) {
  $view = $Database.OpenView($Sql)
  try {
    [void]$view.Execute()
    while ($null -ne ($record = $view.Fetch())) {
      try {
        $values = @(for ($i = 1; $i -le $Columns; $i++) {
          $record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, @($i))
        })
        ,$values
      } finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($record) }
    }
  } finally {
    [void]$view.Close()
    [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($view)
  }
}
function Set-SessionProperty($Session, [string] $Name, [string] $Value) {
  [void]$Session.GetType().InvokeMember('Property', 'SetProperty', $null, $Session, @($Name, $Value))
}
function Get-SessionProperty($Session, [string] $Name) {
  $Session.GetType().InvokeMember('Property', 'GetProperty', $null, $Session, @($Name))
}
function Get-DirectoryPath($Directories, [string] $Id) {
  # A Directory row's path, built only from its DefaultDir chain, so two rows that resolve to the
  # same Windows folder compare equal even when their ids differ (an alias, or the shared root
  # spelled with another Directory row). No Windows API and no install: the compiled table only.
  $segments = @()
  $current = $Id
  $steps = 0
  while ($true) {
    if (-not $Directories.ContainsKey($current)) {
      throw "Cleanup policy: '$Id' walks into '$current', which the Directory table does not define."
    }
    $steps++
    if ($steps -gt 256) { throw "Cleanup policy: the Directory table chain for '$Id' does not terminate." }
    # Standard directory rows often have DefaultDir='.'. Keep the owned known-folder root
    # distinct from DesktopFolder and other roots that otherwise flatten to SourceDir.
    if ($current -eq 'LocalAppDataFolder') {
      $segments = @('[LocalAppDataFolder]') + $segments
      break
    }
    $parent = [string]$Directories[$current][0]
    $name = [string]$Directories[$current][1]
    # Directory.DefaultDir is a name, a 'short|long' pair, a 'target:source' pair, or '.' for the
    # parent's folder; only the target name is part of the path, so drop the source name first.
    $name = ($name.Trim() -split ':')[0]
    $name = ($name -split '\|')[-1].Trim()
    if ($name -ne '' -and $name -ne '.') { $segments = @($name) + $segments }
    if ($parent -eq '' -or $parent -eq $current) { break }
    $current = $parent
  }
  return ($segments -join '\').ToUpperInvariant()
}
function Get-DirectoryAncestorIds($Directories, [string] $Id) {
  # Every Directory row above $Id, nearest first: the folders that must not be retargeted either.
  $ids = @()
  $current = $Id
  $steps = 0
  while ($true) {
    if (-not $Directories.ContainsKey($current)) { break }
    $steps++
    if ($steps -gt 256) { break }
    $parent = [string]$Directories[$current][0]
    if ($parent -eq '' -or $parent -eq $current) { break }
    $ids += $parent
    $current = $parent
  }
  return $ids
}
function Assert-CleanupPolicy($Database, $Directories, [string] $InstallRoot) {
  # #584: an uninstall may remove only the folders this MSI created. %LOCALAPPDATA%\Programs
  # (LocalProgramsFolder) is shared with the user and with every other per-user application, so a
  # RemoveFile row may not name it, and may not reach it through an equally-resolving Directory row.
  # The compiled table is checked, not one .wxs: any source in the build can add a row. Failing
  # closed is cheap (a package with an extra cleanup row is corrected in review); removing a
  # shared folder is not.
  if (-not $Directories.ContainsKey($InstallRoot)) {
    throw "Cleanup policy: the package defines no '$InstallRoot' directory."
  }
  $suiteRoot = [string]$Directories[$InstallRoot][0]
  if ($suiteRoot -eq '') { throw "Cleanup policy: '$InstallRoot' must not be a top-level folder." }
  $installPath = Get-DirectoryPath $Directories $InstallRoot
  $suitePath = Get-DirectoryPath $Directories $suiteRoot
  if ($suitePath -eq '' -or $installPath -eq $suitePath) {
    throw 'Cleanup policy: the install folder must be its own subfolder of the suite folder; otherwise removing the suite folder removes a folder other applications share.'
  }
  $owned = @($installPath, $suitePath)
  $tables = @(Read-Rows $Database 'SELECT `Name` FROM `_Tables`' 1 | ForEach-Object { $_[0] })
  $removeRows = @()
  if ('RemoveFile' -in $tables) {
    $removeRows = @(Read-Rows $Database 'SELECT `FileKey`, `Component_`, `DirProperty`, `FileName`, `InstallMode` FROM `RemoveFile`' 5)
  }
  $components = @{}
  foreach ($row in (Read-Rows $Database 'SELECT `Component` FROM `Component`' 1)) { $components[[string]$row[0]] = $true }
  $featured = @{}
  foreach ($row in (Read-Rows $Database 'SELECT `Component_` FROM `FeatureComponents`' 1)) { $featured[[string]$row[0]] = $true }
  $retained = @{}
  foreach ($remove in $removeRows) {
    $id = [string]$remove[0]
    $component = [string]$remove[1]
    $target = [string]$remove[2]
    $name = [string]$remove[3]
    $mode = [string]$remove[4]
    if ($target -eq '') { throw "Cleanup policy: RemoveFile '$id' names no folder to remove." }
    if (-not $Directories.ContainsKey($target)) {
      throw "Cleanup policy: RemoveFile '$id' targets '$target', which is not a folder this package declares; only the folders it creates may be removed."
    }
    $path = Get-DirectoryPath $Directories $target
    if ($path -notin $owned) {
      throw "Cleanup policy: RemoveFile '$id' removes '$target' ($path), which is not one of the folders this MSI creates. The per-user Programs folder belongs to the user and to other applications."
    }
    if ($name -ne '') {
      throw "Cleanup policy: RemoveFile '$id' deletes '$name' rather than an empty folder; this MSI may remove only the folders it created, never a file pattern inside them (portable mode keeps data beside the exe)."
    }
    if ($mode -ne '2') {
      throw "Cleanup policy: RemoveFile '$id' has InstallMode '$mode'; cleanup must run on uninstall only (2), never during an install or update."
    }
    if (-not $components.ContainsKey($component)) {
      throw "Cleanup policy: RemoveFile '$id' belongs to component '$component', which the Component table does not define."
    }
    if (-not $featured.ContainsKey($component)) {
      throw "Cleanup policy: RemoveFile '$id' belongs to component '$component', which no feature installs, so its cleanup would never run."
    }
    if ($retained.ContainsKey($path)) { $retained[$path]++ } else { $retained[$path] = 1 }
  }
  # A property or directory action can retarget a folder after the table above was built, which would
  # turn an allowed row into a shared-folder removal. Only the documented restore may write the
  # install folder, from the folder recorded by the previous version, and nothing may write the suite
  # folder or a parent of it.
  $reserved = @{}
  foreach ($id in @($InstallRoot, $suiteRoot)) { $reserved[([string]$id).ToUpperInvariant()] = $true }
  foreach ($id in (Get-DirectoryAncestorIds $Directories $suiteRoot)) { $reserved[([string]$id).ToUpperInvariant()] = $true }
  # A cleanup row may use another Directory ID for an owned path. Protect that alias and its
  # parents too, or a setter could turn the allowed alias into a shared-folder removal.
  foreach ($remove in $removeRows) {
    $target = [string]$remove[2]
    $reserved[$target.ToUpperInvariant()] = $true
    foreach ($id in (Get-DirectoryAncestorIds $Directories $target)) { $reserved[([string]$id).ToUpperInvariant()] = $true }
  }
  foreach ($action in (Read-Rows $Database 'SELECT `Action`, `Type`, `Source`, `Target` FROM `CustomAction`' 4)) {
    # For the property and directory setters (base types 51 and 35) the CustomAction row carries the
    # name of the property or directory being written in Source and the new value in Target (the
    # reverse of the reading order here). Type also holds the base code plus scheduling flags
    # (0x40 continue, 0x400 deferred, ...), so mask the flags before comparing.
    $actionType = [int]([string]$action[1]) -band 0x3F
    if ($actionType -ne 35 -and $actionType -ne 51) { continue }
    $writes = ([string]$action[2]).ToUpperInvariant()
    if (-not $reserved.ContainsKey($writes)) { continue }
    $restore = $actionType -eq 51 -and [string]$action[0] -eq 'CraftRestoreLauncherFolder' -and
      $writes -eq $InstallRoot.ToUpperInvariant() -and
      ([string]$action[3]).ToUpperInvariant() -eq '[PREVIOUSINSTALLFOLDER]'
    if ($restore) { continue }
    throw "Cleanup policy: action '$($action[0])' writes '$($action[2])' (CustomAction type $($action[1])); only CraftRestoreLauncherFolder may write the install folder, and nothing may write the suite folder or a shared parent of it."
  }
  foreach ($required in $owned) {
    if (-not $retained.ContainsKey($required)) {
      throw "Cleanup policy: no uninstall row removes '$required'; the package must clean up its own install and suite folders, not only the shared ones it must not touch."
    }
  }
}
$installer = $null; $database = $null; $session = $null
try {
  $installer = New-Object -ComObject WindowsInstaller.Installer
  $file = (Resolve-Path -LiteralPath $MsiPath).Path
  $database = $installer.OpenDatabase($file, 0)
  $session = $installer.OpenPackage($file, 1)
  $properties = @{}
  foreach ($row in (Read-Rows $database 'SELECT `Property`, `Value` FROM `Property`' 2)) { $properties[$row[0]] = $row[1] }
  $secureProperties = @($properties['SecureCustomProperties'] -split ';')
  if ('INSTALLFOLDER' -notin $secureProperties) {
    throw 'The selected installation folder must survive elevation (secure INSTALLFOLDER).'
  }
  if ($policy.mode -eq 'perUser') {
    if ($properties.ContainsKey('ALLUSERS')) { throw 'Launcher must remain per-user; machine self-update is not part of this change.' }
    if ($properties['UpgradeCode'].Trim('{}') -ne $policy.upgradeCode) { throw 'Launcher upgrade identity changed.' }
    $dirs = @{}
    foreach ($row in (Read-Rows $database 'SELECT `Directory`, `Directory_Parent`, `DefaultDir` FROM `Directory`' 3)) { $dirs[$row[0]] = @($row[1],$row[2]) }
    if ($dirs['INSTALLFOLDER'][0] -ne 'CraftSuiteFolder' -or $dirs['CraftSuiteFolder'][1] -ne 'ArtCraft' -or
        $dirs['CraftSuiteFolder'][0] -ne 'LocalProgramsFolder' -or $dirs['LocalProgramsFolder'][0] -ne 'LocalAppDataFolder') {
      throw 'Launcher new-install folder must be under the per-user ArtCraft suite root.'
    }
    # #584: the folders left behind on uninstall are the launcher's own, and nothing else.
    Assert-CleanupPolicy -Database $database -Directories $dirs -InstallRoot 'INSTALLFOLDER'
    foreach ($explicit in @('', 'F:\Chosen exact launcher')) {
      Set-SessionProperty $session 'PREVIOUSINSTALLFOLDER' 'E:\Existing exact launcher'
      Set-SessionProperty $session 'INSTALLFOLDER' $explicit
      $row = @(Read-Rows $database 'SELECT `Condition` FROM `InstallUISequence` WHERE `Action` = ''CraftRestoreLauncherFolder''' 1)[0]
      if ($session.EvaluateCondition($row[0]) -eq 1) {
        if ($session.DoAction('CraftRestoreLauncherFolder') -ne 1) { throw 'Launcher restore action failed.' }
      }
      $expected = if ($explicit) { $explicit } else { 'E:\Existing exact launcher' }
      if ((Get-SessionProperty $session 'INSTALLFOLDER') -ne $expected) { throw 'Launcher custom or existing path was lost.' }
    }
    $rows = @(Read-Rows $database 'SELECT `Root`, `Key`, `Name`, `Value` FROM `Registry`' 4)
    if (@($rows | Where-Object { $_[0] -ne '1' }).Count) { throw 'Launcher registration must stay under the current account.' }
    foreach ($entry in $policy.registry) {
      if (@($rows | Where-Object { $_[0] -eq $entry[0] -and $_[1] -eq $entry[1] -and $_[2] -eq $entry[2] -and $_[3] -eq $entry[3] }).Count -ne 1) {
        throw 'Launcher registration or shortcut preference changed unexpectedly.'
      }
    }
    # Silent and reduced-UI updates never show the wizard, so the folder must also be restored in the execute sequence.
    $execute = @{}
    foreach ($row in (Read-Rows $database 'SELECT `Action`, `Sequence` FROM `InstallExecuteSequence`' 2)) { $execute[$row[0]] = [int]$row[1] }
    if (-not ($execute.ContainsKey('CraftRestoreLauncherFolder') -and $execute['AppSearch'] -lt $execute['CraftRestoreLauncherFolder'] -and
        $execute['CraftRestoreLauncherFolder'] -lt $execute['CostInitialize'])) {
      throw 'Silent updates must restore the launcher folder after AppSearch and before directory costing.'
    }
    Write-Output 'PASS ArtCraft Launcher: per-user scope, suite folder, remembered/custom paths and existing registration. No installation performed.'
    return
  }
  if ($properties['ALLUSERS'] -ne '2' -or $properties['MSIINSTALLPERUSER'] -ne '1') {
    throw 'Policy regression: new packages must default to the per-user dual-scope context.'
  }
  if ($properties['UpgradeCode'].Trim('{}') -ne $policy.upgradeCode) { throw 'The existing upgrade identity must not change.' }
  $directories = @{}
  foreach ($row in (Read-Rows $database 'SELECT `Directory`, `Directory_Parent`, `DefaultDir` FROM `Directory`' 3)) {
    $directories[$row[0]] = @($row[1], $row[2])
  }
  if ($directories['INSTALLFOLDER'][0] -ne 'CraftSuiteFolder' -or
      $directories['CraftSuiteFolder'][1] -ne 'ArtCraft') { throw 'Expected the app under the ArtCraft suite folder.' }
  # The same compiled-table rule guards a machine-scope policy: a shared folder removed there is a
  # machine-wide data loss, and the guard applies to both paths so neither can drift back (#584).
  Assert-CleanupPolicy -Database $database -Directories $directories -InstallRoot 'INSTALLFOLDER'
  $programs = $directories['CraftSuiteFolder'][0]
  if ($programs -eq 'ProgramFiles6432Folder') { $programs = $directories[$programs][0] }
  if ($programs -notin 'ProgramFilesFolder', 'ProgramFiles64Folder') {
    throw 'Use Windows Installer native folder redirection, not a hard-coded profile path.'
  }
  $guards = @(Read-Rows $database 'SELECT `Condition`, `Description` FROM `LaunchCondition`' 2 |
    Where-Object { $_[0] -match 'CRAFT_(USER|MACHINE)_DIR' })
  if ($guards.Count -ne 2) { throw 'Expected both duplicate-installation guards.' }
  $cases = 0
  foreach ($scope in @('', '2', '1')) {
    foreach ($user in @('', 'D:\Existing user app')) {
      foreach ($machine in @('', 'E:\Existing machine app')) {
        foreach ($installed in @('', '1')) {
          Set-SessionProperty $session 'ALLUSERS' $scope
          Set-SessionProperty $session 'Installed' $installed
          Set-SessionProperty $session 'CRAFT_USER_DIR' $user
          Set-SessionProperty $session 'CRAFT_MACHINE_DIR' $machine
          $allowed = $true
          foreach ($guard in $guards) {
            $value = $session.EvaluateCondition($guard[0])
            if ($value -notin 0, 1) { throw 'Invalid duplicate guard.' }
            if ($value -eq 0) { $allowed = $false }
          }
          $opposite = if ($scope -eq '1') { $user } else { $machine }
          $expected = [bool]$installed -or -not [bool]$opposite
          if ($allowed -ne $expected) { throw "Scope regression: ALLUSERS='$scope', user=$([bool]$user), machine=$([bool]$machine), maintenance=$([bool]$installed)." }
          $cases++
        }
      }
    }
  }
  # Execute the actual authored type-51 actions in a safe session, not copied resolver logic.
  foreach ($scope in @('', '2', '1')) {
    foreach ($explicit in @('', 'F:\Chosen exact app')) {
      Set-SessionProperty $session 'ALLUSERS' $scope
      Set-SessionProperty $session 'INSTALLFOLDER' $explicit
      Set-SessionProperty $session 'CRAFT_USER_DIR' 'D:\Existing user app'
      Set-SessionProperty $session 'CRAFT_MACHINE_DIR' 'E:\Existing machine app'
      $action = if ($scope -eq '1') { 'CraftRestoreMachineFolder' } else { 'CraftRestoreUserFolder' }
      # Evaluate the compiled condition, so the test exercises it instead of calling the action directly.
      $restore = @(Read-Rows $database ('SELECT `Condition` FROM `InstallExecuteSequence` WHERE `Action` = ''{0}''' -f $action) 1)[0]
      $selected = $session.EvaluateCondition($restore[0]) -eq 1
      if ($selected -eq [bool]$explicit) { throw "$action must run only when no folder was supplied (condition: $($restore[0]))." }
      if ($selected -and $session.DoAction($action) -ne 1) { throw 'Remembered-folder property action failed.' }
      $expected = if ($explicit) { $explicit } elseif ($scope -eq '1') { 'E:\Existing machine app' } else { 'D:\Existing user app' }
      if ((Get-SessionProperty $session 'INSTALLFOLDER') -ne $expected) { throw 'Custom or remembered folder was not preserved.' }
    }
  }
  $ui = @{}
  foreach ($row in (Read-Rows $database 'SELECT `Action`, `Sequence` FROM `InstallUISequence`' 2)) { $ui[$row[0]] = [int]$row[1] }
  if (-not ($ui['AppSearch'] -lt $ui['CraftScopeDlg'] -and $ui['CraftScopeDlg'] -lt $ui['LaunchConditions'] -and
      $ui['CraftScopeDlg'] -lt $ui['CraftRestoreUserFolder'] -and $ui['CraftRestoreUserFolder'] -lt $ui['CostInitialize'])) {
    throw 'Scope must be chosen after discovery and before duplicate checks, folder restoration and costing.'
  }
  $execute = @{}
  foreach ($row in (Read-Rows $database 'SELECT `Action`, `Sequence` FROM `InstallExecuteSequence`' 2)) { $execute[$row[0]] = [int]$row[1] }
  foreach ($action in @('CraftRestoreUserFolder','CraftRestoreMachineFolder')) {
    if (-not ($execute['AppSearch'] -lt $execute[$action] -and $execute[$action] -lt $execute['CostInitialize'])) {
      throw 'Silent updates must restore the folder before directory costing.'
    }
  }
  # Switching the first-page choice must reset BOTH properties, including a back-and-forth change.
  $events = @(Read-Rows $database 'SELECT `Event`, `Argument`, `Condition`, `Ordering` FROM `ControlEvent` WHERE `Dialog_` = ''CraftScopeDlg'' AND `Control_` = ''Next'' ORDER BY `Ordering`' 4)
  foreach ($choice in @('user','machine','user')) {
    Set-SessionProperty $session 'CRAFT_SCOPE' $choice
    foreach ($event in $events) {
      if ($event[0] -notmatch '^\[([^\]]+)\]$') { continue }
      $property = $Matches[1]
      if ($session.EvaluateCondition($event[2]) -eq 1) {
        Set-SessionProperty $session $property $(if ($event[1] -eq '{}') { '' } else { $event[1] })
      }
    }
    $allUsers = Get-SessionProperty $session 'ALLUSERS'
    $perUser = Get-SessionProperty $session 'MSIINSTALLPERUSER'
    if (($choice -eq 'machine' -and ($allUsers -ne '1' -or $perUser)) -or
        ($choice -eq 'user' -and ($allUsers -ne '2' -or $perUser -ne '1'))) {
      throw 'Scope selector does not reset the requested installation context.'
    }
  }
  $actions = @(Read-Rows $database 'SELECT `Action`, `Source`, `Target`, `Type` FROM `CustomAction`' 4)
  $savedOptions = @($actions | Where-Object { $_[0] -match '^CraftLoadOption[0-9]+M$' })
  foreach ($option in $savedOptions) {
    if ([int]$option[3] -ne 51) { throw 'Saved-option test may run only property-setting actions.' }
    $number = [regex]::Match($option[0], '[0-9]+').Value
    foreach ($scope in @('2','1')) {
      Set-SessionProperty $session 'ALLUSERS' $scope
      Set-SessionProperty $session "CRAFT_OPTION_${number}_M" 'saved-machine-choice'
      Set-SessionProperty $session "CRAFT_OPTION_${number}_U" 'saved-user-choice'
      Set-SessionProperty $session $option[1] ''
      $action = "CraftLoadOption$number" + $(if ($scope -eq '1') { 'M' } else { 'U' })
      if ($session.DoAction($action) -ne 1) { throw 'Saved-option property action failed.' }
      $expected = if ($scope -eq '1') { 'saved-machine-choice' } else { 'saved-user-choice' }
      if ((Get-SessionProperty $session $option[1]) -ne $expected) { throw 'Saved installer choices leaked across contexts.' }
    }
  }
  $registry = @(Read-Rows $database 'SELECT `Root`, `Key`, `Name`, `Value` FROM `Registry`' 4)
  if (@($registry | Where-Object { $_[0] -in '1', '2' }).Count) { throw 'Registration must follow the selected context, not force HKCU or HKLM.' }
  if (@($registry | Where-Object { $_[0] -eq '0' -and $_[1] -match '^Software\\Classes(\\|$)' }).Count) {
    throw 'HKCR keys must be relative to Classes.'
  }
  foreach ($entry in $policy.registry) {
    $found = @($registry | Where-Object { $_[0] -eq $entry[0] -and $_[1] -eq $entry[1] -and $_[2] -eq $entry[2] -and $_[3] -eq $entry[3] })
    if ($found.Count -ne 1) { throw "Lost or changed registration: $($entry[1]) / $($entry[2])" }
  }
  $installDir = @($registry | Where-Object { $_[0] -eq '-1' -and $_[1] -eq "Software\$($policy.name)" -and $_[2] -eq 'InstallDir' -and $_[3] -eq '[INSTALLFOLDER]' })
  if ($installDir.Count -ne 1) { throw 'A context-sensitive installation-folder record is required.' }
  Write-Output "PASS $($policy.name): $cases scope cases, folder preservation, action sequencing and registry contract. No installation performed."
} finally {
  foreach ($com in @($session, $database, $installer)) {
    if ($null -ne $com) { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($com) }
  }
}
