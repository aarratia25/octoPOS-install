# octoPOS-admin - cleanup completo (Windows + WSL).
#
# Limpia un equipo a estado virgen para volver a probar el bootstrapper
# desde cero. Borra TODO lo que el bootstrapper deja:
#   Windows: servicio OctoPOSUpdater, tarea programada, MSIs instalados,
#            entradas de Add/Remove Programs, shortcuts, registry,
#            ProgramData\OctoPOS.
#   WSL:     /opt/octopos, containers octopos-api / octopos-mongodb /
#            octopos-autoheal, volumenes huerfanos, imagenes de
#            ghcr.io/aarratia25/octopos-admin-api.
#
# Idempotente: si algo no existe, lo skipea sin error. Pensado para que
# el integrador pueda resetear y volver a empezar entre pruebas.
#
# NO toca otros stacks Docker que puedas tener corriendo (ej.
# octopos-web-mongodb, octopos-platform-*) ni la imagen mongo:7: es
# compartida y la proxima instalacion la reusa de cache.
#
# Nota: los mensajes salen sin acentos a proposito. Windows PowerShell
# 5.1 no renderiza UTF-8 de forma consistente en la consola clasica;
# los acentos y caracteres de dibujo Unicode salen como `?`. Mantenemos
# espaniol pero ASCII-safe.
#
# Uso:
#   Abri PowerShell como Administrador y ejecuta:
#     iwr -useb https://raw.githubusercontent.com/aarratia25/octoPOS-install/main/uninstall.ps1 | iex

#Requires -RunAsAdministrator
$ErrorActionPreference = 'Continue'

