# octoPOS-admin - silent installer (Windows, non-interactive variant).
#
# Same flow as install.ps1 but EVERY input comes from environment
# variables instead of Read-Host. Designed to be invoked by the
# bootstrapper.exe which already collected (or pre-embedded) the
# tenant data; under no circumstance does this script open a console
# prompt.
#
# Required env vars (exit code 2 when any is missing):
#   OCTOPOS_BRANCH_SLUG       slug for the branch (e.g. "principal")
#   OCTOPOS_PLATFORM_API_KEY  shared secret with the platform
#
# Optional (sensible defaults applied):
#   OCTOPOS_PLATFORM_URL      default https://platform.octo-pos.net
#   OCTOPOS_MONGO_USER        default "octopos"
#   OCTOPOS_MONGO_DB          default "octopos"
#   OCTOPOS_MONGO_PASSWORD    auto-generated when empty
#   OCTOPOS_JWT_SECRET        auto-generated when empty
#   OCTOPOS_WSL_DISTRO        when set, prefer this distro over the first one
#
# ASCII-safe by design: PowerShell 5.1 mangles non-ASCII output on
# legacy consoles and the bootstrapper logs both stdout and stderr
# verbatim into %TEMP%\OctoPOS-Setup.log.

#Requires -RunAsAdministrator
$ErrorActionPreference = 'Stop'

$GitHubOwner       = 'aarratia25'
$GitHubRepo        = 'octoPOS-install'
$RawBase           = "https://raw.githubusercontent.com/$GitHubOwner/$GitHubRepo/main"
$DefaultDistroName = 'Ubuntu-22.04'

function Step($m)    { Write-Host "==> $m" -ForegroundColor Cyan }
function Success($m) { Write-Host "    $m" -ForegroundColor Green }
function Warn($m)    { Write-Host "    $m" -ForegroundColor Yellow }
function Fail($m)    { Write-Host "!!! $m" -ForegroundColor Red; exit 1 }

function New-Hex {
    $b = New-Object byte[] 32
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($b)
    return -join ($b | ForEach-Object { $_.ToString('x2') })
}

# --- Required env vars ----------------------------------------------------

$BranchSlug     = [Environment]::GetEnvironmentVariable('OCTOPOS_BRANCH_SLUG')
$PlatformApiKey = [Environment]::GetEnvironmentVariable('OCTOPOS_PLATFORM_API_KEY')

if ([string]::IsNullOrWhiteSpace($BranchSlug)) {
    Write-Host "!!! OCTOPOS_BRANCH_SLUG is required" -ForegroundColor Red
    exit 2
}
if ([string]::IsNullOrWhiteSpace($PlatformApiKey)) {
    Write-Host "!!! OCTOPOS_PLATFORM_API_KEY is required" -ForegroundColor Red
    exit 2
}

# --- Optional env vars ----------------------------------------------------

$PlatformUrl  = if ($env:OCTOPOS_PLATFORM_URL)  { $env:OCTOPOS_PLATFORM_URL }  else { 'https://platform.octo-pos.net' }
$MongoUser    = if ($env:OCTOPOS_MONGO_USER)    { $env:OCTOPOS_MONGO_USER }    else { 'octopos' }
$MongoDb      = if ($env:OCTOPOS_MONGO_DB)      { $env:OCTOPOS_MONGO_DB }      else { 'octopos' }
$MongoPassword = if ($env:OCTOPOS_MONGO_PASSWORD) { $env:OCTOPOS_MONGO_PASSWORD } else { New-Hex }
$JwtSecret    = if ($env:OCTOPOS_JWT_SECRET)    { $env:OCTOPOS_JWT_SECRET }    else { New-Hex }

# --- Windows version check ------------------------------------------------

Step 'Verificando version de Windows...'
$build = [int](Get-CimInstance Win32_OperatingSystem).BuildNumber
if ($build -lt 19041) { Fail "Se requiere Windows 10 build 19041+ (tenes $build)." }
Success "Windows build $build - OK."

# --- WSL + Ubuntu ---------------------------------------------------------

