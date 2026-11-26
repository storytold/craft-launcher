#Requires -Version 7.0
<#
.SYNOPSIS
  Provision the manifest-pinned external signing reference as data, never execute it.
#>
[CmdletBinding()]
param([string] $Destination)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Copy-PinnedStream($InputStream, [string] $OutputPath, [long] $Size, [string] $Sha256,
                           [Threading.CancellationToken] $Cancellation = [Threading.CancellationToken]::None) {
  if ($Size -le 0) { throw 'Expected a positive bounded fixture size.' }
  $output = $null; $hash = [Security.Cryptography.SHA256]::Create()
  try {
    $output = [IO.File]::Open($OutputPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    $buffer = [byte[]]::new(65536); $total = 0L
    while (($count = $InputStream.ReadAsync($buffer, 0, [int][Math]::Min($buffer.Length, $Size - $total + 1), $Cancellation).GetAwaiter().GetResult()) -gt 0) {
      $total += $count
      if ($total -gt $Size) { throw 'Fixture stream exceeds its pinned size.' }
      $output.Write($buffer, 0, $count)
      [void]$hash.TransformBlock($buffer, 0, $count, $buffer, 0)
    }
    if ($total -ne $Size) { throw 'Fixture stream is shorter than its pinned size.' }
    [void]$hash.TransformFinalBlock([byte[]]::new(0), 0, 0)
    $actual = [BitConverter]::ToString($hash.Hash).Replace('-', '').ToLowerInvariant()
    if ($Sha256 -and $actual -cne $Sha256) { throw 'Fixture stream SHA-256 differs from the manifest.' }
  } finally {
    if ($null -ne $output) { $output.Dispose() }
    $hash.Dispose()
  }
}

function Receive-PinnedArchive($Pin, [string] $OutputPath) {
  $uri = [Uri]$Pin.url
  if ($uri.Scheme -cne 'https' -or $uri.Host -cne 'github.com') { throw 'Fixture source must be the pinned official HTTPS release.' }
  $client = [Net.Http.HttpClient]::new(); $cancel = [Threading.CancellationTokenSource]::new([TimeSpan]::FromMinutes(5))
  $response = $null; $stream = $null
  try {
    $client.DefaultRequestHeaders.UserAgent.ParseAdd('ArtCraft-Launcher-CI/1')
    $response = $client.GetAsync($uri, [Net.Http.HttpCompletionOption]::ResponseHeadersRead, $cancel.Token).GetAwaiter().GetResult()
    if ($response.StatusCode -ne [Net.HttpStatusCode]::OK -or $response.RequestMessage.RequestUri.Scheme -cne 'https') {
      throw 'The official fixture download did not return a complete HTTPS response.'
    }
    $length = $response.Content.Headers.ContentLength
    if ($null -ne $length -and $length -ne [long]$Pin.size) { throw 'Fixture HTTP length differs from the manifest.' }
    $stream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
    Copy-PinnedStream $stream $OutputPath ([long]$Pin.size) $Pin.sha256 $cancel.Token
  } finally {
    if ($null -ne $stream) { $stream.Dispose() }
    if ($null -ne $response) { $response.Dispose() }
    $cancel.Dispose(); $client.Dispose()
  }
}

function Expand-PinnedReference([string] $ArchivePath, $Pin, [string] $OutputDirectory) {
  $archiveStream = [IO.File]::Open($ArchivePath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
  $hash = [Security.Cryptography.SHA256]::Create(); $zip = $null
  try {
    if ($archiveStream.Length -ne [long]$Pin.source.size) { throw 'Pinned archive length differs before ZIP parsing.' }
    $digest = [BitConverter]::ToString($hash.ComputeHash($archiveStream)).Replace('-', '').ToLowerInvariant()
    if ($digest -cne $Pin.source.sha256) { throw 'Pinned archive SHA-256 differs before ZIP parsing.' }
    $archiveStream.Position = 0
    $zip = [IO.Compression.ZipArchive]::new($archiveStream, [IO.Compression.ZipArchiveMode]::Read, $true)
    if ($zip.Entries.Count -gt 10000) { throw 'Fixture ZIP has an excessive entry count.' }
    $entries = [Collections.Generic.Dictionary[string, IO.Compression.ZipArchiveEntry]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $zip.Entries) {
      if (-not $entries.TryAdd($entry.FullName, $entry)) { throw 'Fixture ZIP contains duplicate or case-aliased entry names.' }
    }
    $names = @($Pin.artifact.entry) + @($Pin.notices)
    foreach ($name in $names) {
      if ([string]::IsNullOrEmpty($name) -or $name -match '[/\\:]' -or $name -in @('.', '..')) { throw 'Fixture outputs must be explicit root filenames.' }
      if (-not $entries.ContainsKey($name) -or $entries[$name].FullName -cne $name) { throw "Fixture ZIP is missing exact root entry $name." }
      $entry = $entries[$name]
      $expected = if ($name -ceq $Pin.artifact.entry) { [long]$Pin.artifact.size } else { $entry.Length }
      if ($expected -le 0 -or $expected -gt 1048576 -or $entry.Length -ne $expected) { throw 'Fixture root entry size is invalid.' }
      $digest = if ($name -ceq $Pin.artifact.entry) { $Pin.artifact.sha256 } else { $null }
      $stream = $entry.Open()
      try { Copy-PinnedStream $stream (Join-Path $OutputDirectory $name) $expected $digest }
      finally { $stream.Dispose() }
    }
  } finally {
    if ($null -ne $zip) { $zip.Dispose() }
    $hash.Dispose(); $archiveStream.Dispose()
  }
}

function New-PinnedSignedFixture([string] $OutputDirectory) {
  $root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
  $manifest = Join-Path $root 'crates/native/tests/fixtures/windows-signed-reference.json'
  $pin = Get-Content -Raw -LiteralPath $manifest | ConvertFrom-Json
  if ($pin.schema -cne 'craft-test-fixture/v1') { throw 'Unsupported external fixture manifest.' }
  $destination = [IO.Path]::GetFullPath($OutputDirectory)
  if (Test-Path -LiteralPath $destination) { throw 'Fixture destination already exists; no existing data was changed.' }
  $stage = "$destination.partial-$([Guid]::NewGuid().ToString('N'))"
  [void][IO.Directory]::CreateDirectory($stage)
  try {
    $archive = Join-Path $stage 'reference.zip'
    Receive-PinnedArchive $pin.source $archive
    Expand-PinnedReference $archive $pin $stage
    Remove-Item -LiteralPath $archive
    Copy-Item -LiteralPath $manifest -Destination (Join-Path $stage 'provenance.json')
    [IO.Directory]::Move($stage, $destination)
    Write-Output "Provisioned pinned signing test data and upstream notices at $destination; reference was not executed."
  } finally {
    if (Test-Path -LiteralPath $stage) { Remove-Item -Recurse -LiteralPath $stage }
  }
}

# Dot-sourcing exposes the real bounded helpers to the small data-only regression script.
if ($MyInvocation.InvocationName -ne '.') {
  if (-not $Destination) {
    if (-not $env:RUNNER_TEMP) { throw 'RUNNER_TEMP or an explicit destination is required.' }
    $Destination = Join-Path $env:RUNNER_TEMP 'craft-launcher-signed-fixture'
  }
  New-PinnedSignedFixture $Destination
}