# Hint to the console driver to expect UTF-8. Helps when the bash
# heredoc below pipes log lines through. Harmless on consoles that
# don't honor it.
try {
    [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
} catch {}

function Step($m)    { Write-Host "==> $m" -ForegroundColor Cyan }
function Success($m) { Write-Host "    $m" -ForegroundColor Green }
function Skip($m)    { Write-Host "    $m" -ForegroundColor DarkGray }
function Warn($m)    { Write-Host "    $m" -ForegroundColor Yellow }

# --- Windows ---

Step 'Deteniendo servicio OctoPOSUpdater...'
$svc = Get-Service OctoPOSUpdater -ErrorAction SilentlyContinue
if ($svc) {
    Stop-Service OctoPOSUpdater -Force -ErrorAction SilentlyContinue
    & sc.exe delete OctoPOSUpdater | Out-Null
    Success 'Servicio borrado.'
} else {
    Skip 'No estaba registrado.'
}

Step 'Borrando tareas programadas...'
$removedTasks = 0
foreach ($name in @('OctoPOS WSL Autostart', 'OctoPOSSetupCleanup')) {
    $task = Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
    if ($task) {
        & schtasks /delete /tn $name /f 2>$null | Out-Null
        Write-Host "    borrada: $name" -ForegroundColor Green
        $removedTasks++
    }
}
if ($removedTasks -eq 0) { Skip 'Ninguna registrada.' }

Step 'Cerrando OctoPOS Admin si esta corriendo...'
$killedAdmin = 0
# El binario se llama octopos-admin.exe (Cargo package name) aunque
# el productName de display sea "OctoPOS Admin". Matamos ambos por
# si en futuro cambia el nombre.
Get-Process -Name 'octopos-admin','OctoPOS Admin' -ErrorAction SilentlyContinue |
    ForEach-Object {
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
        $killedAdmin++
    }
# WebView2 hosts the Tauri UI as a separate child process tree; el
# admin lanza varios `msedgewebview2.exe`. Sin matarlos quedan
# huerfanos consumiendo RAM.
Get-Process -Name 'msedgewebview2' -ErrorAction SilentlyContinue |
    Where-Object { $_.MainModule.FileName -like '*OctoPOS*' -or $_.Path -like '*OctoPOS*' } |
    ForEach-Object {
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
        $killedAdmin++
    }
if ($killedAdmin -gt 0) {
    Success "$killedAdmin proceso(s) cerrado(s)."
    Start-Sleep -Milliseconds 500   # Que Windows libere handles del binario
} else {
    Skip 'No estaba corriendo.'
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
    # Despues del msiexec verificar que el directorio de Program Files
    # se haya borrado. Si quedo algo, forzar limpieza manual.
    $adminDir = Join-Path $env:ProgramFiles 'OctoPOS Admin'
    if (Test-Path $adminDir) {
        Write-Host "    msiexec dejo residuos en $adminDir, limpiando manualmente..." -ForegroundColor Yellow
        Remove-Item $adminDir -Recurse -Force -ErrorAction SilentlyContinue
        if (Test-Path $adminDir) {
            Warn "No se pudo borrar $adminDir (handle abierto?)."
        }
    }
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
    Skip 'No quedo residual (auto-uninstall hizo su trabajo).'
}

Step 'Borrando shortcuts del escritorio...'
$desktop = [Environment]::GetFolderPath('Desktop')
$publicDesktop = [Environment]::GetFolderPath('CommonDesktopDirectory')
$removed = 0
foreach ($base in @($desktop, $publicDesktop)) {
    foreach ($name in @('OctoPOS Admin.lnk', 'OctoPOS Setup.lnk')) {
        $path = Join-Path $base $name
        if (Test-Path $path) {
            Remove-Item $path -Force -ErrorAction SilentlyContinue
            if (-not (Test-Path $path)) {
                Write-Host "    borrado: $path" -ForegroundColor Green
                $removed++
            }
        }
    }
}
if ($removed -eq 0) { Skip 'Ninguno encontrado.' }

Step 'Borrando registro HKLM\Software\OctoPOS...'
if (Test-Path 'HKLM:\Software\OctoPOS') {
    # Remove-Item -Recurse falla con "Requested registry access is not
    # allowed" cuando la key fue creada por SYSTEM (el companion service
    # la escribio sobre HKLM con su token elevado). reg.exe respeta los
    # ACLs pero no exige ownership como Remove-Item.
    & reg.exe delete 'HKLM\Software\OctoPOS' /f 2>&1 | Out-Null
    if (Test-Path 'HKLM:\Software\OctoPOS') {
        Warn 'reg.exe delete no pudo borrarla; revisar ownership manualmente.'
    } else {
        Success 'Borrado.'
    }
} else {
    Skip 'No existia.'
}

Step 'Borrando RunOnce de resume post-reboot...'
$runOnce = 'HKLM:\Software\Microsoft\Windows\CurrentVersion\RunOnce'
if (Get-ItemProperty $runOnce -Name OctoPOSBootstrapResume -ErrorAction SilentlyContinue) {
    Remove-ItemProperty $runOnce -Name OctoPOSBootstrapResume -Force
    Success 'Borrado.'
} else {
    Skip 'No existia.'
}

Step 'Cerrando procesos que mantienen setup.log abierto...'
$killed = 0
# Bootstrappers de Tauri (en cualquiera de los dos nombres)
Get-Process -Name 'octopos-bootstrapper', 'OctoPOS Setup' -ErrorAction SilentlyContinue |
    ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue; $killed++ }
# Notepad abierto via el boton "Abrir setup.log" del splash
Get-Process -Name notepad -ErrorAction SilentlyContinue |
    Where-Object { $_.MainWindowTitle -like '*setup.log*' } |
    ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue; $killed++ }
# Companion service binary (octopos-updater.exe). Stop-Service lo
# detiene pero el proceso puede tardar unos segundos en cerrar y
# liberar sus handles al ProgramData secret/log; matamos por si acaso.
Get-Process -Name 'octopos-updater' -ErrorAction SilentlyContinue |
    ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue; $killed++ }
if ($killed -gt 0) {
    Success "$killed proceso(s) cerrado(s)."
} else {
    Skip 'Ninguno corriendo.'
}