Step 'Verificando WSL...'
if ($null -eq (Get-Command wsl -ErrorAction SilentlyContinue)) {
    Step 'Instalando WSL (requiere reiniciar al terminar)...'
    wsl --install --no-distribution
    # Bootstrapper handles the reboot + RunOnce continuation. Exit
    # code 3 signals "reboot required, re-invoke me after reboot".
    Warn 'Reinicio requerido. El bootstrapper continuara despues del reboot.'
    exit 3
}
Success 'WSL disponible.'

# Same registry-based distro discovery as the interactive script —
# avoids fighting wsl -l -q's UTF-16LE stdout in PowerShell 5.1.
function Get-WslDistros {
    $key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss'
    if (-not (Test-Path $key)) { return @() }
    $names = Get-ChildItem $key -ErrorAction SilentlyContinue | ForEach-Object {
        $v = Get-ItemProperty $_.PSPath -Name 'DistributionName' -ErrorAction SilentlyContinue
        if ($v -and $v.DistributionName) { $v.DistributionName }
    }
    return @($names | Where-Object { $_ -and $_ -notmatch '^docker-desktop' })
}

$distroList = @(Get-WslDistros)
if ($distroList.Count -eq 0) {
    Step "Instalando $DefaultDistroName (puede tardar 5-10 min)..."
    wsl --install -d $DefaultDistroName
    # wsl --install -d still requires a reboot in some Windows
    # builds; treat the same as exit 3 so the bootstrapper re-runs.
    Warn 'Distribucion instalada. Reejecutar despues del primer arranque.'
    exit 3
}

if ($env:OCTOPOS_WSL_DISTRO -and ($distroList -contains $env:OCTOPOS_WSL_DISTRO)) {
    $DistroName = $env:OCTOPOS_WSL_DISTRO
} else {
    $DistroName = $distroList[0]
}
Success "Usando distro WSL: $DistroName"

# --- Handoff a WSL --------------------------------------------------------

Step 'Descargando install.sh...'
$sh = "$env:TEMP\octopos-install.sh"
$shContent = (Invoke-WebRequest -UseBasicParsing -Uri "$RawBase/install.sh").Content
$shContent = $shContent -replace "`r`n", "`n"
[System.IO.File]::WriteAllText($sh, $shContent, (New-Object System.Text.UTF8Encoding $false))
Success 'Descargado.'

Step 'Ejecutando install.sh dentro de WSL (Docker + Mongo + API)...'

function Convert-WindowsToWslPath($p) {
    $p = $p -replace '\\', '/'
    if ($p -match '^([A-Za-z]):(.*)$') {
        return "/mnt/$($matches[1].ToLower())$($matches[2])"
    }
    return $p
}
$wslPath = Convert-WindowsToWslPath $sh

$env:WSLENV = (@(
    'ADMIN_RAW_BASE/u', 'ADMIN_MONGO_USER/u', 'ADMIN_MONGO_DB/u', 'ADMIN_MONGO_PASSWORD/u',
    'ADMIN_JWT_SECRET/u', 'ADMIN_PLATFORM_URL/u', 'ADMIN_PLATFORM_API_KEY/u', 'ADMIN_BRANCH_SLUG/u'
) -join ':')
$env:ADMIN_RAW_BASE         = $RawBase
$env:ADMIN_MONGO_USER       = $MongoUser
$env:ADMIN_MONGO_DB         = $MongoDb
$env:ADMIN_MONGO_PASSWORD   = $MongoPassword
$env:ADMIN_JWT_SECRET       = $JwtSecret
$env:ADMIN_PLATFORM_URL     = $PlatformUrl
$env:ADMIN_PLATFORM_API_KEY = $PlatformApiKey
$env:ADMIN_BRANCH_SLUG      = $BranchSlug

wsl -d $DistroName -u root bash $wslPath
if ($LASTEXITCODE -ne 0) { Fail "install.sh fallo con codigo $LASTEXITCODE" }

Success 'Mongo + API corriendo dentro de WSL.'
exit 0
