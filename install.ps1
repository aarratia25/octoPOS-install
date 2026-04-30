# octoPOS-admin - instalador (Windows).
#
# Ejecutar como Administrador:
#   iwr -useb https://raw.githubusercontent.com/aarratia25/octoPOS-install/main/install.ps1 | iex
#
# Reejecutable: si WSL/Docker ya estan, se saltan.
#
# Nota: los mensajes estan sin acentos a proposito. Windows PowerShell 5.1
# no renderiza UTF-8 de forma consistente en la consola clasica, y los
# acentos/lineas `-` salian como `?`. Mantenemos el espaniol pero ASCII-safe.

#Requires -RunAsAdministrator
$ErrorActionPreference = 'Stop'

$GitHubOwner        = 'aarratia25'
$GitHubRepo         = 'octoPOS-install'
$RawBase            = "https://raw.githubusercontent.com/$GitHubOwner/$GitHubRepo/main"
$DefaultDistroName  = 'Ubuntu-22.04'
$DefaultPlatformUrl = 'https://platform.octo-pos.net'
$DistroName         = $null

function Step($m)    { Write-Host "==> $m" -ForegroundColor Cyan }
function Success($m) { Write-Host "    $m" -ForegroundColor Green }
function Warn($m)    { Write-Host "    $m" -ForegroundColor Yellow }
function Fail($m)    { Write-Host "!!! $m" -ForegroundColor Red; throw $m }

