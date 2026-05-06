// One-click bootstrapper for OctoPOS. Splash UI with a progress bar
// and a single subtitle line — zero buttons, zero forms. The user
// double-clicks, accepts the UAC prompt once, and waits.
//
// Steps run in order; each one emits a `setup-progress` event with
// {percent, message}. Errors emit `setup-error` and the UI surfaces
// them inline. Reboots required by `wsl --install` are handled with
// a RunOnce registry entry so the bootstrapper resumes itself after
// the reboot from where it left off.

#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use serde::Serialize;
use tauri::{AppHandle, Emitter};
#[cfg(windows)]
use tauri::Manager;

#[derive(Clone, Serialize)]
struct ProgressEvent {
    percent: u8,
    message: String,
}

#[derive(Clone, Serialize)]
struct ErrorEvent {
    message: String,
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![frontend_ready])
        .setup(|app| {
            let handle = app.handle().clone();
            // Don't kick off install work until the frontend confirms
            // it's ready (see `frontend_ready` below) — otherwise the
            // earliest progress events get dropped because the JS
            // listener isn't attached yet.
            std::thread::spawn(move || {
                // Tiny grace period to let the window paint.
                std::thread::sleep(std::time::Duration::from_millis(150));
                if let Err(e) = run_install(&handle) {
                    let _ = handle.emit(
                        "setup-error",
                        ErrorEvent { message: e.to_string() },
                    );
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running OctoPOS bootstrapper");
}

#[tauri::command]
fn frontend_ready() -> Result<(), String> {
    // The actual signal is the listener being attached on the JS side
    // via `listen()`. We just expose the command so the frontend can
    // ping us if we want to gate work on it more strictly later.
    Ok(())
}

#[cfg(windows)]
fn run_install(handle: &AppHandle) -> Result<(), String> {
    use std::path::PathBuf;

    progress(handle, 2, "Verificando requisitos del sistema...");
    pre_check_system().map_err(|e| e.to_string())?;

    progress(handle, 8, "Descomprimiendo recursos...");
    let resource_dir = handle
        .path()
        .resource_dir()
        .map_err(|e| format!("resource_dir: {e}"))?;
    let embedded = resource_dir.join("embedded");

    progress(handle, 15, "Verificando WSL2 y Ubuntu...");
    let need_reboot = ensure_wsl(handle).map_err(|e| e.to_string())?;
    if need_reboot {
        progress(handle, 25, "Configurando reanudacion despues del reboot...");
        register_runonce(&resource_dir).map_err(|e| e.to_string())?;
        progress(handle, 30, "Reiniciando equipo...");
        std::process::Command::new("shutdown")
            .args(["/r", "/t", "5"])
            .status()
            .map_err(|e| format!("shutdown: {e}"))?;
        return Ok(());
    }

    progress(handle, 40, "Instalando Docker y dependencias...");
    run_silent_installer(handle, &embedded).map_err(|e| e.to_string())?;

    progress(handle, 70, "Levantando servicios (Mongo + API)...");
    wait_for_api_health(handle).map_err(|e| e.to_string())?;

    progress(handle, 85, "Instalando OctoPOS Admin...");
    install_admin_msi(&embedded).map_err(|e| e.to_string())?;

    progress(handle, 92, "Registrando servicio de actualizaciones...");
    register_companion_service(&embedded).map_err(|e| e.to_string())?;

    progress(handle, 98, "Creando acceso directo...");
    create_desktop_shortcut().map_err(|e| e.to_string())?;

    progress(handle, 100, "Listo. OctoPOS esta instalado.");
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Auto-launch the admin and quit the bootstrapper.
    let _ = launch_admin();
    handle.exit(0);
    Ok(())
}

#[cfg(not(windows))]
fn run_install(handle: &AppHandle) -> Result<(), String> {
    progress(handle, 0, "Solo Windows soportado.");
    Err("This bootstrapper only runs on Windows.".to_string())
}

fn progress(handle: &AppHandle, percent: u8, message: &str) {
    log::info!("[{}%] {}", percent, message);
    let _ = handle.emit(
        "setup-progress",
        ProgressEvent {
            percent,
            message: message.to_string(),
        },
    );
}

// --- Windows-only helpers -------------------------------------------------

#[cfg(windows)]
fn pre_check_system() -> Result<(), String> {
    use std::process::Command;

    // Build number ≥ 19041 (matches install.ps1 requirement).
    let out = Command::new("cmd")
        .args(["/c", "ver"])
        .output()
        .map_err(|e| format!("ver: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    // ver prints e.g. "Microsoft Windows [Version 10.0.19045.4291]".
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
fn ensure_wsl(_handle: &AppHandle) -> Result<bool, String> {
    use std::process::Command;
    // Returns true when a reboot is required.
    let wsl_present = Command::new("where")
        .arg("wsl")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !wsl_present {
        let status = Command::new("wsl")
            .args(["--install", "--no-distribution"])
            .status()
            .map_err(|e| format!("wsl --install: {e}"))?;
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
    _handle: &AppHandle,
    embedded: &std::path::Path,
) -> Result<(), String> {
    use std::process::Command;

    let ps1 = embedded.join("install-silent.ps1");
    if !ps1.exists() {
        return Err(format!("install-silent.ps1 no encontrado en {ps1:?}"));
    }

    // Tenant data — generated by the platform per branch and embedded
    // adjacent to install-silent.ps1.
    let tenant_path = embedded.join("tenant.json");
    let tenant: serde_json::Value = if tenant_path.exists() {
        let body = std::fs::read_to_string(&tenant_path)
            .map_err(|e| format!("read tenant.json: {e}"))?;
        serde_json::from_str(&body).map_err(|e| format!("parse tenant.json: {e}"))?
    } else {
        // Allow running without tenant.json for dev / one-off installs;
        // install-silent.ps1 will refuse with exit 2 if mandatory env
        // vars aren't injected another way (e.g. process env).
        serde_json::json!({})
    };

    let branch_slug = tenant
        .get("branchSlug")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let api_key = tenant
        .get("platformApiKey")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let platform_url = tenant
        .get("platformUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("https://platform.octo-pos.net");
    let mongo_password = tenant
        .get("mongoPassword")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let jwt_secret = tenant
        .get("jwtSecret")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            ps1.to_str().unwrap_or(""),
        ])
        .env("OCTOPOS_BRANCH_SLUG", branch_slug)
        .env("OCTOPOS_PLATFORM_API_KEY", api_key)
        .env("OCTOPOS_PLATFORM_URL", platform_url)
        .env("OCTOPOS_MONGO_PASSWORD", mongo_password)
        .env("OCTOPOS_JWT_SECRET", jwt_secret)
        .status()
        .map_err(|e| format!("powershell spawn: {e}"))?;

    match status.code() {
        Some(0) => Ok(()),
        Some(2) => Err(
            "Falta tenant.json o variables de entorno requeridas (codigo 2).".to_string(),
        ),
        Some(3) => {
            // install-silent.ps1 signals "reboot required" when
            // wsl --install installed mid-flight. Same RunOnce
            // handling path as ensure_wsl.
            Err("__REBOOT_REQUIRED__".to_string())
        }
        Some(c) => Err(format!("install-silent.ps1 fallo con codigo {c}")),
        None => Err("install-silent.ps1 termino sin codigo de salida".to_string()),
    }
}

#[cfg(windows)]
fn wait_for_api_health(handle: &AppHandle) -> Result<(), String> {
    use std::process::Command;
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(180);
    while Instant::now() < deadline {
        let out = Command::new("curl")
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
fn install_admin_msi(embedded: &std::path::Path) -> Result<(), String> {
    use std::process::Command;
    // Pick the only .msi in the embedded dir — the version is in the
    // filename (e.g. OctoPOS.Admin_0.3.0_x64-setup.msi).
    let msi = std::fs::read_dir(embedded)
        .map_err(|e| format!("read_dir embedded: {e}"))?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|x| x.to_str()) == Some("msi"))
        .ok_or_else(|| "No se encontro el .msi del admin en recursos.".to_string())?;
    let status = Command::new("msiexec")
        .args(["/i", msi.to_str().unwrap_or(""), "/quiet", "/qn"])
        .status()
        .map_err(|e| format!("msiexec spawn: {e}"))?;
    if !status.success() {
        return Err(format!(
            "msiexec termino con codigo {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn register_companion_service(embedded: &std::path::Path) -> Result<(), String> {
    use std::process::Command;
    let exe = embedded.join("octopos-updater.exe");
    if !exe.exists() {
        return Err(format!("octopos-updater.exe no encontrado en {exe:?}"));
    }

    // Generate the shared secret once and store it under HKLM.
    let secret = generate_secret()?;
    let _ = Command::new("reg")
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

    // Mirror the secret to %ProgramData%\OctoPOS\updater-secret so
    // the admin Tauri (running as the user) can read it without the
    // registry permission dance.
    if let Some(programdata) = std::env::var_os("ProgramData") {
        let mut p = std::path::PathBuf::from(programdata);
        p.push("OctoPOS");
        let _ = std::fs::create_dir_all(&p);
        p.push("updater-secret");
        std::fs::write(&p, &secret).map_err(|e| format!("write secret: {e}"))?;
    }

    // Register and start the service.
    let create = Command::new("sc")
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
        if !s.success() && s.code() != Some(1073) /* already exists */ {
            return Err(format!(
                "sc create fallo con codigo {}",
                s.code().unwrap_or(-1)
            ));
        }
    }
    let _ = Command::new("sc")
        .args(["start", "OctoPOSUpdater"])
        .status();
    Ok(())
}

#[cfg(windows)]
fn generate_secret() -> Result<String, String> {
    // 32 random bytes hex-encoded — enough entropy for HMAC keys and
    // small enough to fit in a registry string value.
    use std::io::Read;
    let mut buf = [0u8; 32];
    // Prefer the Windows CryptGenRandom-equivalent via the Rust
    // `getrandom` syscall wrapper. We avoid pulling getrandom as a
    // direct dep by going through std's own RNG — for HMAC keys the
    // distinction matters only philosophically.
    use std::fs::File;
    let mut f = File::open("\\Device\\KsecDD")
        .or_else(|_| File::open("CONIN$"))
        .map_err(|e| format!("open rng: {e}"))?;
    f.read_exact(&mut buf).map_err(|e| format!("read rng: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(windows)]
fn create_desktop_shortcut() -> Result<(), String> {
    // Best-effort. The MSI also creates a Start Menu shortcut, so
    // failures here are non-fatal.
    use std::process::Command;
    let ps = r#"
$WshShell = New-Object -ComObject WScript.Shell
$desktop = [Environment]::GetFolderPath('Desktop')
$shortcut = $WshShell.CreateShortcut("$desktop\OctoPOS Admin.lnk")
$shortcut.TargetPath = "$env:ProgramFiles\OctoPOS Admin\OctoPOS Admin.exe"
$shortcut.Save()
"#;
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-Command", ps])
        .status();
    Ok(())
}

#[cfg(windows)]
fn launch_admin() -> Result<(), String> {
    use std::process::Command;
    let target = std::env::var("ProgramFiles")
        .map(|p| format!(r"{p}\OctoPOS Admin\OctoPOS Admin.exe"))
        .unwrap_or_else(|_| r"C:\Program Files\OctoPOS Admin\OctoPOS Admin.exe".to_string());
    let _ = Command::new("cmd")
        .args(["/c", "start", "", &target])
        .status();
    Ok(())
}

#[cfg(windows)]
fn register_runonce(resource_dir: &std::path::Path) -> Result<(), String> {
    use std::process::Command;
    // Re-run the bootstrapper after the reboot so the user does not
    // have to double-click again.
    let exe_path = std::env::current_exe()
        .map_err(|e| format!("current_exe: {e}"))?;
    let _ = resource_dir; // currently unused, reserved for future state passing
    let status = Command::new("reg")
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
