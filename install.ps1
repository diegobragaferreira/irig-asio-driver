#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Install the iRig USB ASIO driver (Rust) on Windows 10/11.

.DESCRIPTION
    Copies irig_asio.dll to %SystemRoot%\System32, then calls regsvr32
    to register the COM server and write the ASIO registry keys.

.PARAMETER DllPath
    Path to the compiled irig_asio.dll.
    Defaults to .\irig_asio\target\release\irig_asio.dll

.EXAMPLE
    .\install.ps1
    .\install.ps1 -DllPath "C:\MyBuild\irig_asio.dll"
#>

param(
    [string]$DllPath = ".\target\release\irig_asio.dll"
)

$ErrorActionPreference = "Stop"

# -- Resolve paths -------------------------------------------------------------
$DllPath  = Resolve-Path $DllPath -ErrorAction Stop
$DestDir  = "$env:SystemRoot\System32"
$DestPath = Join-Path $DestDir "irig_asio.dll"

Write-Host ""
Write-Host "=====================================================" -ForegroundColor Cyan
Write-Host "   iRig USB ASIO Driver (Rust) -- Installer          " -ForegroundColor Cyan
Write-Host "=====================================================" -ForegroundColor Cyan
Write-Host ""

# -- Step 1: Copy DLL ----------------------------------------------------------
Write-Host "[1/3] Copying DLL to $DestPath ..." -ForegroundColor Yellow
Copy-Item -Path $DllPath -Destination $DestPath -Force
Write-Host "      OK - Copied" -ForegroundColor Green

# -- Step 2: Register COM server -----------------------------------------------
Write-Host "[2/3] Registering COM server (regsvr32)..." -ForegroundColor Yellow
$result = Start-Process -FilePath "regsvr32.exe" `
    -ArgumentList "/s", "`"$DestPath`"" `
    -Wait -PassThru

if ($result.ExitCode -ne 0) {
    Write-Host "      FAILED - regsvr32 exit code: $($result.ExitCode)" -ForegroundColor Red
    Write-Host "      Check $env:TEMP\irig_asio.log for details." -ForegroundColor Red
    exit 1
}
Write-Host "      OK - Registered" -ForegroundColor Green

# -- Step 3: Verify registry keys ----------------------------------------------
Write-Host "[3/3] Verifying ASIO registry entry..." -ForegroundColor Yellow
$asioKey = "HKLM:\SOFTWARE\ASIO\iRig USB ASIO (Rust)"

if (Test-Path $asioKey) {
    $clsid = (Get-ItemProperty -Path $asioKey).CLSID
    Write-Host "      OK - ASIO key present" -ForegroundColor Green
    Write-Host "      CLSID = $clsid" -ForegroundColor Green
} else {
    Write-Host "      FAILED - ASIO registry key not found at:" -ForegroundColor Red
    Write-Host "      $asioKey" -ForegroundColor Red
    exit 1
}

Write-Host ""
Write-Host "=====================================================" -ForegroundColor Green
Write-Host "   Installation complete!                             " -ForegroundColor Green
Write-Host "                                                      " -ForegroundColor Green
Write-Host "   Open your DAW and select:                          " -ForegroundColor Green
Write-Host "   Audio -> ASIO -> 'iRig USB ASIO (Rust)'           " -ForegroundColor Green
Write-Host "=====================================================" -ForegroundColor Green
Write-Host ""