Step 'Borrando C:\Program Files\OctoPOS Updater (binario del companion)...'
$updaterRoot = Join-Path $env:ProgramFiles 'OctoPOS Updater'
if (Test-Path $updaterRoot) {
    Remove-Item $updaterRoot -Recurse -Force -ErrorAction SilentlyContinue
    if (Test-Path $updaterRoot) {
        Warn 'No pude borrar (handle del servicio?). Reinicia + reejecuta.'
    } else {
        Success 'Borrado.'
    }
} else {
    Skip 'No existia.'
}

Step 'Borrando C:\ProgramData\OctoPOS (logs + secretos)...'
if (Test-Path 'C:\ProgramData\OctoPOS') {
    # Retry con back-off: Windows tarda unos segundos en liberar
    # handles despues de un Stop-Service / Stop-Process, sobre todo
    # del companion service que escribia en updater-secret.
    $deleted = $false
    for ($i = 1; $i -le 6; $i++) {
        Remove-Item 'C:\ProgramData\OctoPOS' -Recurse -Force -ErrorAction SilentlyContinue
        if (-not (Test-Path 'C:\ProgramData\OctoPOS')) {
            $deleted = $true
            break
        }
        Start-Sleep -Milliseconds (500 * $i)   # 0.5, 1, 1.5, 2, 2.5, 3 = 10.5s total
    }
    if ($deleted) {
        Success 'Borrado.'
    } else {
        # Last-resort: schedule a delete on next boot via MoveFileEx
        # with MOVEFILE_DELAY_UNTIL_REBOOT (flag = 4). Windows replays
        # these pending operations during the next session manager
        # init, before any user can grab a handle on the targets.
        Warn 'Handles abiertos despues de 10s. Encolando borrado para el proximo boot...'
        try {
            Add-Type -ErrorAction SilentlyContinue -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class OctoFileOps {
    [DllImport("kernel32.dll", CharSet=CharSet.Auto, SetLastError=true)]
    public static extern bool MoveFileEx(string existing, string replacement, int flags);
}
'@
            $queued = 0
            # Walk children-first so the parent dir can be queued last.
            Get-ChildItem 'C:\ProgramData\OctoPOS' -Recurse -Force -ErrorAction SilentlyContinue |
                Sort-Object FullName -Descending |
                ForEach-Object {
                    if ([OctoFileOps]::MoveFileEx($_.FullName, $null, 4)) { $queued++ }
                }
            if ([OctoFileOps]::MoveFileEx('C:\ProgramData\OctoPOS', $null, 4)) { $queued++ }
            Warn "$queued entradas encoladas para borrar al proximo reboot."
            Warn 'Reinicia Windows cuando puedas y la limpieza se completa sola.'
        } catch {
            Warn 'No pude encolar el borrado diferido. Reiniciar Windows + reejecutar.'
        }
    }
} else {
    Skip 'No existia.'
}

# --- WSL ---

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
        # la pipeline de PowerShell - asi no tenemos que escribir un
        # archivo intermedio en el host.
        $bash = @'
set +e
echo "=> Bajando stack del admin..."
if [ -f /opt/octopos/docker-compose.yml ]; then
    cd /opt/octopos
    docker compose down -v 2>/dev/null
    echo "   stack bajado"
else
    echo "   /opt/octopos no existia"
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
    echo "   no habia"
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
Write-Host '------------------------------------------------------------' -ForegroundColor Green
Write-Host ' Limpieza completa.'                                          -ForegroundColor Green
Write-Host ' El equipo esta virgen para probar el bootstrapper de nuevo.' -ForegroundColor Green
Write-Host '------------------------------------------------------------' -ForegroundColor Green
Write-Host ''
Write-Host '  Proximo paso:'
Write-Host '    1. Baja el ultimo OctoPOS-Setup-vX.Y.Z.exe de:'
Write-Host '       https://github.com/aarratia25/octoPOS-releases/releases/latest'
Write-Host '    2. Doble-click. Acepta el UAC.'
Write-Host '    3. Mira el panel de log mientras instala.'
Write-Host ''
