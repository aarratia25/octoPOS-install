// Telemetry: ships the setup.log up to the platform so failed
// installs in the field show up in the central portal without
// anyone having to ask the operator for the file.
//
// Design choices:
//   - Anonymous: the bootstrapper has no license key yet, so we
//     cannot present an installation token. Auth is per-IP throttle
//     on the platform endpoint. Anyone could in theory spam logs;
//     the throttle bounds the blast radius.
//   - Anchored by hwFingerprint (SHA256 of Windows Machine GUID +
//     primary MAC). Stable across reinstalls of the same physical
//     machine, so the portal can group repeat attempts under one
//     "machine" row.
//   - One installId (UUID v4) per bootstrapper launch. Repeat
//     attempts on the same machine produce separate rows that
//     share the hwFingerprint.
//   - Upload happens on every progress() tick that is far enough
//     past the previous one (at least 30s) and once at the very
//     end with the final outcome. Network failures are swallowed —
//     a missed log is never worth aborting the install.

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use base64::Engine;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::Serialize;

const ENDPOINT_DEFAULT: &str = "https://platform.octo-pos.net/install-logs";
const MIN_UPLOAD_INTERVAL_SECS: u64 = 30;
const HTTP_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    InProgress,
    Success,
    Error,
}

#[derive(Debug)]
pub struct TelemetryContext {
    pub install_id: String,
    pub hw_fingerprint: String,
    pub app_version: String,
    pub os_build: Option<u32>,
    pub started_at: Instant,
    pub last_upload: Mutex<Option<Instant>>,
}

impl TelemetryContext {
    pub fn new() -> Self {
        Self {
            install_id: uuid::Uuid::new_v4().to_string(),
            hw_fingerprint: hw_fingerprint(),
            app_version: format!("v{}", env!("CARGO_PKG_VERSION")),
            os_build: detect_os_build(),
            started_at: Instant::now(),
            last_upload: Mutex::new(None),
        }
    }

    /// Upload the current state. Throttled to one request every
    /// MIN_UPLOAD_INTERVAL_SECS unless `force = true` (used for the
    /// final upload). Best-effort: network errors are logged, never
    /// returned to the caller.
    pub fn upload(
        &self,
        outcome: Outcome,
        error_step: Option<&str>,
        error_message: Option<&str>,
        log_path: &PathBuf,
        force: bool,
    ) {
        if !force {
            let mut last = match self.last_upload.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            if let Some(prev) = *last {
                if prev.elapsed().as_secs() < MIN_UPLOAD_INTERVAL_SECS {
                    return;
                }
            }
            *last = Some(Instant::now());
        }

        let log_b64 = match read_and_gzip(log_path) {
            Ok(buf) if !buf.is_empty() => Some(buf),
            _ => None,
        };

        let payload = UploadPayload {
            install_id: &self.install_id,
            hw_fingerprint: &self.hw_fingerprint,
            app_version: &self.app_version,
            os_build: self.os_build,
            outcome,
            error_step,
            error_message,
            duration_sec: Some(self.started_at.elapsed().as_secs()),
            log_gzip_b64: log_b64.as_deref(),
        };

        let endpoint = std::env::var("OCTOPOS_TELEMETRY_URL")
            .unwrap_or_else(|_| ENDPOINT_DEFAULT.to_string());

        // ureq 2.x is sync — fire-and-forget on a background thread so
        // the install pipeline never waits on the network.
        let body = match serde_json::to_string(&payload) {
            Ok(s) => s,
            Err(_) => return,
        };
        std::thread::spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
                .build();
            let _ = agent
                .post(&endpoint)
                .set("Content-Type", "application/json")
                .send_string(&body);
        });
    }
}

#[derive(Serialize)]
struct UploadPayload<'a> {
    #[serde(rename = "installId")]
    install_id: &'a str,
    #[serde(rename = "hwFingerprint")]
    hw_fingerprint: &'a str,
    #[serde(rename = "appVersion")]
    app_version: &'a str,
    #[serde(rename = "osBuild", skip_serializing_if = "Option::is_none")]
    os_build: Option<u32>,
    outcome: Outcome,
    #[serde(rename = "errorStep", skip_serializing_if = "Option::is_none")]
    error_step: Option<&'a str>,
    #[serde(rename = "errorMessage", skip_serializing_if = "Option::is_none")]
    error_message: Option<&'a str>,
    #[serde(rename = "durationSec", skip_serializing_if = "Option::is_none")]
    duration_sec: Option<u64>,
    #[serde(rename = "logGzipB64", skip_serializing_if = "Option::is_none")]
    log_gzip_b64: Option<&'a str>,
}

