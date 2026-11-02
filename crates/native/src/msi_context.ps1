# Read-only MSI registration, limited to the current account and machine contexts.
# $code is supplied by the caller only after strict braced-GUID validation.
$ErrorActionPreference = 'Stop'
$installer = $null
$products = $null
try {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $products = $installer.ProductsEx($code, '', 7)
    foreach ($product in $products) {
        try {
            $context = $product.GetType().InvokeMember('Context', 'GetProperty', $null, $product, $null)
            Write-Output ([int]$context)
        } finally {
            [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($product)
        }
    }
} finally {
    foreach ($com in @($products, $installer)) {
        if ($null -ne $com) {
            [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($com)
        }
    }
}
