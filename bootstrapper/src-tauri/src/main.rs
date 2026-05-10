// One-click bootstrapper for OctoPOS. The window opens on a tiny
// two-field form (Sucursal + Clave de la plataforma) that the IT
// fills once; everything from there is automatic — WSL2, Ubuntu,
// Docker, Mongo, the API container and the OctoPOS Admin .msi all
// install themselves while a progress bar slides.
//
// On non-Windows the binary is a no-op so the workspace still
// `cargo check`s on the dev machine.

#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use serde::Serialize;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};
#[cfg(windows)]
use tauri::Manager;

mod telemetry;
use telemetry::{Outcome as TelemetryOutcome, TelemetryContext};

#[derive(Clone, Serialize)]
struct ProgressEvent {
    percent: u8,
    message: String,
}

#[derive(Clone, Serialize)]
struct ErrorEvent {
    message: String,
}

#[allow(dead_code)] // Only constructed under cfg(windows); silences the lint on Linux/CI checks.
#[derive(Clone, Serialize)]
struct LogLineEvent {
    stream: &'static str,
    line: String,
}

/// Guards the install pipeline from being started twice (e.g. impatient
/// double click on the submit button) — the form keeps Submit disabled
/// while we run, so this is belt-and-suspenders.
#[derive(Default)]
struct InstallState {
    started: Mutex<bool>,
}

/// Shared across the install pipeline. Carries the installId +
/// hwFingerprint + accumulated state used by every periodic upload.
/// Set once at the top of run_install() and read by progress() +
/// the final upload at handle.exit time.
#[cfg(windows)]
static TELEMETRY: std::sync::OnceLock<Arc<TelemetryContext>> = std::sync::OnceLock::new();

fn main() {
    // Self-elevate before Tauri spins up. Embedding a manifest in the
    // exe (the "right" way) collides with the one tauri-build already
    // ships (`CVT1100: duplicate resource type:MANIFEST`), so we
    // detect elevation at runtime and re-launch ourselves with the
    // `runas` verb when needed. UAC fires once, before any window is
    // painted, exactly like the Discord / NVIDIA Experience pattern.
    #[cfg(windows)]
    {
        if !is_elevated() {
            let _ = relaunch_as_admin();
            return;
        }
    }

    tauri::Builder::default()
        .manage(InstallState::default())
        .invoke_handler(tauri::generate_handler![
            start_install,
            open_setup_log,
            finalize_install,
        ])
        .run(tauri::generate_context!())
        .expect("error while running OctoPOS bootstrapper");
}

/// Probes the current process token by calling `net session`, which
/// the OS gates on local administrator privileges. A non-zero exit
/// (or spawn failure) means we are running unelevated. We could call
/// `OpenProcessToken` + `GetTokenInformation` for the same answer,
/// but that drags `windows-sys` into the dependency tree just for
/// this one boolean — the `net` shellout is good enough and adds no
/// new deps. CREATE_NO_WINDOW prevents a console flash.
#[cfg(windows)]
fn is_elevated() -> bool {
    silent_command("net")
        .args(["session"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Re-launches the current binary through `Start-Process -Verb RunAs`,
/// which triggers the consent prompt and starts the elevated copy.
/// We deliberately drop our own process the moment we kick off the
/// re-launch — the elevated child takes over from here. If the user
/// clicks No on UAC, no window appears at all, which is the expected
/// behaviour for a stub installer.
#[cfg(windows)]
fn relaunch_as_admin() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    // Quote-escape any single quotes in the path (rare but possible)
    // so the PowerShell argument stays well-formed.
    let escaped = exe.to_string_lossy().replace('\'', "''");
    let ps_command = format!("Start-Process -FilePath '{escaped}' -Verb RunAs");
    silent_command("powershell")
        .args(["-NoProfile", "-Command", &ps_command])
        .spawn()
        .map_err(|e| format!("spawn elevated child: {e}"))?;
    Ok(())
}

/// Final hand-off invoked by the splash when the operator clicks
/// "Abrir OctoPOS Admin" after install completes. Launches the admin
/// (so the user sees the activation screen immediately), schedules
/// the bootstrapper's own NSIS uninstaller, then exits this process.
/// Doing the whole sequence here — instead of from a timer at the
/// end of run_install — guarantees that:
///   - The admin is launched after the user explicitly asked for it
///     (no surprising "ya se abrió solo").
///   - The bootstrapper exits while the user is looking at the
///     admin window, so the cleanup .bat sees the .exe gone within
///     its first poll iteration instead of racing a 5s sleep.
#[tauri::command]
fn finalize_install(handle: AppHandle) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = launch_admin();
        let _ = schedule_self_uninstall();
    }
    handle.exit(0);
    Ok(())
}

