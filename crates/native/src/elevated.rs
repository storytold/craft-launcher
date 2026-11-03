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
        @('MsiSummaryInfoGetPropertyW', @($u, $u, $ref, $iref, [Text.StringBuilder], $ref)),
        @('MsiViewClose', @($u)),
        @('MsiCloseHandle', @($u)))
    foreach ($definition in $definitions) {
        $method = $type.DefinePInvokeMethod($definition[0], $dll,
            [Reflection.MethodAttributes]'Public, Static, PinvokeImpl',
            [Reflection.CallingConventions]::Standard, $u, [Type[]]$definition[1],
            [Runtime.InteropServices.CallingConvention]::Winapi,
            [Runtime.InteropServices.CharSet]::Unicode)
        $method.SetImplementationFlags([Reflection.MethodImplAttributes]::PreserveSig)
    }
    return $type.CreateType()
}

function Get-MsiTemplate($api, [uint32]$db) {
    # PID_TEMPLATE (7) records the actual package platform independently of its filename.
    $arguments = [object[]]@($db, [string]$null, [uint32]0, [uint32]0)
    if ($api.GetMethod('MsiGetSummaryInformationW').Invoke($null, $arguments) -ne 0) { throw 'MSI summary query failed' }
    $summary = [uint32]$arguments[3]
    try {
        $template = [Text.StringBuilder]::new(256)
        $arguments = [object[]]@($summary, [uint32]7, [uint32]0, [int32]0, $template, [uint32]256)
        if ($api.GetMethod('MsiSummaryInfoGetPropertyW').Invoke($null, $arguments) -ne 0) { throw 'MSI platform metadata is unreadable' }
        return $template.ToString().Split(';')[0]
    } finally { $null = $api.GetMethod('MsiCloseHandle').Invoke($null, [object[]]@($summary)) }
}

function Get-MsiProperty($api, [uint32]$db, [string]$name) {
    # name is a program literal. MSI APIs open the protected staged file READ-ONLY.
    $arguments = [object[]]@($db, "SELECT Value FROM Property WHERE Property='$name'", [uint32]0)
    if ($api.GetMethod('MsiDatabaseOpenViewW').Invoke($null, $arguments) -ne 0) { throw 'MSI query failed' }
    $view = [uint32]$arguments[2]
    try {
        if ($api.GetMethod('MsiViewExecute').Invoke($null, [object[]]@($view, [uint32]0)) -ne 0) { throw 'MSI query failed' }
        $arguments = [object[]]@($view, [uint32]0)
        $status = $api.GetMethod('MsiViewFetch').Invoke($null, $arguments)
        if ($status -eq 259) { return '' }
        if ($status -ne 0) { throw 'MSI record failed' }
        $record = [uint32]$arguments[1]
        try {
            $value = [Text.StringBuilder]::new(1024)
            $arguments = [object[]]@($record, [uint32]1, $value, [uint32]1024)
            if ($api.GetMethod('MsiRecordGetStringW').Invoke($null, $arguments) -ne 0) { throw 'MSI property too long or unreadable' }
            return $value.ToString()
        } finally { $null = $api.GetMethod('MsiCloseHandle').Invoke($null, [object[]]@($record)) }
    } finally {
        $null = $api.GetMethod('MsiViewClose').Invoke($null, [object[]]@($view))
        $null = $api.GetMethod('MsiCloseHandle').Invoke($null, [object[]]@($view))
    }
}

function Assert-MsiIdentity([string]$file, $app, [string]$version, [string]$asset) {
    $api = New-MsiApi
    # Reflection.Invoke must receive System.String, not PowerShell's PSObject wrapper.
    $arguments = [object[]]@([string]$file, [IntPtr]::Zero, [uint32]0)
    if ($api.GetMethod('MsiOpenDatabaseW').Invoke($null, $arguments) -ne 0) { throw 'Cannot open MSI read-only' }
    $db = [uint32]$arguments[2]
    try {
        $packageArch = switch -Regex ($asset) {
            '-windows-x64\.msi$' { 'x64'; break }
            '-windows-x86\.msi$' { 'Intel'; break }
            '-windows-arm64\.msi$' { 'Arm64'; break }
            default { throw 'MSI architecture is not authorized' }
        }
        if ((Get-MsiTemplate $api $db) -cne $packageArch -or
            (Get-MsiProperty $api $db 'ALLUSERS') -cne '1' -or
            (Get-MsiProperty $api $db 'MSIINSTALLPERUSER') -notin @('', '0') -or
            (Get-MsiProperty $api $db 'Manufacturer') -cne 'Learning Machines LLC' -or
            (Get-MsiProperty $api $db 'ProductName') -cnotin $app.names -or
            (Get-MsiProperty $api $db 'ProductVersion') -cne $version) { throw 'MSI identity/scope mismatch' }
    } finally { $null = $api.GetMethod('MsiCloseHandle').Invoke($null, [object[]]@($db)) }
}

function Assert-AuthenticodePublisher([string]$publisher) {
    # Signing identity is independent of the authored MSI Manufacturer (Learning Machines LLC).
    # Exact identity, not an alias/prefix/wildcard. Windows validates the certificate chain first.
    if ($publisher -cne 'Learning Machines Inc') { throw 'Official Authenticode publisher required' }
}