function Read-Default { param($p, $d)
  $v = Read-Host "   $p [$d]"; if ([string]::IsNullOrWhiteSpace($v)) { $d } else { $v }
}
function Read-Required { param($p)
  while ($true) {
    $v = Read-Host "   $p"
    if (-not [string]::IsNullOrWhiteSpace($v)) { return $v }
    Warn '   Este valor es obligatorio.'
  }
}
function Confirm-YN { param($p, $d = 'y')
  $hint = if ($d -eq 'y') { '[S/n]' } else { '[s/N]' }
  $a = Read-Host "   $p $hint"; if ([string]::IsNullOrWhiteSpace($a)) { $a = $d }
  return $a -match '^[SsYy]'
}
function New-Hex {
  $b = New-Object byte[] 32
  [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($b)
  return -join ($b | ForEach-Object { $_.ToString('x2') })
}
function ConvertTo-Slug { param([string]$s)
  # Normaliza acentos (San Bernardino -> san-bernardino, Santa Monica -> santa-monica).
  $n = [Text.NormalizationForm]::FormD
  $sb = New-Object System.Text.StringBuilder
  foreach ($c in $s.Normalize($n).ToCharArray()) {
    if ([Globalization.CharUnicodeInfo]::GetUnicodeCategory($c) -ne [Globalization.UnicodeCategory]::NonSpacingMark) {
      [void]$sb.Append($c)
    }
  }
  $clean = $sb.ToString().ToLowerInvariant()
  $clean = [Text.RegularExpressions.Regex]::Replace($clean, '[^a-z0-9]+', '-')
  return $clean.Trim('-')
}
function Read-Secret { param($label)
  if (Confirm-YN "Generar $label automaticamente?" 'y') {
    Success "   $label generado."
    return New-Hex
  }
  while ($true) {
    $v = Read-Host "   Ingresa $label"
    if (-not [string]::IsNullOrWhiteSpace($v)) { return $v }
    Warn '   Este valor es obligatorio.'
  }
}

# --- Windows ---------------------------------------------------------------

Step 'Verificando version de Windows...'
$build = [int](Get-CimInstance Win32_OperatingSystem).BuildNumber
if ($build -lt 19041) { Fail "Se requiere Windows 10 build 19041+ (tenes $build)." }
Success "Windows build $build - OK."

# --- WSL + Ubuntu ----------------------------------------------------------

Step 'Verificando WSL...'
if ($null -eq (Get-Command wsl -ErrorAction SilentlyContinue)) {
  Step 'Instalando WSL (requiere reiniciar al terminar)...'
  wsl --install --no-distribution
  Warn 'Reinicia Windows y volve a correr el one-liner.'
  return
}
Success 'WSL disponible.'

# Leer las distros WSL directo del Registry de Windows, que es donde
# el propio WSL las guarda. Evita pelear con la codificacion UTF-16LE
# del stdout de `wsl -l -q` (PowerShell 5.1 trunca el primer NULL y
# ningun workaround funciona parejo).
function Get-WslDistros {
  $key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss'
  if (-not (Test-Path $key)) { return @() }
  $names = Get-ChildItem $key -ErrorAction SilentlyContinue | ForEach-Object {
    $v = Get-ItemProperty $_.PSPath -Name 'DistributionName' -ErrorAction SilentlyContinue
    if ($v -and $v.DistributionName) { $v.DistributionName }
  }
  # Filtrar distros internas de Docker Desktop — no las usamos.
  return @($names | Where-Object { $_ -and $_ -notmatch '^docker-desktop' })
}

# @(...) fuerza array — sin esto PowerShell desempaqueta arrays de un
# solo elemento a string, y despues $distroList[0] indexa caracter por
# caracter ("Ubuntu"[0] = "U"). Ese era el bug que daba `Distro WSL: U`.
$distroList = @(Get-WslDistros)

if ($distroList.Count -eq 0) {
  Step "No hay ninguna distro WSL - instalando $DefaultDistroName (puede tardar 5-10 min)..."
  wsl --install -d $DefaultDistroName
  Warn 'Termina la configuracion del usuario unix en la ventana de Ubuntu y reejecuta.'
  return
}

if ($env:OCTOPOS_WSL_DISTRO -and ($distroList -contains $env:OCTOPOS_WSL_DISTRO)) {
  $DistroName = $env:OCTOPOS_WSL_DISTRO
} else {
  $DistroName = $distroList[0]
}
Success "Usando distro WSL: $DistroName"

# --- Onboarding ------------------------------------------------------------

Step 'Datos de la instalacion - enter acepta el valor entre [corchetes].'
Write-Host ''
Write-Host '--- Base de datos -------------------' -ForegroundColor DarkGray
$MongoUser     = Read-Default 'Usuario de la base' 'octopos'
$MongoDb       = Read-Default 'Nombre de la base'  'octopos'
$MongoPassword = Read-Secret  'contrasena de la base (MONGO_PASSWORD)'

Write-Host ''
Write-Host '--- Autenticacion API ---------------' -ForegroundColor DarkGray
$JwtSecret = Read-Secret 'clave JWT (JWT_SECRET)'

Write-Host ''
Write-Host '--- Conexion con la plataforma ------' -ForegroundColor DarkGray
$PlatformUrl    = $DefaultPlatformUrl
$PlatformApiKey = Read-Required 'API key de la plataforma (secreto compartido)'
$BranchName     = Read-Required 'Nombre de la sucursal (ej: San Bernardino)'
$BranchSlug     = ConvertTo-Slug $BranchName
if ([string]::IsNullOrWhiteSpace($BranchSlug)) {
  Fail "No se pudo generar slug a partir de '$BranchName'."
}
Success "Slug generado: $BranchSlug"

Write-Host ''

# --- Handoff a WSL ---------------------------------------------------------

Step 'Descargando install.sh...'
$sh = "$env:TEMP\octopos-install.sh"
# Bajamos el contenido a memoria y lo escribimos como UTF-8 SIN BOM
# y con line endings LF. `Set-Content` en PS 5.1 agrega BOM y CRLF,
# cosa que rompe bash (el shebang no se detecta si empieza con BOM).
$shContent = (Invoke-WebRequest -UseBasicParsing -Uri "$RawBase/install.sh").Content
$shContent = $shContent -replace "`r`n", "`n"
[System.IO.File]::WriteAllText($sh, $shContent, (New-Object System.Text.UTF8Encoding $false))
Success 'Descargado.'

Step 'Ejecutando install.sh dentro de WSL (Docker + Mongo + API)...'
# Convertimos el path Windows -> Linux aca mismo en PowerShell para no
# depender de `wsl wslpath` (los backslashes se comen en el paso a WSL
# y wslpath recibe "C:UsersalfreAppData..." sin separadores).
function Convert-WindowsToWslPath($p) {
  $p = $p -replace '\\', '/'
  if ($p -match '^([A-Za-z]):(.*)$') {
    return "/mnt/$($matches[1].ToLower())$($matches[2])"
  }
  return $p
}
$wslPath = Convert-WindowsToWslPath $sh
$env:WSLENV = (@(
  'ADMIN_RAW_BASE/u','ADMIN_MONGO_USER/u','ADMIN_MONGO_DB/u','ADMIN_MONGO_PASSWORD/u',
  'ADMIN_JWT_SECRET/u','ADMIN_PLATFORM_URL/u','ADMIN_PLATFORM_API_KEY/u','ADMIN_BRANCH_SLUG/u'
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
if ($LASTEXITCODE -ne 0) { Fail 'install.sh fallo - revisa los logs arriba.' }

# --- Fin -------------------------------------------------------------------

Step 'Instalacion base lista.'
Write-Host ''
Write-Host '-------------------------------------------------------------' -ForegroundColor Green
Write-Host '  Mongo + API corriendo dentro de WSL.'                        -ForegroundColor Green
Write-Host '  Configuracion en /opt/octopos/.env (dentro de WSL).'         -ForegroundColor Green
Write-Host ''
Write-Host '  Siguiente:'                                                   -ForegroundColor Green
Write-Host "    1. Baja el .msi del admin: https://github.com/$GitHubOwner/octoPOS-admin/releases/latest" -ForegroundColor Green
Write-Host '    2. Instalalo en esta misma PC.'                             -ForegroundColor Green
Write-Host '    3. Al abrir, el admin lee el .env y se activa solo.'        -ForegroundColor Green
Write-Host '    4. Instala el POS .msi en cada caja.'                       -ForegroundColor Green
Write-Host '-------------------------------------------------------------' -ForegroundColor Green
