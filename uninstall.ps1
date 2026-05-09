# octoPOS-admin - cleanup completo (Windows + WSL).
#
# Limpia un equipo a estado virgen para volver a probar el bootstrapper
# desde cero. Borra TODO lo que el bootstrapper deja:
#   Windows: servicio OctoPOSUpdater, tarea programada, MSIs instalados,
#            entradas de Add/Remove Programs, shortcuts, registry,
#            ProgramData\OctoPOS.
#   WSL:     /opt/octopos, containers octopos-api / octopos-mongodb /
#            octopos-autoheal, volúmenes huérfanos, imágenes de
#            ghcr.io/aarratia25/octopos-admin-api.
#
# Idempotente: si algo no existe, lo skipea sin error. Pensado para que
# vos como integrador puedas resetear y volver a empezar entre pruebas.
#
# NO toca otros stacks Docker que puedas tener corriendo (ej.
# octopos-web-mongodb, octopos-platform-*) ni la imagen mongo:7 — es
# compartida y la próxima instalación la reusa de cache.
#
# Uso:
#   Abrí PowerShell como Administrador y ejecutá:
#     iwr -useb https://raw.githubusercontent.com/aarratia25/octoPOS-install/main/uninstall.ps1 | iex

#Requires -RunAsAdministrator
$ErrorActionPreference = 'Continue'

function Step($m)    { Write-Host "==> $m" -ForegroundColor Cyan }
function Success($m) { Write-Host "    $m" -ForegroundColor Green }
function Skip($m)    { Write-Host "    $m" -ForegroundColor DarkGray }
function Warn($m)    { Write-Host "    $m" -ForegroundColor Yellow }

# ── Windows ───────────────────────────────────────────────────────────

Step 'Deteniendo servicio OctoPOSUpdater...'
$svc = Get-Service OctoPOSUpdater -ErrorAction SilentlyContinue
if ($svc) {
    Stop-Service OctoPOSUpdater -Force -ErrorAction SilentlyContinue
    & sc.exe delete OctoPOSUpdater | Out-Null
    Success 'Servicio borrado.'
} else {
    Skip 'No estaba registrado.'
}

Step 'Borrando tarea programada al boot...'
$task = Get-ScheduledTask -TaskName 'OctoPOS WSL Autostart' -ErrorAction SilentlyContinue
if ($task) {
    & schtasks /delete /tn 'OctoPOS WSL Autostart' /f | Out-Null
    Success 'Tarea borrada.'
} else {
    Skip 'No estaba registrada.'
}

Step 'Desinstalando OctoPOS Admin (.msi)...'
$adminEntries = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue |
    Where-Object { $_.DisplayName -eq 'OctoPOS Admin' }
if ($adminEntries) {
    foreach ($entry in $adminEntries) {
        $code = $entry.PSChildName
        Write-Host "    msiexec /x $code /quiet /qn"
        Start-Process msiexec -ArgumentList "/x $code /quiet /qn" -Wait
    }
    Success 'Admin desinstalado.'
} else {
    Skip 'No estaba instalado.'
}

Step 'Desinstalando OctoPOS Setup (.exe NSIS)...'
$setupEntries = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue |
    Where-Object { $_.DisplayName -eq 'OctoPOS Setup' }
if ($setupEntries) {
    foreach ($entry in $setupEntries) {
        $u = $entry.UninstallString -replace '^"|"$', ''
        Write-Host "    $u /S"
        Start-Process -FilePath $u -ArgumentList '/S' -Wait
    }
    Success 'Setup desinstalado.'
} else {
    Skip 'No quedó residual (auto-uninstall hizo su trabajo).'
}

Step 'Borrando shortcut del escritorio...'
$desktop = [Environment]::GetFolderPath('Desktop')
$shortcut = Join-Path $desktop 'OctoPOS Admin.lnk'
if (Test-Path $shortcut) {
    Remove-Item $shortcut -Force
    Success 'Shortcut borrado.'
} else {
    Skip 'No existía.'
}

Step 'Borrando registro HKLM\Software\OctoPOS...'
if (Test-Path 'HKLM:\Software\OctoPOS') {
    Remove-Item 'HKLM:\Software\OctoPOS' -Recurse -Force
    Success 'Borrado.'
} else {
    Skip 'No existía.'
}