fn read_and_gzip(path: &PathBuf) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut raw = Vec::with_capacity(64 * 1024);
    f.read_to_end(&mut raw)?;
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(&raw)?;
    let compressed = gz.finish()?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&compressed))
}

/// SHA256(machine GUID || primary MAC). Stable across reinstalls of
/// the same physical machine. On non-Windows (dev builds) returns a
/// deterministic placeholder so the type still satisfies callers.
#[cfg(windows)]
fn hw_fingerprint() -> String {
    use std::process::Command;

    let machine_guid = read_machine_guid().unwrap_or_default();
    let mac = primary_mac().unwrap_or_default();
    let mut hasher_input = String::new();
    hasher_input.push_str(&machine_guid);
    hasher_input.push('|');
    hasher_input.push_str(&mac);
    sha256_hex(hasher_input.as_bytes())
}

#[cfg(not(windows))]
fn hw_fingerprint() -> String {
    sha256_hex(b"non-windows-dev-build")
}

#[cfg(windows)]
fn read_machine_guid() -> Option<String> {
    use std::process::Command;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    // reg.exe is available everywhere; HKLM\SOFTWARE\Microsoft\
    // Cryptography\MachineGuid is the standard machine-stable ID
    // Microsoft installs at first boot.
    let out = Command::new("reg")
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Cryptography",
            "/v",
            "MachineGuid",
        ])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    for line in s.lines() {
        if let Some(idx) = line.find("REG_SZ") {
            return Some(line[idx + 6..].trim().to_string());
        }
    }
    None
}

#[cfg(windows)]
fn primary_mac() -> Option<String> {
    use std::process::Command;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    // getmac /fo csv /nh outputs lines like:
    //   "AA-BB-CC-DD-EE-FF","\Device\Tcpip_..."
    let out = Command::new("getmac")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/fo", "csv", "/nh"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // First column inside double quotes.
        let trimmed = line.trim_start_matches('"');
        if let Some(end) = trimmed.find('"') {
            let mac = &trimmed[..end];
            // Skip obvious "no MAC" placeholders like "N/A".
            if mac.contains('-') || mac.contains(':') {
                return Some(mac.to_string());
            }
        }
    }
    None
}

fn sha256_hex(bytes: &[u8]) -> String {
    // Inline SHA256 via a tiny implementation would bloat this file;
    // we use a hash through the system PowerShell instead. This is
    // called once at startup so the cost is negligible. Falls back
    // to a deterministic non-cryptographic hash on non-Windows so
    // the dev build still compiles.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let input = String::from_utf8_lossy(bytes).to_string();
        // PowerShell: returns lowercase hex of SHA256.
        let script = format!(
            "$h = [System.Security.Cryptography.SHA256]::Create(); \
             $b = [System.Text.Encoding]::UTF8.GetBytes('{}'); \
             ($h.ComputeHash($b) | ForEach-Object {{ '{{0:x2}}' -f $_ }}) -join ''",
            input.replace('\'', "''")
        );
        if let Ok(out) = Command::new("powershell")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["-NoProfile", "-Command", &script])
            .stdin(Stdio::null())
            .output()
        {
            return String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
    }
    // Fallback: FNV-1a for shape only (64 hex chars). Never actually
    // used in production builds.
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}{:016x}{:016x}{:016x}", h, !h, h.wrapping_add(1), !h.wrapping_add(1))
}

#[cfg(windows)]
fn detect_os_build() -> Option<u32> {
    use std::process::Command;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let out = Command::new("cmd")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/c", "ver"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split('.')
        .nth(2)
        .and_then(|s| s.split(']').next())
        .and_then(|s| s.parse::<u32>().ok())
}

#[cfg(not(windows))]
fn detect_os_build() -> Option<u32> {
    None
}
