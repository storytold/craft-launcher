//! MSI batch elevation uses only the OS PowerShell host and Windows Installer. The GUI
//! executable is NEVER elevated. See docs/windows-msi-security.md for the trust boundary.

#[cfg(windows)]
use std::path::{Path, PathBuf};

use artcraft_launcher_core::catalog::AppKind;
use artcraft_launcher_core::{CraftApp, PackageKind, release, version};
use serde::Serialize;

#[cfg(any(windows, test))]
use crate::msi_batch::Item;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReleaseIdentity {
    pub app: String,
    pub tag: String,
    pub asset: String,
}

/// Derive the release identity from the catalog and official URL, never from a caller's hash.
pub fn release_identity(app: &CraftApp, asset: &release::Asset) -> Result<ReleaseIdentity, String> {
    if app.kind != AppKind::Official || !app.repo.starts_with("storytold/") {
        return Err("only official ArtCraft releases may use the batched elevated MSI path".into());
    }
    let prefix = release::download_prefix(app.repo);
    if !release::download_url_under(&asset.url, &prefix) {
        return Err("the MSI must come from the app's official GitHub release".into());
    }
    let rest = asset.url.strip_prefix(&prefix).ok_or("invalid release URL")?;
    let (tag, name) = rest.split_once('/').ok_or("invalid release URL")?;
    let stems: Vec<_> = app.stems().collect();
    let parsed = artcraft_launcher_core::platform::parse_asset_name(name, &stems).ok_or("invalid MSI release name")?;
    if name != asset.name
        || !tag.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
        || version::parse(tag) != version::parse(&parsed.version)
        || parsed.kind != PackageKind::Msi
        || !matches!(parsed.token.as_str(), "windows-x64" | "windows-x86" | "windows-arm64")
    {
        return Err("the MSI does not match the official Windows release".into());
    }
    Ok(ReleaseIdentity { app: app.id.into(), tag: tag.into(), asset: name.into() })
}

pub fn msi_outcome(code: Option<i32>) -> Result<(), String> {
    match code {
        Some(0 | 3010 | 1641) => Ok(()),
        Some(1602 | 1223) => Err("cancelled".into()),
        Some(1618) => Err("another installation is in progress. Wait for it to finish, then try again.".into()),
        Some(1925 | 1303) => Err("Windows didn't allow the install (administrator permission is needed)".into()),
        Some(c) => Err(format!("the Windows Installer stopped with error {c}")),
        None => Err("the Windows Installer was interrupted".into()),
    }
}

fn result_outcome(value: &str) -> Result<(), String> {
    match value {
        "rejected" => Err("Windows refused an MSI that failed official release, signature, identity, scope, or file integrity checks".into()),
        "running" => Err("the app is running. Quit it, then try again.".into()),
        value => msi_outcome(value.parse().ok()),
    }
}

/// Ignore torn lines and duplicate/out-of-range indices. The transport is in an
/// administrator-owned directory; parsing alone is not an authentication mechanism.
pub fn parse_results(text: &str, count: usize) -> Vec<(usize, Result<(), String>)> {
    let mut seen = vec![false; count];
    text.split_inclusive('\n')
        .filter_map(|line| {
            let (index, value) = line.strip_suffix('\n')?.trim().split_once(' ')?;
            let index = index.parse::<usize>().ok()?;
            let seen = seen.get_mut(index).filter(|seen| !**seen)?;
            *seen = true;
            Some((index, result_outcome(value)))
        })
        .collect()
}

/// The script is fixed program code embedded in Rust, not a downloaded/file-based script.
/// All request data is base64 JSON. Only Windows system executables execute with elevation.
#[cfg(any(windows, test))]
pub const POLICY: &str = r#"
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2
$system = [Environment]::SystemDirectory
[Environment]::CurrentDirectory = $system
Set-Location -LiteralPath $system
$env:PSModulePath = [IO.Path]::Combine($PSHOME, 'Modules')
Import-Module ([IO.Path]::Combine($PSHOME, 'Modules\Microsoft.PowerShell.Security\Microsoft.PowerShell.Security.psd1'))
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$script:ArtCraftValidationStage = 'startup'

function Set-ArtCraftValidationStage([string]$stage) {
    $script:ArtCraftValidationStage = $stage
}

function Get-ArtCraftRootException($exception) {
    while ($null -ne $exception.InnerException) { $exception = $exception.InnerException }
    return $exception
}

function Format-ArtCraftHResult($exception) {
    return ('0x{0:X8}' -f ($exception.HResult -band 0xffffffffL))
}

function Write-ArtCraftDiagnostic($writer, [string]$kind, $index, $details) {
    if ($null -eq $writer) { return }
    try {
        $entry = [ordered]@{
            timeUtc = [DateTime]::UtcNow.ToString('o')
            kind = $kind
            item = $index
            stage = $script:ArtCraftValidationStage
            details = $details
        }
        $writer.WriteLine(($entry | ConvertTo-Json -Compress -Depth 8))
        $writer.Flush()
    } catch {
        # Diagnostics are best-effort and must not change the fail-closed install decision.
    }
}

function Write-ArtCraftFailure($writer, [int]$index, $record) {
    $outer = $record.Exception
    $root = Get-ArtCraftRootException $outer
    $reason = ([string]$root.Message) -replace '[\r\n\t]+', ' '
    if ($reason.Length -gt 2048) { $reason = $reason.Substring(0, 2048) }
    Write-ArtCraftDiagnostic $writer 'validation-failure' $index @{
        exceptionType = $outer.GetType().FullName
        hresult = Format-ArtCraftHResult $outer
        rootExceptionType = $root.GetType().FullName
        rootHresult = Format-ArtCraftHResult $root
        reason = $reason
    }
}

function Get-ArtCraftUacContext {
    $key = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System'
    $policy = Get-ItemProperty -LiteralPath $key -ErrorAction SilentlyContinue
    $parentElevated = $null
    if ($env:ARTCRAFT_BATCH_PARENT_ELEVATED -cin @('True', 'False')) {
        $parentElevated = [bool]::Parse($env:ARTCRAFT_BATCH_PARENT_ELEVATED)
    }
    $enableLUA = $null
    $adminPrompt = $null
    $userPrompt = $null
    if ($null -ne $policy) {
        $value = $policy.PSObject.Properties['EnableLUA']
        if ($null -ne $value) { $enableLUA = [int]$value.Value }
        $value = $policy.PSObject.Properties['ConsentPromptBehaviorAdmin']
        if ($null -ne $value) { $adminPrompt = [int]$value.Value }
        $value = $policy.PSObject.Properties['ConsentPromptBehaviorUser']
        if ($null -ne $value) { $userPrompt = [int]$value.Value }
    }
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    $observation = 'RunAs was requested; prompt visibility depends on the caller token and Windows policy.'
    if ($parentElevated -eq $true) {
        $observation = 'The requesting PowerShell process was already elevated, so RunAs may not display another approval.'
    } elseif ($enableLUA -eq 0) {
        $observation = 'EnableLUA is 0; User Account Control is disabled by machine policy.'
    } elseif ($adminPrompt -eq 0) {
        $observation = 'ConsentPromptBehaviorAdmin is 0; administrator elevation is configured without an interactive prompt.'
    } elseif ($adminPrompt -eq 5) {
        $observation = 'ConsentPromptBehaviorAdmin is 5; this policy prompts only for non-Windows binaries, so inspect whether Windows PowerShell was auto-approved.'
    }
    return [ordered]@{
        requestingPowerShellWasElevated = $parentElevated
        elevatedPowerShellIsAdministrator = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
        enableLUA = $enableLUA
        consentPromptBehaviorAdmin = $adminPrompt
        consentPromptBehaviorUser = $userPrompt
        observation = $observation
    }
}