/// Lets the splash open `%ProgramData%\OctoPOS\setup.log` in the user's
/// default editor (Notepad). The error banner has an "Abrir log" link
/// that calls into this so the operator can read the transcript with
/// one click instead of pasting the path into File Explorer.
#[tauri::command]
fn open_setup_log() -> Result<(), String> {
    #[cfg(windows)]
    {
        let path = setup_log_path();
        if !path.exists() {
            return Err(format!("{} no existe (todavia).", path.display()));
        }
        silent_command("notepad")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("notepad: {e}"))?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        Err("Solo Windows.".to_string())
    }
}

/// JS fires this once when the window finishes painting. The pipeline
/// is fire-and-forget: we spawn the worker thread and return so the
/// UI stays responsive. Subsequent invocations are ignored — the
/// install lock guarantees a single run per process.
///
/// The pipeline takes no arguments. License key, branch and role are
/// captured later by the OctoPOS Admin's own activation screen — the
/// bootstrapper only owns the system-level setup (WSL, Docker, the
/// containers, the .msi, the companion service).
#[tauri::command]
fn start_install(
    handle: AppHandle,
    state: State<'_, InstallState>,
) -> Result<(), String> {
    {
        let mut started = state
            .started
            .lock()
            .map_err(|e| format!("lock poisoned: {e}"))?;
        if *started {
            return Ok(());
        }
        *started = true;
    }

    let h = handle.clone();
    std::thread::spawn(move || {
        // Tiny grace period to let the window paint.
        std::thread::sleep(std::time::Duration::from_millis(120));
        // Initialize telemetry once for this install attempt. Any
        // subsequent progress() ticks reuse the same installId and
        // hwFingerprint, so the platform sees coherent rows.
        #[cfg(windows)]
        {
            let ctx = Arc::new(TelemetryContext::new());
            let _ = TELEMETRY.set(ctx);
        }
        let result = run_install(&h);
        #[cfg(windows)]
        if let Some(ctx) = TELEMETRY.get() {
            match &result {
                Ok(()) => ctx.upload(
                    TelemetryOutcome::Success,
                    None,
                    None,
                    &setup_log_path(),
                    true,
                ),
                Err(e) => ctx.upload(
                    TelemetryOutcome::Error,
                    Some("run_install"),
                    Some(e),
                    &setup_log_path(),
                    true,
                ),
            }
        }
        if let Err(e) = result {
            let _ = h.emit("setup-error", ErrorEvent { message: e });
        }
    });
    Ok(())
}

