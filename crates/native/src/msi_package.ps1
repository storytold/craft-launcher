# Read-only: what a downloaded package says about itself. Prints the Word Count summary property,
# then the package's own ALLUSERS property (an empty line when it has none), then its ProductCode.
# The package path comes from the environment, so it is never parsed as PowerShell.
$ErrorActionPreference = 'Stop'
$installer = $null
$summary = $null
$database = $null
function Read-Property([string]$name) {
    $view = $null
    $record = $null
    try {
        $view = $database.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $database, @("SELECT ``Value`` FROM ``Property`` WHERE ``Property``='$name'"))
        [void]$view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null)
        $record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
        if ($null -ne $record) {
            [string]$record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, @(1))
        } else {
            ''
        }
    } finally {
        foreach ($com in @($record, $view)) {
            if ($null -ne $com) {
                [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($com)
            }
        }
    }
}
try {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $summary = $installer.GetType().InvokeMember('SummaryInformation', 'GetProperty', $null, $installer, @($env:ARTCRAFT_MSI_PACKAGE, 0))
    Write-Output ([int]$summary.GetType().InvokeMember('Property', 'GetProperty', $null, $summary, @(15)))
    $database = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($env:ARTCRAFT_MSI_PACKAGE, 0))
    Write-Output (Read-Property 'ALLUSERS')
    Write-Output (Read-Property 'ProductCode')
} finally {
    foreach ($com in @($database, $summary, $installer)) {
        if ($null -ne $com) {
            [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($com)
        }
    }
}