function Assert-LocalPath([string]$path) {
    # No UNC/device paths, alternate streams, quotes, control characters or relative paths.
    if ($path -notmatch '^[A-Za-z]:\\' -or $path.Substring(2) -match '[:"\x00-\x1f]' -or $path.Length -gt 240) {
        throw 'Invalid local path'
    }
    $full = [IO.Path]::GetFullPath($path)
    $cursor = $full
    while ($cursor) {
        if ([IO.File]::Exists($cursor) -or [IO.Directory]::Exists($cursor)) {
            if (([IO.File]::GetAttributes($cursor) -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw 'Reparse points are not allowed'
            }
        }
        $cursor = [IO.Path]::GetDirectoryName($cursor)
    }
    return $full
}

function Assert-InstallDestination([string]$path, $app) {
    $full = Assert-LocalPath $path
    $leaf = [IO.Path]::GetFileName($full.TrimEnd('\'))
    if ($leaf -notin $app.names) { throw 'Install destination does not match the app' }
    $windows = [IO.Path]::GetFullPath([Environment]::GetFolderPath('Windows')).TrimEnd('\')
    if ($full.StartsWith($windows + '\', [StringComparison]::OrdinalIgnoreCase) -or
        $full.Equals($windows, [StringComparison]::OrdinalIgnoreCase)) { throw 'Windows folders are not install destinations' }
    $cursor = $full
    $nearest = $true
    $trusted = @('S-1-5-18', 'S-1-5-32-544',
        'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464')
    while ($cursor) {
        if ([IO.Directory]::Exists($cursor)) {
            $acl = Microsoft.PowerShell.Security\Get-Acl -LiteralPath $cursor
            if ($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -notin $trusted) {
                throw 'Install destination has an untrusted owner'
            }
            foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
                if (($rule.PropagationFlags -band [Security.AccessControl.PropagationFlags]::InheritOnly) -ne 0) { continue }
                if ($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -notin $trusted) {
                    # Ancestors must not permit deletion/replacement or ACL/owner changes.
                    # The closest existing directory must also forbid creating/replacing files.
                    $mask = 0x000d0040
                    if ($nearest) { $mask = 0x000d0156 }
                    if (([int]$rule.FileSystemRights -band $mask) -ne 0) { throw 'Install destination is user-writable' }
                }
            }
            $nearest = $false
        } elseif ([IO.File]::Exists($cursor)) { throw 'Install destination is a file' }
        $cursor = [IO.Path]::GetDirectoryName($cursor)
    }
    return $full
}

function New-ProtectedRun([string]$session) {
    if ($session -cnotmatch '^[a-f0-9]{32}$') { throw 'Invalid session' }
    # SystemDirectory is an OS API result, not SystemRoot/PATH/LOCALAPPDATA from the caller.
    $root = [IO.Path]::Combine([Environment]::SystemDirectory, "ArtCraft-MSI-$session")
    $null = Assert-LocalPath $root
    if ([IO.Directory]::Exists($root) -or [IO.File]::Exists($root)) { throw 'Session already exists' }
    $acl = New-Object Security.AccessControl.DirectorySecurity
    # SYSTEM/Administrators own all writes. Users have read/execute only for progress/results.
    $acl.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)')
    $null = [IO.Directory]::CreateDirectory($root, $acl)
    return $root
}

function New-ProtectedFile([string]$path, [bool]$readable = $false) {
    # Ownership does not inherit from a directory. Set it atomically at creation, rather than
    # relying on the elevated account's default owner (which could be its unelevated user SID).
    $acl = New-Object Security.AccessControl.FileSecurity
    $sddl = 'O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)'
    if ($readable) { $sddl += '(A;;0x120089;;;BU)' }
    $acl.SetSecurityDescriptorSddlForm($sddl)
    return [IO.FileStream]::new($path, [IO.FileMode]::CreateNew,
        [Security.AccessControl.FileSystemRights]::Write, [IO.FileShare]::Read,
        4096, [IO.FileOptions]::None, $acl)
}

function Read-OfficialChecksums([string]$url) {
    for ($redirect = 0; $redirect -lt 6; $redirect++) {
        $uri = [Uri]$url
        if ($uri.Scheme -cne 'https' -or -not $uri.IsDefaultPort -or $uri.UserInfo -ne '' -or
            $uri.Host -notin @('github.com', 'release-assets.githubusercontent.com', 'objects.githubusercontent.com')) {
            throw 'Untrusted checksum redirect'
        }
        $req = [Net.HttpWebRequest]::Create($uri)
        $req.AllowAutoRedirect = $false
        $req.Timeout = 20000
        $req.ReadWriteTimeout = 20000
        $req.UserAgent = 'ArtCraft-Launcher/MSI'
        $response = $req.GetResponse()
        try {
            if ([int]$response.StatusCode -ge 300 -and [int]$response.StatusCode -lt 400) {
                $url = [Uri]::new($uri, $response.Headers['Location']).AbsoluteUri
                continue
            }
            if ([int]$response.StatusCode -ne 200) { throw 'No official checksums' }
            $stream = $response.GetResponseStream()
            $memory = New-Object IO.MemoryStream
            try {
                $buffer = New-Object byte[] 8192
                while (($n = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) {
                    if ($memory.Length + $n -gt 4194304) { throw 'Oversized checksums' }
                    $memory.Write($buffer, 0, $n)
                }
                return [Text.Encoding]::UTF8.GetString($memory.ToArray())
            } finally { $memory.Dispose(); $stream.Dispose() }
        } finally { $response.Dispose() }
    }
    throw 'Too many checksum redirects'
}

function Get-PublishedHash([string]$text, [string]$asset) {
    $found = @()
    foreach ($line in ($text -split "\n")) {
        if ($line -match '^([0-9a-fA-F]{64})\s+\*?([^\r\n]+)\r?$' -and $Matches[2].TrimEnd("`r") -ceq $asset) {
            $found += $Matches[1].ToLowerInvariant()
        }
    }
    if ($found.Count -ne 1) { throw 'Missing or ambiguous published checksum' }
    return $found[0]
}

function New-MsiApi {
    # Call the absolute system MSI library directly. COM activation is deliberately avoided:
    # caller-owned HKCU COM registrations must never influence an elevated DLL load.
    # Reflection.Emit supplies managed P/Invoke declarations, without C# compilation or
    # writing/loading an assembly in a user-owned temporary directory.
    $assembly = [AppDomain]::CurrentDomain.DefineDynamicAssembly(
        [Reflection.AssemblyName]::new('ArtCraftMsi' + [Guid]::NewGuid().ToString('N')), [Reflection.Emit.AssemblyBuilderAccess]::Run)
    $module = $assembly.DefineDynamicModule('ArtCraftMsi')
    $type = $module.DefineType('ArtCraftMsi', [Reflection.TypeAttributes]'Public, Abstract, Sealed')
    $dll = [IO.Path]::Combine([Environment]::SystemDirectory, 'msi.dll')
    $u = [uint32]
    $ref = $u.MakeByRefType()
    $i = [int32]
    $iref = $i.MakeByRefType()
    $definitions = @(
        @('MsiOpenDatabaseW', @([string], [IntPtr], $ref)),
        @('MsiDatabaseOpenViewW', @($u, [string], $ref)),
        @('MsiViewExecute', @($u, $u)),
        @('MsiViewFetch', @($u, $ref)),
        @('MsiRecordGetStringW', @($u, $u, [Text.StringBuilder], $ref)),
        @('MsiGetSummaryInformationW', @($u, [string], $u, $ref)),
        # FILETIME* is required even for string properties such as PID_TEMPLATE.
        @('MsiSummaryInfoGetPropertyW', @($u, $u, $ref, $iref, [IntPtr], [Text.StringBuilder], $ref)),
        @('MsiViewClose', @($u)),
        @('MsiCloseHandle', @($u)))
    foreach ($definition in $definitions) {
        $name = [string]$definition[0]
        # Use the overload with a separately named export to keep the native entry point explicit.
        $method = $type.DefinePInvokeMethod($name, $dll, $name,
            [Reflection.MethodAttributes]'Public, Static, PinvokeImpl',
            [Reflection.CallingConventions]::Standard, $u, [Type[]]$definition[1],
            [Runtime.InteropServices.CallingConvention]::Winapi,
            [Runtime.InteropServices.CharSet]::Unicode)
        $method.SetImplementationFlags([Reflection.MethodImplAttributes]::PreserveSig)
    }
    return $type.CreateType()
}

function Invoke-MsiApi($api, [string]$name, [object[]]$arguments) {
    $method = $api.GetMethod($name)
    if ($null -eq $method) { throw "MSI API method $name was not declared" }
    # Keep the caller's exact array instance so ref/out values can be copied back into it.
    $source = $arguments
    if ($source -isnot [Array]) { throw "MSI API $name arguments were not a CLR array" }
    $parameters = $method.GetParameters()
    if ($source.Length -ne $parameters.Length) {
        throw "MSI API $name expected $($parameters.Length) arguments but received $($source.Length)"
    }

    # PowerShell can retain PSObject adapters inside object[] arrays. Reflection.Emit P/Invoke
    # stubs require the actual CLR values, so unwrap each item and check it against the native
    # declaration before invoking. Reflection writes ref/out values back into callArguments.
    $callArguments = [Array]::CreateInstance([object], $parameters.Length)
    for ($index = 0; $index -lt $parameters.Length; $index++) {
        $value = $source.GetValue($index)
        if ($null -ne $value) { $value = [System.Management.Automation.PSObject]::Base($value) }
        while ($value -is [System.Management.Automation.PSObject]) {
            $baseValue = [System.Management.Automation.PSObject]::Base($value)
            if ([object]::ReferenceEquals($value, $baseValue)) { break }
            $value = $baseValue
        }
        $expectedType = $parameters[$index].ParameterType
        if ($expectedType.IsByRef) { $expectedType = $expectedType.GetElementType() }
        if (($null -eq $value -and $expectedType.IsValueType) -or
            ($null -ne $value -and -not $expectedType.IsInstanceOfType($value))) {
            $actualType = if ($null -eq $value) { '<null>' } else { $value.GetType().FullName }
            throw "MSI API $name argument $index expected CLR $($expectedType.FullName), received $actualType"
        }
        $callArguments.SetValue([object]$value, $index)
    }

    try {
        $result = $method.Invoke($null, $callArguments)
    } catch {
        $failure = $_.Exception
        while ($null -ne $failure.InnerException) { $failure = $failure.InnerException }
        throw "MSI API $name reflection invocation failed: $($failure.GetType().FullName): $($failure.Message)"
    }

    # Preserve native by-ref outputs in the caller's original array for handle/status use.
    for ($index = 0; $index -lt $parameters.Length; $index++) {
        $value = $callArguments.GetValue($index)
        if ($null -ne $value) { $value = [System.Management.Automation.PSObject]::Base($value) }
        while ($value -is [System.Management.Automation.PSObject]) {
            $baseValue = [System.Management.Automation.PSObject]::Base($value)
            if ([object]::ReferenceEquals($value, $baseValue)) { break }
            $value = $baseValue
        }
        $source.SetValue($value, $index)
    }
    $result = [System.Management.Automation.PSObject]::Base($result)
    return $result
}

function Get-MsiTemplate($api, [uint32]$db) {
    # PID_TEMPLATE (7) records the actual package platform independently of its filename.
    $arguments = [object[]]@($db, [string]$null, [uint32]0, [uint32]0)
    Set-ArtCraftValidationStage 'msi-summary-open'
    $status = Invoke-MsiApi $api 'MsiGetSummaryInformationW' $arguments
    if ($status -ne 0) { throw "MsiGetSummaryInformationW failed with Windows Installer status $status" }
    $summary = [uint32]$arguments[3]
    try {
        $template = [Text.StringBuilder]::new(256)
        $arguments = [object[]]@($summary, [uint32]7, [uint32]0, [int32]0, [IntPtr]::Zero, $template, [uint32]256)
        Set-ArtCraftValidationStage 'msi-summary-template'
        $status = Invoke-MsiApi $api 'MsiSummaryInfoGetPropertyW' $arguments
        if ($status -ne 0) { throw "MsiSummaryInfoGetPropertyW failed with Windows Installer status $status" }
        return $template.ToString().Split(';')[0]
    } finally { $null = Invoke-MsiApi $api 'MsiCloseHandle' ([object[]]@($summary)) }
}

function Get-MsiProperty($api, [uint32]$db, [string]$name) {
    # name is a program literal. MSI APIs open the protected staged file READ-ONLY.
    Set-ArtCraftValidationStage "msi-property-open:$name"
    $arguments = [object[]]@($db, "SELECT Value FROM Property WHERE Property='$name'", [uint32]0)
    $status = Invoke-MsiApi $api 'MsiDatabaseOpenViewW' $arguments
    if ($status -ne 0) { throw "MsiDatabaseOpenViewW failed for property $name with Windows Installer status $status" }
    $view = [uint32]$arguments[2]
    try {
        Set-ArtCraftValidationStage "msi-property-execute:$name"
        $status = Invoke-MsiApi $api 'MsiViewExecute' ([object[]]@($view, [uint32]0))
        if ($status -ne 0) { throw "MsiViewExecute failed for property $name with Windows Installer status $status" }
        $arguments = [object[]]@($view, [uint32]0)
        Set-ArtCraftValidationStage "msi-property-fetch:$name"
        $status = Invoke-MsiApi $api 'MsiViewFetch' $arguments
        if ($status -eq 259) { return '' }
        if ($status -ne 0) { throw "MsiViewFetch failed for property $name with Windows Installer status $status" }
        $record = [uint32]$arguments[1]
        try {
            $value = [Text.StringBuilder]::new(1024)
            $arguments = [object[]]@($record, [uint32]1, $value, [uint32]1024)
            Set-ArtCraftValidationStage "msi-property-read:$name"
            $status = Invoke-MsiApi $api 'MsiRecordGetStringW' $arguments
            if ($status -ne 0) { throw "MsiRecordGetStringW failed for property $name with Windows Installer status $status" }
            return $value.ToString()
        } finally { $null = Invoke-MsiApi $api 'MsiCloseHandle' ([object[]]@($record)) }
    } finally {
        $null = Invoke-MsiApi $api 'MsiViewClose' ([object[]]@($view))
        $null = Invoke-MsiApi $api 'MsiCloseHandle' ([object[]]@($view))
    }
}

function Assert-MsiIdentity([string]$file, $app, [string]$version, [string]$asset) {
    Set-ArtCraftValidationStage 'msi-api-bindings'
    $api = New-MsiApi
    # Invoke-MsiApi unwraps and validates every value before Reflection.Invoke.
    $arguments = [object[]]@([string]$file, [IntPtr]::Zero, [uint32]0)
    Set-ArtCraftValidationStage 'msi-database-open'
    $status = Invoke-MsiApi $api 'MsiOpenDatabaseW' $arguments
    if ($status -ne 0) { throw "MsiOpenDatabaseW failed with Windows Installer status $status" }
    $db = [uint32]$arguments[2]
    try {
        $packageArch = switch -Regex ($asset) {
            '-windows-x64\.msi$' { 'x64'; break }
            '-windows-x86\.msi$' { 'Intel'; break }
            '-windows-arm64\.msi$' { 'Arm64'; break }
            default { throw 'MSI architecture is not authorized' }
        }
        Set-ArtCraftValidationStage 'msi-template'
        $template = Get-MsiTemplate $api $db
        if ($template -cne $packageArch) { throw 'MSI platform does not match the release asset architecture' }
        Set-ArtCraftValidationStage 'msi-property:ALLUSERS'
        if ((Get-MsiProperty $api $db 'ALLUSERS') -cne '1') { throw 'MSI must declare ALLUSERS=1' }
        Set-ArtCraftValidationStage 'msi-property:MSIINSTALLPERUSER'
        if ((Get-MsiProperty $api $db 'MSIINSTALLPERUSER') -notin @('', '0')) { throw 'MSI declares per-user installation scope' }
        Set-ArtCraftValidationStage 'msi-property:Manufacturer'
        if ((Get-MsiProperty $api $db 'Manufacturer') -cne 'Learning Machines LLC') { throw 'MSI Manufacturer is not the approved ArtCraft value' }
        Set-ArtCraftValidationStage 'msi-property:ProductName'
        if ((Get-MsiProperty $api $db 'ProductName') -cnotin $app.names) { throw 'MSI ProductName does not match the authorized app' }
        Set-ArtCraftValidationStage 'msi-property:ProductVersion'
        if ((Get-MsiProperty $api $db 'ProductVersion') -cne $version) { throw 'MSI ProductVersion does not match the authorized release' }
    } finally { $null = Invoke-MsiApi $api 'MsiCloseHandle' ([object[]]@($db)) }
}

function Assert-AuthenticodePublisher([string]$publisher) {
    # Signing identity is independent of the authored MSI Manufacturer (Learning Machines LLC).
    # Exact identity, not an alias/prefix/wildcard. Windows validates the certificate chain first.
    if ($publisher -cne 'Learning Machines Inc') { throw "Authenticode signer '$publisher' is not the approved publisher" }
}

function Assert-OfficialSignature([string]$file) {
    try { $sig = Microsoft.PowerShell.Security\Get-AuthenticodeSignature -LiteralPath $file }
    catch { throw "Authenticode inspection failed: $($_.Exception.Message)" }
    if ($sig.Status -ne 'Valid' -or $null -eq $sig.SignerCertificate) {
        throw "Authenticode status is $($sig.Status); a valid Windows certificate chain is required"
    }
    $certificate = $sig.SignerCertificate
    $simpleName = $certificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false)
    $organization = $certificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::OrganizationName, $false)
    Assert-AuthenticodePublisher $simpleName
    if ($organization -cne 'Learning Machines Inc') { throw "Authenticode signer organization '$organization' is not the approved publisher" }
}

function Get-ReleaseAuthorization($item, $catalog) {
    Set-ArtCraftValidationStage 'release-catalog-identity'
    $app = @($catalog | Where-Object { $_.id -ceq $item.release.app })
    if ($app.Count -ne 1) { throw 'Unknown app' }
    $app = $app[0]
    $tag = [string]$item.release.tag
    $asset = [string]$item.release.asset
    if ($tag.Length -gt 64 -or $tag -cnotmatch '^v?([0-9]+\.[0-9]+\.[0-9]+)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$') {
        throw 'Invalid official release tag'
    }
    $numeric = $Matches[1]
    $releaseVersion = $tag -creplace '^v', ''
    $valid = $false
    foreach ($stem in $app.stems) {
        foreach ($arch in @('x64', 'x86', 'arm64')) {
            if ($asset -ceq "$stem-$releaseVersion-windows-$arch.msi") { $valid = $true }
        }
    }
    if (-not $valid) { throw 'MSI does not match its app and version' }
    # The catalog is compiled into the policy. No caller-supplied repository, URL or digest is
    # authoritative. Fetch SHA256SUMS.txt independently after elevation over Windows TLS.
    $url = "https://github.com/$($app.repo)/releases/download/$tag/SHA256SUMS.txt"
    Set-ArtCraftValidationStage 'published-checksum-download'
    $checksumFile = Read-OfficialChecksums $url
    Set-ArtCraftValidationStage 'published-checksum-entry'
    $sha = Get-PublishedHash $checksumFile $asset
    Set-ArtCraftValidationStage 'published-checksum-match'
    if ($item.sha256 -cne $sha) { throw 'Caller checksum disagrees with official publication' }
    return @{ app = $app; sha = $sha; version = $numeric; asset = $asset }
}

function Copy-VerifiedPackage($item, $authorization, [string]$root, [int]$index, [int]$port, [string]$session) {
    $target = [IO.Path]::Combine($root, "$index.msi")
    Set-ArtCraftValidationStage 'loopback-package-stream'
    # Never open a caller-owned path as administrator: even an initially local path could
    # race through a junction to an SMB share and leak elevated network credentials.
    # A loopback-only byte source runs unelevated. Its bytes remain entirely untrusted.
    $req=[Net.HttpWebRequest]::Create("http://127.0.0.1:$port/$session/$index")
    $req.Proxy=$null
    $req.Credentials=$null
    $req.UserAgent='ArtCraft-Launcher/MSI'
    $req.AllowAutoRedirect=$false
    $req.Timeout=20000
    $req.ReadWriteTimeout=20000
    $response=$req.GetResponse()
    try {
        if ([int]$response.StatusCode -ne 200 -or $response.ContentLength -lt 0 -or $response.ContentLength -gt 17179869184) {
            throw 'Invalid package stream'
        }
        $inputFile=$response.GetResponseStream()
        $output = New-ProtectedFile $target
        try {
            $remaining=$response.ContentLength
            $buffer=New-Object byte[] 65536
            while ($remaining -gt 0) {
                $n=$inputFile.Read($buffer,0,[int][Math]::Min($remaining,$buffer.Length))
                if ($n -eq 0) { throw 'Truncated package stream' }
                $output.Write($buffer,0,$n)
                $remaining-=$n
            }
        } finally { $output.Dispose(); $inputFile.Dispose() }
    } finally { $response.Dispose() }
    $locked = [IO.File]::Open($target, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        Set-ArtCraftValidationStage 'staged-package-hash'
        $hash = [Security.Cryptography.SHA256]::Create()
        try { $actual = ([BitConverter]::ToString($hash.ComputeHash($locked))).Replace('-', '').ToLowerInvariant() }
        finally { $hash.Dispose() }
        if ($actual -cne $authorization.sha) { throw 'Changed package' }
        Set-ArtCraftValidationStage 'authenticode-chain-and-publisher'
        Assert-OfficialSignature $target
        Set-ArtCraftValidationStage 'msi-identity-and-scope'
        Assert-MsiIdentity ([string]$target) $authorization.app $authorization.version $authorization.asset
    } finally { $locked.Dispose() }
    return $target
}

function Test-AppRunning($app) {
    foreach ($process in [Diagnostics.Process]::GetProcesses()) {
        try {
            if ($process.ProcessName -in $app.stems) { return $true }
        } finally { $process.Dispose() }
    }
    return $false
}

function Invoke-ArtCraftBatch($request, $catalog) {
    Set-ArtCraftValidationStage 'batch-request'
    if ($request.items.Count -lt 1 -or $request.items.Count -gt 64) { throw 'Invalid batch size' }
    if ($request.port -lt 1 -or $request.port -gt 65535) { throw 'Invalid local source port' }
    $root = New-ProtectedRun ([string]$request.session)
    $diagnosticsHandle = New-ProtectedFile ([IO.Path]::Combine($root, 'diagnostics.jsonl'))
    $diagnosticsWriter = [IO.StreamWriter]::new($diagnosticsHandle, [Text.UTF8Encoding]::new($false))
    $script:ArtCraftDiagnosticWriter = $diagnosticsWriter
    $results = $null
    $writer = $null
    try {
        Set-ArtCraftValidationStage 'uac-context'
        $uacContext = Get-ArtCraftUacContext
        Write-ArtCraftDiagnostic $diagnosticsWriter 'batch-context' $null $uacContext
        $results = New-ProtectedFile ([IO.Path]::Combine($root, 'results')) $true
        $writer = [IO.StreamWriter]::new($results, [Text.UTF8Encoding]::new($false))
        $writer.AutoFlush = $true
        for ($i = 0; $i -lt $request.items.Count; $i++) {
            $item = $request.items[$i]
            $code = 'rejected'
            $staged = [IO.Path]::Combine($root, "$i.msi")
            try {
                Set-ArtCraftValidationStage 'release-authorization'
                $authorization = Get-ReleaseAuthorization $item $catalog
                $staged = Copy-VerifiedPackage $item $authorization $root $i $request.port $request.session
                # Check immediately before each installation, including apps started during UAC.
                Set-ArtCraftValidationStage 'running-application-check'
                if (Test-AppRunning $authorization.app) { $code = 'running' }
                else {
                    Set-ArtCraftValidationStage 'installer-file-lock'
                    $locked = [IO.File]::Open($staged, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
                    try {
                        $start = New-Object Diagnostics.ProcessStartInfo
                        $start.FileName = [IO.Path]::Combine([Environment]::SystemDirectory, 'msiexec.exe')
                        $start.WorkingDirectory = [Environment]::SystemDirectory
                        $start.UseShellExecute = $false
                        # Logs live with the protected staged package; no user-writable log paths.
                        $log = [IO.Path]::Combine($root, "$i.log")
                        $logHandle = New-ProtectedFile $log
                        $logHandle.Dispose()
                        # Keep Windows Installer's cancel button available. The elevated parent
                        # remains in the same single UAC session for each sequential MSI.
                        $start.Arguments = '/i "' + $staged + '" /passive /norestart ALLUSERS=1 /l*v "' + $log + '"'
                        if ($null -ne $item.destination) {
                            Set-ArtCraftValidationStage 'install-destination-acl'
                            $destination = Assert-InstallDestination ([string]$item.destination) $authorization.app
                            # Always pass a trailing separator, doubled before the closing quote
                            # for Windows argument decoding (including a drive-root folder).
                            $start.Arguments += ' INSTALLFOLDER="' + $destination.TrimEnd('\') + '\\"'
                        }
                        Set-ArtCraftValidationStage 'windows-installer-launch'
                        $child = [Diagnostics.Process]::Start($start)
                        try { $child.WaitForExit(); $code = [string]$child.ExitCode }
                        finally { $child.Dispose() }
                        Write-ArtCraftDiagnostic $diagnosticsWriter 'installer-exit' $i @{ exitCode = $code; log = "$i.log" }
                    } finally { $locked.Dispose() }
                }
            } catch {
                # Keep the launcher message fixed and sanitized; full failure data stays in the
                # administrator-owned diagnostics file, never in the user-writable UI channel.
                Write-ArtCraftFailure $diagnosticsWriter $i $_
                $code = 'rejected'
            } finally {
                [IO.File]::Delete($staged)
            }
            $writer.WriteLine("$i $code")
        }
    } catch {
        Write-ArtCraftFailure $diagnosticsWriter -1 $_
        throw
    } finally {
        if ($null -ne $writer) { $writer.Dispose() }
        if ($null -ne $results) { $results.Dispose() }
        $diagnosticsWriter.Dispose()
        $script:ArtCraftDiagnosticWriter = $null
    }
}
"#;

#[cfg(any(windows, test))]
fn encode_command(script: &str) -> String {
    use base64::Engine as _;
    let bytes: Vec<_> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(any(windows, test))]
fn compact_policy(script: &str) -> String {
    script.lines().filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#')).collect::<Vec<_>>().join("\n")
}

#[cfg(any(windows, test))]
fn compressed_command(script: &str) -> Result<String, String> {
    use base64::Engine as _;
    use std::io::Write as _;
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    // Source comments/blank lines do not affect the fixed PowerShell policy and waste space
    // in the bounded ShellExecute command line.
    gzip.write_all(compact_policy(script).as_bytes()).map_err(|e| e.to_string())?;
    let data = base64::engine::general_purpose::STANDARD.encode(gzip.finish().map_err(|e| e.to_string())?);
    Ok(encode_command(&format!(
        "$m=[IO.MemoryStream]::new([Convert]::FromBase64String('{data}')); \
         $g=[IO.Compression.GZipStream]::new($m,[IO.Compression.CompressionMode]::Decompress); \
         $r=[IO.StreamReader]::new($g); & ([ScriptBlock]::Create($r.ReadToEnd()))"
    )))
}

#[cfg(any(windows, test))]
fn batch_script(session: &str, port: u16, items: &[Item]) -> Result<String, String> {
    use base64::Engine as _;
    #[derive(Serialize)]
    struct CatalogApp {
        id: &'static str,
        repo: &'static str,
        names: Vec<&'static str>,
        stems: Vec<&'static str>,
    }
    let catalog: Vec<_> = artcraft_launcher_core::catalog::SUITE
        .iter()
        .filter(|app| app.kind == AppKind::Official && app.repo.starts_with("storytold/"))
        .map(|app| CatalogApp { id: app.id, repo: app.repo, names: app.names().collect(), stems: app.stems().collect() })
        .collect();
    // Filesystem source paths never cross the privilege boundary.
    let items: Vec<_> = items
        .iter()
        .map(|item| {
            serde_json::json!({
                "release": item.release, "sha256": item.sha256, "destination": item.destination
            })
        })
        .collect();
    let request = serde_json::json!({ "session": session, "port": port, "items": items });
    let json = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    let catalog = serde_json::to_vec(&catalog).map_err(|e| e.to_string())?;
    let data = base64::engine::general_purpose::STANDARD.encode(json);
    let catalog = base64::engine::general_purpose::STANDARD.encode(catalog);
    Ok(format!(
        "{POLICY}\ntry {{\n$request = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{data}')) | ConvertFrom-Json\n\
         $catalog = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{catalog}')) | ConvertFrom-Json\n\
         Invoke-ArtCraftBatch $request $catalog\nexit 0\n}} catch {{ exit 1 }}\n"
    ))
}

#[cfg(windows)]
pub fn system_powershell() -> Result<PathBuf, String> {
    system_directory().map(|dir| dir.join(r"WindowsPowerShell\v1.0\powershell.exe"))
}

#[cfg(windows)]
fn system_directory() -> Result<PathBuf, String> {
    winsafe::GetSystemDirectory().map(PathBuf::from).map_err(|e| format!("couldn't locate Windows' system directory ({e})"))
}

#[cfg(windows)]
pub fn install(items: &[Item], report: &mut dyn FnMut(usize, crate::msi_batch::Report)) {
    use crate::msi_batch::Report;
    use artcraft_launcher_core::protocol::JobPhase;

    let fail = |error: String, report: &mut dyn FnMut(usize, Report)| {
        for i in 0..items.len() {
            report(i, Report::Done(Err(error.clone())));
        }
    };
    let prepared = (|| {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|e| format!("couldn't create an installer session ({e})"))?;
        let session = crate::http::hex(&random);
        let source = crate::msi_source::Source::new(items, &session)?;
        let system = system_directory()?;
        let powershell = system_powershell()?;
        let script = compressed_command(&batch_script(&session, source.port, items)?)?;
        // The unelevated launcher asks the OS to elevate ONLY its system PowerShell executable.
        // Both paths/code are data, never interpolation of MSI/path strings into shell source.
        let host = powershell.to_str().ok_or("unusual Windows system path")?.replace('\'', "''");
        let cwd = system.to_str().ok_or("unusual Windows system path")?.replace('\'', "''");
        let modules = powershell.parent().ok_or("unusual Windows system path")?.join("Modules");
        let management = modules.join(r"Microsoft.PowerShell.Management\Microsoft.PowerShell.Management.psd1");
        let management = management.to_str().ok_or("unusual Windows system path")?.replace('\'', "''");
        let outer = format!(
            "$ErrorActionPreference='Stop'; try {{ Import-Module '{management}'; \
             $parentIdentity = [Security.Principal.WindowsIdentity]::GetCurrent(); \
             $parentPrincipal = [Security.Principal.WindowsPrincipal]::new($parentIdentity); \
             $env:ARTCRAFT_BATCH_PARENT_ELEVATED = [string]$parentPrincipal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator); \
             $p = Microsoft.PowerShell.Management\\Start-Process -FilePath '{host}' -WorkingDirectory '{cwd}' \
             -ArgumentList '-NoProfile -NonInteractive -EncodedCommand {script}' -Verb RunAs -Wait -PassThru; exit $p.ExitCode \
             }} catch {{ $e=$_.Exception; while ($e) {{ if ($e.NativeErrorCode -eq 1223) {{ exit 1223 }}; $e=$e.InnerException }}; exit 1 }}"
        );
        // Avoid the Windows 32K command line limit before asking for UAC.
        if outer.len() > 30000 {
            return Err("the MSI batch command is too large for Windows".into());
        }
        let results = system.join(format!("ArtCraft-MSI-{session}")).join("results");
        let mut command = crate::process::command(powershell);
        command.env_clear().current_dir(&system);
        if let Some(root) = system.parent() {
            command.env("SystemRoot", root).env("WINDIR", root);
        }
        command.env("PATH", &system).env("PSModulePath", modules);
        command.args(["-NoProfile", "-NonInteractive", "-Command", &outer]);
        command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        Ok((command, results, source))
    })();
    let (mut command, results, _source) = match prepared {
        Ok(prepared) => prepared,
        Err(e) => return fail(e, report),
    };
    for i in 0..items.len() {
        report(i, Report::Phase(JobPhase::Permission));
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return fail(format!("couldn't ask Windows for permission ({e})"), report),
    };
    let mut done = vec![false; items.len()];
    let mut started = false;
    let exit = loop {
        let exit = child.try_wait();
        if let Ok(Some(text)) = read_results(&results) {
            if !started {
                started = true;
                for i in 0..items.len() {
                    report(i, Report::Phase(JobPhase::Installing));
                }
            }
            for (i, result) in parse_results(&text, items.len()) {
                if let Some(finished) = done.get_mut(i).filter(|finished| !**finished) {
                    *finished = true;
                    report(i, Report::Done(result));
                }
            }
        }
        match exit {
            Ok(Some(status)) => break status.code(),
            Err(_) => break None,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(150)),
        }
    };
    for (i, _) in done.iter().enumerate().filter(|(_, done)| !**done) {
        let error = if exit == Some(1223) {
            "cancelled".into()
        } else {
            "Windows didn't complete the verified MSI batch. Check the protected installer logs or try again.".into()
        };
        report(i, Report::Done(Err(error)));
    }
}

/// Results never pass through a user-owned cache path. Bound reads even if another administrator
/// tampers with the protocol. The protected system-directory parent supplies authenticity.
#[cfg(windows)]
fn read_results(path: &Path) -> Result<Option<String>, String> {
    use std::io::Read as _;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut text = String::new();
    file.take(65536).read_to_string(&mut text).map_err(|e| e.to_string())?;
    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn run_powershell_test(script: &str) -> std::process::Output {
        let script = format!("trap {{ [Console]::Error.WriteLine(($_ | Out-String)); exit 1 }}\n{script}");
        let encoded = compressed_command(&script).unwrap_or_else(|error| panic!("could not prepare compressed PowerShell test command: {error}"));
        assert!(encoded.len() < 30_000, "compressed PowerShell test command is too long for process launch: {} bytes", encoded.len());
        let powershell = system_powershell().unwrap_or_else(|error| panic!("could not locate Windows PowerShell: {error}"));
        let child = std::process::Command::new(&powershell)
            .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("failed to launch {}: {error}", powershell.display()));
        let output = child.wait_with_output().unwrap_or_else(|error| panic!("Windows PowerShell launched but collecting its output failed: {error}"));
        assert!(
            output.status.success(),
            "Windows PowerShell launched but the test script failed with {}.\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    #[cfg(windows)]
    fn assert_powershell_output_contains(output: &std::process::Output, expected: &str) {
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected),
            "Windows PowerShell launched successfully but did not report expected test result {expected:?}.\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn asset(url: &str) -> release::Asset {
        release::Asset { name: "photocraft-0.5.0-windows-x64.msi".into(), url: url.into(), ..Default::default() }
    }

    #[test]
    fn authorization_identity_is_pinned_to_app_repository_version_and_asset() {
        let app = CraftApp::by_id("photocraft").unwrap();
        let prefix = "https://github.com/storytold/photocraft/releases/download/v0.5.0/";
        let good = asset(&format!("{prefix}photocraft-0.5.0-windows-x64.msi"));
        assert_eq!(release_identity(app, &good).unwrap().tag, "v0.5.0");
        for bad in [
            good.url.replace("storytold/", "attacker/"),
            good.url.replace("/v0.5.0/", "/v0.6.0/"),
            good.url.replace("/v0.5.0/", "/../"),
            good.url.replace("/v0.5.0/", "/%2e%2e/"),
            good.url.replace("photocraft/releases", "filmcraft/releases"),
            format!("{}?x=1", good.url),
            good.url.replace("windows-x64.msi", "windows-x64.exe"),
        ] {
            assert!(release_identity(app, &asset(&bad)).is_err(), "{bad}");
        }
        let community = CraftApp::by_id("solidcraft").unwrap();
        assert!(release_identity(community, &good).is_err(), "community packages need a separately reviewed publisher policy");
    }

    #[test]
    fn fixed_policy_keeps_publisher_metadata_and_architecture_checks_independent() {
        assert!(POLICY.contains("Learning Machines Inc"), "Authenticode signer policy");
        assert!(POLICY.contains("X509NameType]::OrganizationName") && POLICY.contains("Authenticode signer organization"));
        assert!(POLICY.contains("'Manufacturer') -cne 'Learning Machines LLC'"), "MSI Manufacturer policy");
        assert!(POLICY.contains("Get-MsiTemplate") && POLICY.contains("MSI architecture is not authorized"));
        assert!(POLICY.contains("if ((Get-MsiProperty $api $db 'ALLUSERS') -cne '1')"));
        assert!(POLICY.contains("if ((Get-MsiProperty $api $db 'MSIINSTALLPERUSER') -notin @('', '0'))"));
        assert!(POLICY.contains("[object[]]@([string]$file, [IntPtr]::Zero, [uint32]0)"));
        assert!(POLICY.contains("$type.DefinePInvokeMethod($name, $dll, $name"), "native entry points are explicit");
        assert!(POLICY.contains("@($u, $u, $ref, $iref, [IntPtr], [Text.StringBuilder], $ref)"), "summary API includes FILETIME*");
        assert!(POLICY.contains("[int32]0, [IntPtr]::Zero, $template, [uint32]256"), "PID_TEMPLATE passes a null FILETIME*");
        assert!(
            POLICY.contains("function Invoke-MsiApi") && POLICY.contains("PSObject]::Base($value)"),
            "reflection arguments are explicitly unwrapped"
        );
        assert!(POLICY.contains("$source.SetValue($value, $index)"), "native ref/out values are copied back to the caller");
        assert_eq!(POLICY.matches("$method.Invoke($null, $callArguments)").count(), 1, "all MSI reflections use the typed invocation helper");
        assert!(POLICY.contains("diagnostics.jsonl") && POLICY.contains("rootHresult") && POLICY.contains("validation-failure"));
        assert!(POLICY.contains("ConsentPromptBehaviorAdmin") && POLICY.contains("requestingPowerShellWasElevated"));
        assert!(!POLICY.contains("Learning Machines LLC') -or"), "the certificate subject and MSI Manufacturer stay separate");
    }

    #[cfg(windows)]
    #[test]
    fn powershell_binds_real_msi_exports_with_the_documented_argument_counts() {
        let test = format!(
            "{POLICY}\n{}",
            r#"
$api = New-MsiApi
$expected = @{
    MsiOpenDatabaseW = @('System.String', 'System.IntPtr', 'System.UInt32&')
    MsiDatabaseOpenViewW = @('System.UInt32', 'System.String', 'System.UInt32&')
    MsiViewExecute = @('System.UInt32', 'System.UInt32')
    MsiViewFetch = @('System.UInt32', 'System.UInt32&')
    MsiRecordGetStringW = @('System.UInt32', 'System.UInt32', 'System.Text.StringBuilder', 'System.UInt32&')
    MsiGetSummaryInformationW = @('System.UInt32', 'System.String', 'System.UInt32', 'System.UInt32&')
    MsiSummaryInfoGetPropertyW = @('System.UInt32', 'System.UInt32', 'System.UInt32&', 'System.Int32&', 'System.IntPtr', 'System.Text.StringBuilder', 'System.UInt32&')
    MsiViewClose = @('System.UInt32')
    MsiCloseHandle = @('System.UInt32')
}
foreach ($entry in $expected.GetEnumerator()) {
    $method = $api.GetMethod([string]$entry.Key)
    if ($null -eq $method) { throw "Missing MSI method $($entry.Key)" }
    $parameters = $method.GetParameters()
    if ($parameters.Length -ne $entry.Value.Length) { throw "Wrong arity for $($entry.Key)" }
    for ($index = 0; $index -lt $parameters.Length; $index++) {
        $actualType = $parameters[$index].ParameterType.FullName
        if ($actualType -cne [string]$entry.Value[$index]) { throw "Wrong parameter type for $($entry.Key) at index $($index): $actualType" }
    }
    $attribute = @($method.GetCustomAttributes([Runtime.InteropServices.DllImportAttribute], $false))
    if ($attribute.Count -ne 1 -or $attribute[0].EntryPoint -cne [string]$entry.Key) {
        throw "Wrong native export for $($entry.Key)"
    }
    if ([IO.Path]::GetFileName($attribute[0].Value) -cne 'msi.dll') { throw "Wrong MSI library for $($entry.Key)" }
}
$missing = Join-Path $env:TEMP ('ArtCraft-Missing-' + [Guid]::NewGuid().ToString('N') + '.msi')
$arguments = [object[]]@($missing, [IntPtr]::Zero, [uint32]0)
if ([System.Management.Automation.PSObject]::Base($arguments[0]).GetType().FullName -cne 'System.String') { throw 'MsiOpenDatabaseW path did not unwrap to System.String' }
$status = Invoke-MsiApi $api 'MsiOpenDatabaseW' $arguments
if ([uint32]$status -ne 1619) { throw "MsiOpenDatabaseW returned unexpected status $status, expected 1619" }
Set-ArtCraftValidationStage 'test-start'
$caught = $false
try { Assert-MsiIdentity $missing @{ names = @('PhotoCraft') } '0.5.0' 'photocraft-0.5.0-windows-x64.msi' }
catch {
    if ($script:ArtCraftValidationStage -cne 'msi-database-open') { throw "Unexpected failure stage $script:ArtCraftValidationStage" }
    if ($_.Exception.Message -notmatch 'MsiOpenDatabaseW failed with Windows Installer status 1619') { throw }
    $caught = $true
}
if (-not $caught) { throw 'Assert-MsiIdentity unexpectedly accepted a missing MSI' }
"MSI exports and signatures passed; missing-package status=$status"
"#
        );
        let output = run_powershell_test(&test);
        assert_powershell_output_contains(&output, "MSI exports and signatures passed");
    }

    #[cfg(windows)]
    #[test]
    fn powershell_diagnostics_record_stage_exception_hresult_and_actionable_reason() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("ArtCraft-diagnostics-{}-{nonce}.jsonl", std::process::id()));
        let path = path.to_string_lossy().replace('\'', "''");
        let test = format!(
            "{POLICY}\n{}",
            r#"
$path = 'PATH_TOKEN'
$writer = [IO.StreamWriter]::new($path, $false, [Text.UTF8Encoding]::new($false))
try {
    Set-ArtCraftValidationStage 'test-msi-database-open'
    $record = [PSCustomObject]@{ Exception = [IO.FileNotFoundException]::new('MSI database could not be opened') }
    Write-ArtCraftFailure $writer 4 $record
} finally { $writer.Dispose() }
$entry = Get-Content -LiteralPath $path | ConvertFrom-Json
if ($entry.kind -cne 'validation-failure' -or $entry.item -ne 4) { throw 'Wrong diagnostic kind/item' }
if ($entry.stage -cne 'test-msi-database-open') { throw 'Wrong validation stage' }
if ($entry.details.rootExceptionType -cne 'System.IO.FileNotFoundException') { throw 'Wrong root exception type' }
if ($entry.details.rootHresult -cne '0x80070002') { throw "Wrong root HRESULT $($entry.details.rootHresult)" }
if ($entry.details.reason -cne 'MSI database could not be opened') { throw 'Wrong actionable reason' }
Remove-Item -LiteralPath $path -Force
'Protected diagnostic record shape passed'
"#
            .replace("PATH_TOKEN", &path)
        );
        let output = run_powershell_test(&test);
        assert_powershell_output_contains(&output, "Protected diagnostic record shape passed");
    }

    /// Opt-in real-release check for a disposable Windows test environment; it never installs.
    #[cfg(windows)]
    #[test]
    #[ignore = "requires the official PhotoCraft v0.13.0 and VectorCraft v0.15.0 MSI files"]
    fn validates_documented_official_msi_packages_with_windows_signature_and_database_apis() {
        #[derive(Serialize)]
        struct CatalogApp {
            id: &'static str,
            repo: &'static str,
            names: Vec<&'static str>,
            stems: Vec<&'static str>,
        }

        let dir = std::env::var_os("ARTCRAFT_TEST_OFFICIAL_MSI_DIR")
            .expect("set ARTCRAFT_TEST_OFFICIAL_MSI_DIR to the folder containing the documented releases");
        let dir = std::path::PathBuf::from(dir);
        let identities: serde_json::Value = serde_json::from_str(include_str!("../../../docs/windows-msi-identities.json")).unwrap();
        let mut cases = Vec::new();
        for asset_name in ["photocraft-0.13.0-windows-x64.msi", "vectorcraft-0.15.0-windows-x64.msi"] {
            let package = identities["packages"].as_array().unwrap().iter().find(|package| package["asset"] == asset_name).unwrap();
            let path = dir.join(asset_name);
            let actual_hash = crate::http::sha256_file(&path).unwrap();
            assert_eq!(actual_hash, package["sha256"].as_str().unwrap(), "official checksum mismatch: {asset_name}");
            let app_id = if asset_name.starts_with("photo") { "photocraft" } else { "vectorcraft" };
            let app = CraftApp::by_id(app_id).unwrap();
            let asset = release::Asset { name: asset_name.to_owned(), url: package["url"].as_str().unwrap().to_owned(), ..Default::default() };
            let release = release_identity(app, &asset).unwrap();
            cases.push(serde_json::json!({
                "path": path,
                "release": release,
                "sha256": actual_hash,
            }));
        }
        let catalog: Vec<_> = artcraft_launcher_core::catalog::SUITE
            .iter()
            .filter(|app| app.kind == AppKind::Official && app.repo.starts_with("storytold/"))
            .map(|app| CatalogApp { id: app.id, repo: app.repo, names: app.names().collect(), stems: app.stems().collect() })
            .collect();
        use base64::Engine as _;
        let case_data = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&cases).unwrap());
        let catalog_data = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&catalog).unwrap());
        let test = format!(
            "{POLICY}\n$cases=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{case_data}')) | ConvertFrom-Json\n\
             $catalog=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{catalog_data}')) | ConvertFrom-Json\n\
             foreach ($item in $cases) {{\n\
               $authorization=Get-ReleaseAuthorization $item $catalog\n\
               $file=[string]$item.path\n\
               $locked=[IO.File]::Open($file,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)\n\
               try {{ $hash=[Security.Cryptography.SHA256]::Create(); try {{ $actual=([BitConverter]::ToString($hash.ComputeHash($locked))).Replace('-','').ToLowerInvariant() }} finally {{ $hash.Dispose() }}; if ($actual -cne $authorization.sha) {{ throw 'MSI checksum differs from official publication' }} }} finally {{ $locked.Dispose() }}\n\
               Assert-OfficialSignature $file\n\
               Assert-MsiIdentity $file $authorization.app $authorization.version $authorization.asset\n\
               Write-Output ($item.release.app + ' ' + $authorization.version + ' checksum/signature/MSI identity passed')\n\
             }}\n"
        );
        let output = run_powershell_test(&test);
        assert_powershell_output_contains(&output, "photocraft 0.13.0 checksum/signature/MSI identity passed");
        assert_powershell_output_contains(&output, "vectorcraft 0.15.0 checksum/signature/MSI identity passed");
    }

    #[test]
    fn results_reject_torn_duplicate_and_out_of_range_reports() {
        let results = parse_results("0 0\n1 rejected\n0 1603\n9 0\n2 3010\n3 running\n4 1602\n5 0", 6);
        assert_eq!(results.len(), 5);
        assert_eq!(results[0], (0, Ok(())));
        assert!(results[1].1.is_err());
        assert_eq!(results[2], (2, Ok(())));
        assert_eq!(results[4], (4, Err("cancelled".into())));
    }

    #[test]
    fn encoded_policy_has_no_request_code_or_per_user_executable() {
        use base64::Engine as _;
        let encoded = encode_command("hello '\u{1f600}'");
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        let utf16: Vec<_> = bytes.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect();
        assert_eq!(String::from_utf16(&utf16).unwrap(), "hello '\u{1f600}'");
        let script = batch_script(&"a".repeat(32), 12345, &[]).unwrap();
        assert!(script.contains("Read-OfficialChecksums"));
        assert!(!script.contains("current_exe"));
        let compressed = compressed_command(&script).unwrap();
        assert!(compressed.len() < 30000, "the fixed policy must leave room under the command-line guard: {}", compressed.len());
        let bytes = base64::engine::general_purpose::STANDARD.decode(compressed).unwrap();
        let utf16: Vec<_> = bytes.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect();
        let bootstrap = String::from_utf16(&utf16).unwrap();
        let gzip = bootstrap.split('\'').nth(1).unwrap();
        let gzip = base64::engine::general_purpose::STANDARD.decode(gzip).unwrap();
        let mut decoder = flate2::read::GzDecoder::new(gzip.as_slice());
        let mut decoded = String::new();
        std::io::Read::read_to_string(&mut decoder, &mut decoded).unwrap();
        assert_eq!(decoded, compact_policy(&script));
    }

    #[test]
    fn cancellation_and_reboot_codes_are_preserved() {
        assert_eq!(msi_outcome(Some(3010)), Ok(()));
        assert_eq!(msi_outcome(Some(1641)), Ok(()));
        assert_eq!(msi_outcome(Some(1223)), Err("cancelled".into()));
        assert!(msi_outcome(Some(1618)).unwrap_err().contains("another installation"));
    }
}
