<#
.SYNOPSIS
  Check MSI icon references without building or installing ArtCraft Launcher.

.DESCRIPTION
  Windows Installer caches each <Icon> under %APPDATA%\Microsoft\Installer\{ProductCode}, which
  every update (a major upgrade, new ProductCode) deletes. A shortcut that used one left taskbar
  pins, which an update never refreshes, with a blank icon, so shortcuts must not name an <Icon>:
  they take the exe's embedded icon. ARPPRODUCTICON must reference an <Icon> that exists, with an
  .exe or .ico extension (ICE50), and every shortcut must carry the app's AppUserModelID (toast
  notifications need it).
#>
param(
  [string] $WxsPath = (Join-Path $PSScriptRoot 'artcraft-launcher.wxs')
)
$ErrorActionPreference = 'Stop'
$AppId = 'ai.storyteller.artcraft-launcher'
[xml] $Wxs = Get-Content -LiteralPath $WxsPath -Raw
$Ns = [Xml.XmlNamespaceManager]::new($Wxs.NameTable)
$Ns.AddNamespace('w', 'http://wixtoolset.org/schemas/v4/wxs')
$Icons = @{}
foreach ($icon in $Wxs.SelectNodes('//w:Icon', $Ns)) {
  $Icons[$icon.GetAttribute('Id')] = $icon
}

$shortcuts = $Wxs.SelectNodes('//w:Shortcut', $Ns)
if ($shortcuts.Count -eq 0) { throw 'No <Shortcut> in the MSI.' }
foreach ($shortcut in $shortcuts) {
  $id = $shortcut.GetAttribute('Id')
  if ($shortcut.HasAttribute('Icon')) {
    throw "Shortcut '$id' must not use an MSI <Icon>: an update deletes it and pinned shortcuts go blank. Leave Icon off to use the exe's own icon."
  }
  $aumid = $shortcut.SelectSingleNode('w:ShortcutProperty[@Key="System.AppUserModel.ID"]', $Ns)
  if (-not $aumid -or $aumid.GetAttribute('Value') -ne $AppId) {
    throw "Shortcut '$id' must set System.AppUserModel.ID to $AppId."
  }
}

$arpIcon = $Wxs.SelectSingleNode('//w:Property[@Id="ARPPRODUCTICON"]', $Ns)
if ($arpIcon) {
  $iconId = $arpIcon.GetAttribute('Value')
  if (-not $Icons.ContainsKey($iconId)) {
    throw "ARPPRODUCTICON references missing icon '$iconId'."
  }
  if ([IO.Path]::GetExtension($iconId) -notin '.exe', '.ico') {
    throw "ARPPRODUCTICON icon '$iconId' must have an .exe or .ico extension (ICE50)."
  }
}
Write-Output 'Windows shortcut icon references ok'