Step 'Borrando RunOnce de resume post-reboot...'
$runOnce = 'HKLM:\Software\Microsoft\Windows\CurrentVersion\RunOnce'
if (Get-ItemProperty $runOnce -Name OctoPOSBootstrapResume -ErrorAction SilentlyContinue) {
    Remove-ItemProperty $runOnce -Name OctoPOSBootstrapResume -Force
    Success 'Borrado.'
} else {
    Skip 'No existía.'
}

Step 'Borrando C:\ProgramData\OctoPOS (logs + secretos)...'
if (Test-Path 'C:\ProgramData\OctoPOS') {
    Remove-Item 'C:\ProgramData\OctoPOS' -Recurse -Force
    Success 'Borrado.'
} else {
    Skip 'No existía.'
}

# ── WSL ────────────────────────────────────────────────────────────────

Step 'Verificando WSL...'
if ($null -eq (Get-Command wsl -ErrorAction SilentlyContinue)) {
    Warn 'WSL no instalado; salto la limpieza Linux.'
} else {
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
        Skip 'No hay distros WSL para limpiar.'
    } else {
        $distro = if ($env:OCTOPOS_WSL_DISTRO -and ($distroList -contains $env:OCTOPOS_WSL_DISTRO)) {
            $env:OCTOPOS_WSL_DISTRO
        } else { $distroList[0] }

        Step "Limpiando dentro de $distro..."

        # Heredoc bash con todo el cleanup. Llega a stdin de wsl bash via
        # la pipeline de PowerShell — así no tenemos que escribir un
        # archivo intermedio en el host.
        $bash = @'
set +e
echo "=> Bajando stack del admin..."
if [ -f /opt/octopos/docker-compose.yml ]; then
    cd /opt/octopos
    docker compose down -v 2>/dev/null
    echo "   stack bajado"
else
    echo "   /opt/octopos no existía"
fi

echo "=> Borrando /opt/octopos y /tmp/octopos-test..."
rm -rf /opt/octopos /tmp/octopos-test

echo "=> Borrando containers octopos-api / octopos-mongodb / octopos-autoheal..."
docker rm -f octopos-api octopos-mongodb octopos-autoheal 2>/dev/null | sed "s/^/   /"

echo "=> Borrando imagenes del admin (todas las versiones api-v*)..."
imgs=$(docker images -q ghcr.io/aarratia25/octopos-admin-api 2>/dev/null)
if [ -n "$imgs" ]; then
    docker rmi -f $imgs 2>/dev/null | sed "s/^/   /"
else
    echo "   no había"
fi

echo "=> Borrando volumenes octopos_* (del docker-compose nuestro)..."
docker volume rm \
    octopos_api_backups \
    octopos_api_data \
    octopos_api_uploads \
    octopos_mongo-data \
    octopos_mongo_data \
    octopos-api_mongo-data 2>/dev/null | sed "s/^/   /"

echo "=> Listo. mongo:7 + willfarrell/autoheal NO se borran (compartidas / cacheadas)."
echo
echo "--- Estado final ---"
echo "Containers octopos-* del admin:"
docker ps -a --filter name=octopos-api --filter name=octopos-mongodb --filter name=octopos-autoheal --format "{{.Names}}\t{{.Status}}" | grep -v octopos-web | grep -v octopos-platform
echo "Volumenes octopos_* del admin:"
docker volume ls --filter name=octopos | grep -v 'octopos-web\|octopos-platform' | tail -n +2
echo "/opt/octopos:"
ls /opt/octopos 2>/dev/null || echo "  (no existe)"
'@

        # Pasamos el script al wsl bash via heredoc por stdin. -u root
        # nos asegura permisos para tocar /opt/octopos.
        $bash | & wsl -d $distro -u root bash -s
        Success "WSL ($distro) limpiado."
    }
}

Write-Host ''
Write-Host '────────────────────────────────────────────────────────────' -ForegroundColor Green
Write-Host ' Limpieza completa.'                                          -ForegroundColor Green
Write-Host ' El equipo está virgen para probar el bootstrapper de nuevo.' -ForegroundColor Green
Write-Host '────────────────────────────────────────────────────────────' -ForegroundColor Green
Write-Host ''
Write-Host '  Próximo paso:'
Write-Host '    1. Bajá el ultimo OctoPOS-Setup-vX.Y.Z.exe de:'
Write-Host '       https://github.com/aarratia25/octoPOS-releases/releases/latest'
Write-Host '    2. Doble-click. Aceptá el UAC.'
Write-Host '    3. Mirá el panel de log mientras instala.'
Write-Host ''