#[cfg(windows)]
fn run_install(handle: &AppHandle) -> Result<(), String> {
    progress(handle, 2, "Verificando requisitos del sistema...");
    pre_check_system().map_err(|e| e.to_string())?;

    progress(handle, 8, "Descomprimiendo recursos...");
    let resource_dir = handle
        .path()
        .resource_dir()
        .map_err(|e| format!("resource_dir: {e}"))?;
    let embedded = resource_dir.join("embedded");

    progress(handle, 15, "Preparando entorno...");
    let need_reboot = ensure_wsl(handle).map_err(|e| e.to_string())?;
    if need_reboot {
        progress(handle, 25, "Configurando reanudacion despues del reboot...");
        register_runonce(&resource_dir).map_err(|e| e.to_string())?;
        progress(handle, 30, "Reiniciando equipo...");
        silent_command("shutdown")
            .args(["/r", "/t", "5"])
            .status()
            .map_err(|e| format!("shutdown: {e}"))?;
        return Ok(());
    }

    progress(handle, 40, "Instalando dependencias...");
    run_silent_installer(handle, &embedded).map_err(|e| e.to_string())?;

    progress(handle, 70, "Instalando base de datos y servidor...");
    wait_for_api_health(handle).map_err(|e| e.to_string())?;

    progress(handle, 85, "Descargando e instalando el panel...");
    install_admin_msi(&embedded).map_err(|e| e.to_string())?;

    progress(handle, 90, "Registrando servicio de actualizaciones...");
    register_companion_service(&embedded).map_err(|e| e.to_string())?;

    progress(handle, 94, "Configurando arranque automatico al boot...");
    let _ = register_wsl_autostart();

    // Note: we do NOT create a desktop shortcut here. The admin .msi
    // already creates one with the correct icon and target. Adding a
    // second one from the bootstrapper led to a duplicate "OctoPOS
    // Admin" entry on the desktop with the wrong target path
    // (Cargo binary name `octopos-admin.exe` vs productName-based
    // guess), and Windows showed "Missing Shortcut" when the user
    // clicked it. Trust the MSI installer to own its own shortcut.

    progress(handle, 100, "Listo. OctoPOS esta instalado.");
    // Hand control to the user via the splash UI. The JS listens for
    // `setup-complete`, swaps the progress card for an "Abrir OctoPOS
    // Admin" button, and only invokes `finalize_install` (which
    // launches the admin + auto-uninstalls the bootstrapper) once
    // the user clicks. No more racey sleeps — the bootstrapper
    // process is guaranteed to be ready to die because the user
    // explicitly told us to.
    let _ = handle.emit("setup-complete", ());
    Ok(())
}

#[cfg(not(windows))]
fn run_install(handle: &AppHandle) -> Result<(), String> {
    progress(handle, 0, "Solo Windows soportado.");
    Err("This bootstrapper only runs on Windows.".to_string())
}

fn progress(handle: &AppHandle, percent: u8, message: &str) {
    log::info!("[{}%] {}", percent, message);
    // Periodic telemetry upload — throttled internally to one per
    // 30s so calling it on every step is cheap. Best-effort: any
    // network failure is swallowed inside upload().
    #[cfg(windows)]
    if let Some(ctx) = TELEMETRY.get() {
        ctx.upload(
            TelemetryOutcome::InProgress,
            None,
            None,
            &setup_log_path(),
            false,
        );
    }
    let _ = handle.emit(
        "setup-progress",
        ProgressEvent {
            percent,
            message: message.to_string(),
        },
    );
}

// --- Windows-only helpers -------------------------------------------------

// CREATE_NO_WINDOW prevents Windows from spawning a console window for
// every CLI child the bootstrapper invokes (powershell, wsl, curl,
// schtasks, sc, msiexec, reg). Without it, each of those flashes a
// console behind the splash, which (a) looks unprofessional and
// (b) confused users who closed them and broke the pipeline.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(windows)]
fn silent_command(program: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Runs a command, captures stdout + stderr line-by-line and:
///   1. Emits each line as a `setup-log` Tauri event so the splash
///      can render a live console panel.
///   2. Appends each line to %ProgramData%\OctoPOS\setup.log so the
///      operator (and remote support) can read the full transcript
///      after the fact, even if the splash window is gone.
///
/// Returns the child's exit code on completion. Errors out if we
/// cannot spawn or pipe; the caller decides how to interpret a
/// non-zero exit.
#[cfg(windows)]
fn run_streaming(
    handle: &AppHandle,
    mut cmd: std::process::Command,
) -> Result<std::process::ExitStatus, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::thread;

    let log_path = setup_log_path();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("no pude abrir {log_path:?}: {e}"))?;

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;

    let stdout = child.stdout.take().ok_or("missing stdout pipe")?;
    let stderr = child.stderr.take().ok_or("missing stderr pipe")?;

    fn pump<R: std::io::Read + Send + 'static>(
        reader: R,
        stream: &'static str,
        handle: AppHandle,
        mut sink: std::fs::File,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let buf = BufReader::new(reader);
            for line in buf.lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => break,
                };
                let _ = writeln!(sink, "[{stream}] {line}");
                let _ = handle.emit(
                    "setup-log",
                    LogLineEvent {
                        stream,
                        line,
                    },
                );
            }
        })
    }

    let log_for_out = log_file
        .try_clone()
        .map_err(|e| format!("clone log: {e}"))?;
    let log_for_err = log_file;
    let h_out = handle.clone();
    let h_err = handle.clone();
    let t_out = pump(stdout, "stdout", h_out, log_for_out);
    let t_err = pump(stderr, "stderr", h_err, log_for_err);

    let status = child.wait().map_err(|e| format!("wait: {e}"))?;
    let _ = t_out.join();
    let _ = t_err.join();
    Ok(status)
}