function Assert-OfficialSignature([string]$file) {
    try { $sig = Microsoft.PowerShell.Security\Get-AuthenticodeSignature -LiteralPath $file }
    catch { throw 'Official Authenticode signature required' }
    if ($sig.Status -ne 'Valid' -or $null -eq $sig.SignerCertificate) {
        throw 'Official Authenticode signature required'
    }
    $certificate = $sig.SignerCertificate
    Assert-AuthenticodePublisher ($certificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false))
    if ($certificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::OrganizationName, $false) -cne 'Learning Machines Inc') {
        throw 'Official Authenticode organization required'
    }
}

function Get-ReleaseAuthorization($item, $catalog) {
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
    $sha = Get-PublishedHash (Read-OfficialChecksums $url) $asset
    if ($item.sha256 -cne $sha) { throw 'Caller checksum disagrees with official publication' }
    return @{ app = $app; sha = $sha; version = $numeric; asset = $asset }
}

function Copy-VerifiedPackage($item, $authorization, [string]$root, [int]$index, [int]$port, [string]$session) {
    $target = [IO.Path]::Combine($root, "$index.msi")
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
        $hash = [Security.Cryptography.SHA256]::Create()
        try { $actual = ([BitConverter]::ToString($hash.ComputeHash($locked))).Replace('-', '').ToLowerInvariant() }
        finally { $hash.Dispose() }
        if ($actual -cne $authorization.sha) { throw 'Changed package' }
        Assert-OfficialSignature $target
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
    if ($request.items.Count -lt 1 -or $request.items.Count -gt 64) { throw 'Invalid batch size' }
    if ($request.port -lt 1 -or $request.port -gt 65535) { throw 'Invalid local source port' }
    $root = New-ProtectedRun ([string]$request.session)
    $results = New-ProtectedFile ([IO.Path]::Combine($root, 'results')) $true
    $writer = [IO.StreamWriter]::new($results, [Text.UTF8Encoding]::new($false))
    $writer.AutoFlush = $true
    try {
        for ($i = 0; $i -lt $request.items.Count; $i++) {
            $item = $request.items[$i]
            $code = 'rejected'
            $staged = [IO.Path]::Combine($root, "$i.msi")
            try {
                $authorization = Get-ReleaseAuthorization $item $catalog
                $staged = Copy-VerifiedPackage $item $authorization $root $i $request.port $request.session
                # Check immediately before each installation, including apps started during UAC.
                if (Test-AppRunning $authorization.app) { $code = 'running' }
                else {
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
                            $destination = Assert-InstallDestination ([string]$item.destination) $authorization.app
                            # Always pass a trailing separator, doubled before the closing quote
                            # for Windows argument decoding (including a drive-root folder).
                            $start.Arguments += ' INSTALLFOLDER="' + $destination.TrimEnd('\') + '\\"'
                        }
                        $child = [Diagnostics.Process]::Start($start)
                        try { $child.WaitForExit(); $code = [string]$child.ExitCode }
                        finally { $child.Dispose() }
                    } finally { $locked.Dispose() }
                }
            } catch {
                # Deliberately return fixed codes, not exception text containing source data.
                $code = 'rejected'
            } finally {
                [IO.File]::Delete($staged)
            }
            $writer.WriteLine("$i $code")
        }
    } finally { $writer.Dispose(); $results.Dispose() }
}
"#;

#[cfg(any(windows, test))]
fn encode_command(script: &str) -> String {
    use base64::Engine as _;
    let bytes: Vec<_> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(any(windows, test))]
fn compressed_command(script: &str) -> Result<String, String> {
    use base64::Engine as _;
    use std::io::Write as _;
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(script.as_bytes()).map_err(|e| e.to_string())?;
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
        assert!(POLICY.contains("X509NameType]::OrganizationName") && POLICY.contains("Official Authenticode organization required"));
        assert!(POLICY.contains("'Manufacturer') -cne 'Learning Machines LLC'"), "MSI Manufacturer policy");
        assert!(POLICY.contains("Get-MsiTemplate") && POLICY.contains("MSI architecture is not authorized"));
        assert!(POLICY.contains("(Get-MsiProperty $api $db 'ALLUSERS') -cne '1'"));
        assert!(POLICY.contains("(Get-MsiProperty $api $db 'MSIINSTALLPERUSER') -notin @('', '0')"));
        assert!(POLICY.contains("[object[]]@([string]$file, [IntPtr]::Zero, [uint32]0)"));
        assert!(!POLICY.contains("Learning Machines LLC') -or"), "the certificate subject and MSI Manufacturer stay separate");
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
        assert_eq!(decoded, script);
    }

    #[test]
    fn cancellation_and_reboot_codes_are_preserved() {
        assert_eq!(msi_outcome(Some(3010)), Ok(()));
        assert_eq!(msi_outcome(Some(1641)), Ok(()));
        assert_eq!(msi_outcome(Some(1223)), Err("cancelled".into()));
        assert!(msi_outcome(Some(1618)).unwrap_err().contains("another installation"));
    }
}
