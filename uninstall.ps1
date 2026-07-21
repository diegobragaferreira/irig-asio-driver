#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Uninstall the iRig USB ASIO driver (Rust) from Windows.
#>

$ErrorActionPreference = "Stop"
$DestPath = "$env:SystemRoot\System32\irig_asio.dll"

Write-Host ""
Write-Host "=====================================================" -ForegroundColor Cyan
Write-Host "   iRig USB ASIO Driver (Rust) -- Uninstaller        " -ForegroundColor Cyan
Write-Host "=====================================================" -ForegroundColor Cyan
Write-Host ""

# -- Step 1: Unregister --------------------------------------------------------
Write-Host "[1/3] Unregistering COM server..." -ForegroundColor Yellow
if (Test-Path $DestPath) {
    $result = Start-Process -FilePath "regsvr32.exe" `
        -ArgumentList "/s", "/u", "`"$DestPath`"" `
        -Wait -PassThru
    if ($result.ExitCode -eq 0) {
        Write-Host "      OK - Unregistered" -ForegroundColor Green
    } else {
        Write-Host "      WARNING - regsvr32 /u returned $($result.ExitCode) -- continuing" -ForegroundColor Yellow
    }
} else {
    Write-Host "      WARNING - DLL not found at $DestPath -- skipping regsvr32" -ForegroundColor Yellow
}

# -- Step 2: Remove DLL --------------------------------------------------------
Write-Host "[2/3] Removing DLL..." -ForegroundColor Yellow
if (Test-Path $DestPath) {
    Remove-Item $DestPath -Force
    Write-Host "      OK - Removed $DestPath" -ForegroundColor Green
} else {
    Write-Host "      WARNING - DLL already removed" -ForegroundColor Yellow
}

# -- Step 3: Clean up residual registry keys -----------------------------------
Write-Host "[3/3] Cleaning up registry keys..." -ForegroundColor Yellow
$keys = @(
    "HKLM:\SOFTWARE\ASIO\iRig USB ASIO (Rust)",
    "HKCR:\CLSID\{8F3D4A2B-E1C7-4F89-A0D3-6B2E9C1F5847}"
)
foreach ($k in $keys) {
    if (Test-Path $k) {
        Remove-Item -Path $k -Recurse -Force
        Write-Host "      OK - Removed $k" -ForegroundColor Green
    } else {
        Write-Host "      OK - Already absent: $k" -ForegroundColor Green
    }
}

Write-Host ""
Write-Host "=====================================================" -ForegroundColor Green
Write-Host "   iRig USB ASIO driver uninstalled successfully.    " -ForegroundColor Green
Write-Host "=====================================================" -ForegroundColor Green
Write-Host ""