#[cfg(windows)]
fn pre_check_system() -> Result<(), String> {
    let out = silent_command("cmd")
        .args(["/c", "ver"])
        .output()
        .map_err(|e| format!("ver: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let build = text
        .split('.')
        .nth(2)
        .and_then(|s| s.split(']').next())
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    if build < 19041 {
        return Err(format!(
            "Windows 10 build 19041+ requerido (encontrado {build})"
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn ensure_wsl(handle: &AppHandle) -> Result<bool, String> {
    let wsl_present = silent_command("where")
        .arg("wsl")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !wsl_present {
        let mut cmd = silent_command("wsl");
        cmd.args(["--install", "--no-distribution"]);
        let status = run_streaming(handle, cmd)?;
        if !status.success() {
            return Err(format!(
                "wsl --install fallo con codigo {}",
                status.code().unwrap_or(-1)
            ));
        }
        return Ok(true);
    }
    Ok(false)
}

#[cfg(windows)]
fn run_silent_installer(
    handle: &AppHandle,
    embedded: &std::path::Path,
) -> Result<(), String> {
    let ps1 = embedded.join("install-silent.ps1");
    if !ps1.exists() {
        return Err(format!("install-silent.ps1 no encontrado en {ps1:?}"));
    }

    // Tenant data (license key, branch, role) is captured by the
    // OctoPOS Admin's own activation screen the first time it
    // launches — the bootstrapper does not need any of that. We only
    // pass machine-local secrets that the script auto-generates if
    // empty (Mongo password, JWT secret) and the canonical platform
    // URL so the API can phone home once the operator activates.
    let mut cmd = silent_command("powershell");
    cmd.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        ps1.to_str().unwrap_or(""),
    ])
    .env("OCTOPOS_PLATFORM_URL", "https://platform.octo-pos.net");

    let log_path = setup_log_path();
    let status = run_streaming(handle, cmd)?;

    match status.code() {
        Some(0) => Ok(()),
        Some(3) => Err("__REBOOT_REQUIRED__".to_string()),
        Some(c) => Err(format!(
            "install-silent.ps1 fallo con codigo {c}. Detalles en {}",
            log_path.display()
        )),
        None => Err(format!(
            "install-silent.ps1 termino sin codigo de salida. Detalles en {}",
            log_path.display()
        )),
    }
}

#[cfg(windows)]
fn setup_log_path() -> std::path::PathBuf {
    if let Some(programdata) = std::env::var_os("ProgramData") {
        let mut p = std::path::PathBuf::from(programdata);
        p.push("OctoPOS");
        p.push("setup.log");
        return p;
    }
    std::path::PathBuf::from(r"C:\ProgramData\OctoPOS\setup.log")
}

#[cfg(windows)]
fn wait_for_api_health(handle: &AppHandle) -> Result<(), String> {
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(180);
    while Instant::now() < deadline {
        let out = silent_command("curl")
            .args(["-fsS", "http://localhost:3000/"])
            .output();
        if let Ok(o) = out {
            let body = String::from_utf8_lossy(&o.stdout);
            if body.contains("\"ok\":true") {
                progress(handle, 80, "Servidor listo");
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    Err("El servidor API no respondio dentro de 3 minutos.".to_string())
}

#[cfg(windows)]
fn install_admin_msi(_embedded: &std::path::Path) -> Result<(), String> {
    // Pull metadata for the latest published admin release from the
    // public mirror repo. We hit the GitHub REST API with curl
    // (ships with Windows 10 1803+), parse the JSON with serde, and
    // pick the first NSIS .exe asset (NOT the .msi).
    //
    // We deliberately install the NSIS `-setup.exe` rather than the
    // .msi because Tauri's NSIS template honours `installerHooks`
    // (we use them to taskkill octopos-admin.exe before
    // (un)install). MSI uninstalls from Control Panel just delete
    // files without ever asking the running process to close, so the
    // operator was left with a "ghost admin" — binary gone from
    // disk, process still alive in RAM. The NSIS hooks fix that
    // case AND the upgrade-while-running case.
    //
    // Doing the lookup at install time means an old bootstrapper
    // still installs whatever the latest release says is current.
    let api_url = "https://api.github.com/repos/aarratia25/octoPOS-releases/releases/latest";
    let metadata_out = silent_command("curl")
        .args([
            "-fsSL",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: OctoPOS-Setup",
            api_url,
        ])
        .output()
        .map_err(|e| format!("curl spawn: {e}"))?;
    if !metadata_out.status.success() {
        return Err(format!(
            "No se pudo consultar releases de GitHub (codigo {}).",
            metadata_out.status.code().unwrap_or(-1)
        ));
    }

    let metadata: serde_json::Value = serde_json::from_slice(&metadata_out.stdout)
        .map_err(|e| format!("parse releases JSON: {e}"))?;
    let assets = metadata
        .get("assets")
        .and_then(|a| a.as_array())
        .ok_or_else(|| "Respuesta de GitHub sin 'assets'.".to_string())?;
    // Pick the admin NSIS installer — name pattern looks like
    // "OctoPOS.Admin_X.Y.Z_x64-setup.exe". Filter on `setup.exe`
    // suffix so we don't pick up the bootstrapper "OctoPOS-Setup-*".
    let exe_asset = assets
        .iter()
        .find(|a| {
            a.get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|n| {
                    let lower = n.to_lowercase();
                    lower.contains("admin")
                        && lower.ends_with("setup.exe")
                })
        })
        .ok_or_else(|| "El ultimo release no tiene un -setup.exe del admin.".to_string())?;
    let exe_url = exe_asset
        .get("browser_download_url")
        .and_then(|u| u.as_str())
        .ok_or_else(|| "Asset -setup.exe sin browser_download_url.".to_string())?;
    let exe_name = exe_asset
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("OctoPOS.Admin-setup.exe");

    let temp_dir = std::env::temp_dir();
    let temp_exe = temp_dir.join(exe_name);

    let download_status = silent_command("curl")
        .args([
            "-fsSL",
            "-H",
            "User-Agent: OctoPOS-Setup",
            "-o",
            temp_exe.to_str().unwrap_or(""),
            exe_url,
        ])
        .status()
        .map_err(|e| format!("curl download: {e}"))?;
    if !download_status.success() {
        return Err(format!(
            "Descarga del .exe fallo (codigo {}).",
            download_status.code().unwrap_or(-1)
        ));
    }

    // /S = NSIS silent mode; same flag the auto-update path will use.
    let install_status = silent_command(temp_exe.to_str().unwrap_or(""))
        .arg("/S")
        .status()
        .map_err(|e| format!("admin installer spawn: {e}"))?;
    if !install_status.success() {
        return Err(format!(
            "Admin installer termino con codigo {}",
            install_status.code().unwrap_or(-1)
        ));
    }

    // Best-effort cleanup; if the file lingers in %TEMP% Windows
    // cleans it on its own at the next disk-cleanup pass.
    let _ = std::fs::remove_file(&temp_exe);

    Ok(())
}

#[cfg(windows)]
fn register_companion_service(embedded: &std::path::Path) -> Result<(), String> {
    let bundled = embedded.join("octopos-updater.exe");
    if !bundled.exists() {
        return Err(format!("octopos-updater.exe no encontrado en {bundled:?}"));
    }

    // Copy the companion binary out of `C:\Program Files\OctoPOS Setup\
    // embedded\` (managed by the bootstrapper's NSIS uninstaller) into a
    // standalone location so the bootstrapper can self-uninstall later
    // without taking the running service down with it. Without this
    // copy, NSIS hits "Error abriendo archivo para escritura" the next
    // time the user runs an updated bootstrapper because the old
    // octopos-updater.exe is still being executed by the SCM from the
    // exact path NSIS wants to overwrite.
    let install_root = std::path::PathBuf::from(
        std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".to_string()),
    )
    .join("OctoPOS Updater");
    std::fs::create_dir_all(&install_root)
        .map_err(|e| format!("create_dir_all updater root: {e}"))?;
    let exe = install_root.join("octopos-updater.exe");
    // Stop+delete the service before swapping the file so we don't try
    // to overwrite a binary the SCM is still running. Idempotent — if
    // the service does not exist these calls are no-ops.
    let _ = silent_command("sc")
        .args(["stop", "OctoPOSUpdater"])
        .status();
    let _ = silent_command("sc")
        .args(["delete", "OctoPOSUpdater"])
        .status();
    // Brief grace period for the SCM to release the binary handle.
    std::thread::sleep(std::time::Duration::from_millis(500));
    std::fs::copy(&bundled, &exe).map_err(|e| format!("copy updater binary: {e}"))?;

    let secret = generate_secret()?;
    let _ = silent_command("reg")
        .args([
            "add",
            r"HKLM\Software\OctoPOS",
            "/v",
            "UpdaterSecret",
            "/t",
            "REG_SZ",
            "/d",
            &secret,
            "/f",
        ])
        .status()
        .map_err(|e| format!("reg add: {e}"))?;

    if let Some(programdata) = std::env::var_os("ProgramData") {
        let mut p = std::path::PathBuf::from(programdata);
        p.push("OctoPOS");
        let _ = std::fs::create_dir_all(&p);
        p.push("updater-secret");
        std::fs::write(&p, &secret).map_err(|e| format!("write secret: {e}"))?;
    }

    let create = silent_command("sc")
        .args([
            "create",
            "OctoPOSUpdater",
            "binPath=",
            exe.to_str().unwrap_or(""),
            "start=",
            "auto",
            "DisplayName=",
            "OctoPOS Updater Service",
        ])
        .status();
    if let Ok(s) = create {
        if !s.success() && s.code() != Some(1073) {
            return Err(format!(
                "sc create fallo con codigo {}",
                s.code().unwrap_or(-1)
            ));
        }
    }
    let _ = silent_command("sc")
        .args(["start", "OctoPOSUpdater"])
        .status();
    Ok(())
}

#[cfg(windows)]
fn generate_secret() -> Result<String, String> {
    // BCryptGenRandom under the hood — works in any process state
    // (admin or not), no special handles, no /dev/urandom dance.
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).map_err(|e| format!("getrandom: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Resolves where the OctoPOS Admin binary actually lives. Tauri's
/// MSI installs in `Program Files\<productName>\` (productName is
/// "OctoPOS Admin", from tauri.conf.json), but the executable inside
/// is named after the **Cargo package name** (`octopos-admin.exe`),
/// not the productName. Earlier versions of this bootstrapper hard-
/// coded `OctoPOS Admin.exe`, which doesn't exist, so launching at
/// the end of the install raised "Windows cannot find ..." and the
/// operator was left with the admin installed but no entry point.
///
/// We probe a small list of candidates so a future rename of the
/// Cargo binary (or the productName) doesn't silently break this
/// code path again. First match wins.
#[cfg(windows)]
fn admin_exe_path() -> std::path::PathBuf {
    let program_files = std::env::var("ProgramFiles")
        .unwrap_or_else(|_| r"C:\Program Files".to_string());
    let install_dir = std::path::PathBuf::from(&program_files).join("OctoPOS Admin");
    let candidates = [
        "octopos-admin.exe", // current — Cargo `[package] name`
        "OctoPOS Admin.exe", // legacy — productName-based fallback
    ];
    for c in &candidates {
        let p = install_dir.join(c);
        if p.exists() {
            return p;
        }
    }
    install_dir.join(candidates[0])
}

#[cfg(windows)]
fn launch_admin() -> Result<(), String> {
    let exe = admin_exe_path();
    if !exe.exists() {
        return Err(format!(
            "El admin no se encontro en {}. La instalacion del .msi quizas fallo en silencio.",
            exe.display()
        ));
    }
    // Spawn the admin directly (no `cmd /c start` shenanigans —
    // those briefly flash a console and confuse Windows when the
    // path doesn't resolve). silent_command keeps the inheritance
    // policy consistent with the rest of the pipeline.
    silent_command(exe.to_str().unwrap_or(""))
        .spawn()
        .map_err(|e| format!("no pude lanzar el admin: {e}"))?;
    Ok(())
}

#[cfg(windows)]
fn register_wsl_autostart() -> Result<(), String> {

    // Register a Windows Scheduled Task that fires "at startup" (no
    // interactive login required) and runs `wsl --exec /bin/true`.
    // That single call wakes the Ubuntu distro in the background,
    // which in turn boots systemd, which in turn starts dockerd and
    // every container with `restart: unless-stopped`. Without this
    // task, the API only comes back when an operator opens a WSL
    // shell — defeating the whole point of unattended recovery.
    //
    // Run as SYSTEM with HIGHEST privileges so the task survives
    // user account changes and does not pop UAC. /f overwrites any
    // pre-existing task with the same name (idempotent re-runs).
    let status = silent_command("schtasks")
        .args([
            "/create",
            "/tn",
            "OctoPOS WSL Autostart",
            "/tr",
            "wsl -d Ubuntu-22.04 --exec /bin/true",
            "/sc",
            "onstart",
            "/ru",
            "SYSTEM",
            "/rl",
            "HIGHEST",
            "/f",
        ])
        .status()
        .map_err(|e| format!("schtasks spawn: {e}"))?;
    if !status.success() {
        return Err(format!(
            "schtasks /create fallo con codigo {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn schedule_self_uninstall() -> Result<(), String> {
    use std::io::Write;
    use std::os::windows::process::CommandExt;

    // Write a tiny .bat to %TEMP% that:
    //   1. Waits ~5s so the bootstrapper process can fully exit
    //      (NSIS will refuse to remove a binary that is still running).
    //   2. Looks up the uninstaller via the Add/Remove Programs entry
    //      keyed by DisplayName, so we don't hardcode an install path.
    //      Tauri/NSIS writes that key with productName from
    //      tauri.conf.json — match it exactly.
    //   3. Runs the uninstaller in silent mode (`/S`).
    //   4. Deletes the .bat itself so nothing stays behind.
    //
    // Every command silences its own output (>nul 2>&1) and the
    // PowerShell window is hidden — earlier versions surfaced an
    // empty cmd window mid-shutdown saying "Not enough memory
    // resources are available to process this command" because the
    // child cmd's STDIO inherited a broken handle from the dying
    // bootstrapper. With redirected I/O and a hidden window, the
    // cleanup is invisible to the user.
    //
    // Failures here are best-effort — if anything goes wrong the worst
    // case is the user still sees "OctoPOS Setup" in Programs and can
    // remove it by hand. We never let this break the install flow.
    // Earlier attempts (deferred .bat / .ps1 with admin inheritance)
    // failed in the field: the child loses the admin token the
    // moment the bootstrapper exit()s, and NSIS uninstall silently
    // bails because it cannot write to Program Files without it.
    //
    // The reliable path is to delegate the uninstall to a Windows
    // Scheduled Task running as SYSTEM. The Task Scheduler is a
    // service running with full kernel privileges; every command it
    // launches inherits SYSTEM token, so NSIS gets every permission
    // it needs (Program Files writes, registry HKLM, public
    // shortcuts) without a single UAC prompt.
    //
    // Flow:
    //   1. Write a .ps1 that polls until octopos-bootstrapper.exe
    //      is gone, runs the NSIS uninstaller silently, then deletes
    //      both the scheduled task entry and itself.
    //   2. Create a one-time scheduled task to fire 1 minute from
    //      now under SYSTEM that runs the .ps1 hidden.
    //   3. Exit the bootstrapper. By the time the task fires, the
    //      .exe is gone and NSIS can do its job.
    let ps1_path = std::env::temp_dir().join("octopos-setup-cleanup.ps1");
    // Logging built-in so the operator (and us) can pinpoint why a
    // failed cleanup happened. Every step appends to
    // %ProgramData%\OctoPOS\uninstall-log.txt — survives the
    // .ps1's self-delete and gives a clear rcause to a support
    // engineer who runs the diagnostic later.
    let ps1_body = r#"$ErrorActionPreference = 'Continue'
$logDir = Join-Path $env:ProgramData 'OctoPOS'
if (-not (Test-Path $logDir)) { New-Item -ItemType Directory -Force -Path $logDir | Out-Null }
$logPath = Join-Path $logDir 'uninstall-log.txt'
function L($msg) { "$([DateTime]::UtcNow.ToString('o')) $msg" | Add-Content -LiteralPath $logPath }

L "=== cleanup ps1 starting; user=$env:USERNAME pid=$PID ==="

# Wait for the bootstrapper to die so NSIS can replace its files.
$waited = 0
while (Get-Process -Name 'octopos-bootstrapper' -ErrorAction SilentlyContinue) {
    if ($waited -gt 60) { L "Bootstrapper still alive after 60s — bailing"; break }
    Start-Sleep -Seconds 1
    $waited++
}
L "Bootstrapper waited $waited s"
Start-Sleep -Seconds 2

try {
    $entry = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue |
        Where-Object { $_.DisplayName -eq 'OctoPOS Setup' } |
        Select-Object -First 1
    if (-not $entry) { L "No ARP entry for 'OctoPOS Setup' — already gone?" }
    else {
        $u = $entry.UninstallString -replace '^"|"$', ''
        L "Invoking NSIS uninstaller: $u /S"
        $p = Start-Process -FilePath $u -ArgumentList '/S' -Wait -PassThru -ErrorAction Stop
        L "NSIS uninstall finished with exit code $($p.ExitCode)"
    }
} catch {
    L "Uninstall threw: $($_.Exception.Message)"
}

# Best-effort cleanup of leftovers NSIS might miss.
foreach ($shortcut in @(
    (Join-Path $env:PUBLIC 'Desktop\OctoPOS Setup.lnk'),
    (Join-Path $env:USERPROFILE 'Desktop\OctoPOS Setup.lnk')
)) {
    if (Test-Path $shortcut) {
        Remove-Item -Force $shortcut -ErrorAction SilentlyContinue
        L "Removed stray shortcut: $shortcut"
    }
}

L "Removing scheduled task and self"
schtasks /delete /tn 'OctoPOSSetupCleanup' /f *> $null
Remove-Item -Force $MyInvocation.MyCommand.Path -ErrorAction SilentlyContinue
L "=== cleanup ps1 done ==="
"#;
    let mut f = std::fs::File::create(&ps1_path)
        .map_err(|e| format!("create cleanup ps1: {e}"))?;
    f.write_all(ps1_body.as_bytes())
        .map_err(|e| format!("write cleanup ps1: {e}"))?;
    drop(f);

    // schtasks.exe + HH:mm time arg has nasty edge cases (the time
    // wraps at midnight, the parser is locale-sensitive, the /tr
    // argument double-quoting is broken on long paths with spaces).
    // Use the modern PowerShell scheduled-task cmdlets instead —
    // they take strongly-typed Trigger / Action / Principal objects
    // and don't depend on string formatting. Also write a marker file
    // so we know from the diagnostic whether the schedule call
    // even completed.
    let marker_path = std::env::temp_dir().join("octopos-cleanup-scheduled.txt");
    let ps1_for_register = ps1_path.to_string_lossy().replace('\'', "''");
    let marker_for_register = marker_path.to_string_lossy().replace('\'', "''");
    let schedule_command = format!(
        "$ErrorActionPreference = 'Stop'; \
         $argLine = '-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File \"' + '{ps1_for_register}' + '\"'; \
         $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument $argLine; \
         $trigger = New-ScheduledTaskTrigger -Once -At ((Get-Date).AddSeconds(45)); \
         $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest; \
         $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -DeleteExpiredTaskAfter ([TimeSpan]::FromMinutes(5)); \
         Register-ScheduledTask -TaskName 'OctoPOSSetupCleanup' -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null; \
         Set-Content -LiteralPath '{marker_for_register}' -Value ('scheduled at ' + [DateTime]::UtcNow.ToString('o'))"
    );
    let status = silent_command("powershell")
        .args(["-NoProfile", "-Command", &schedule_command])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("schedule task: {e}"))?;
    if !status.success() {
        return Err(format!(
            "Register-ScheduledTask exited with code {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn register_runonce(_resource_dir: &std::path::Path) -> Result<(), String> {
    let exe_path = std::env::current_exe()
        .map_err(|e| format!("current_exe: {e}"))?;
    let status = silent_command("reg")
        .args([
            "add",
            r"HKLM\Software\Microsoft\Windows\CurrentVersion\RunOnce",
            "/v",
            "OctoPOSBootstrapResume",
            "/t",
            "REG_SZ",
            "/d",
            exe_path.to_str().unwrap_or(""),
            "/f",
        ])
        .status()
        .map_err(|e| format!("reg add: {e}"))?;
    if !status.success() {
        return Err(format!(
            "RunOnce registration fallo con codigo {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}
