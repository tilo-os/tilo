//! Live system probing — best-effort `flatpak` / `podman` queries.
//!
//! In the live app we want the install state to reflect reality: an app
//! already installed via Flatpak should show "Open", not "Install". This
//! module shells out to `flatpak list` and `podman images` with a short
//! timeout and parses the result. Everything here is defensive: a missing
//! binary, a non-zero exit, or a slow call all degrade to "no live data",
//! never a panic and never a hang.
//!
//! It is deliberately NOT used by the `--shots` path (see
//! `lib::app_root`), so the screenshot stays deterministic and offline.

use std::collections::BTreeMap;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

/// Result of probing the system for already-present software.
#[derive(Clone, Debug, Default)]
pub struct ProbeResult {
    /// app-id → installed version, parsed from `flatpak list`.
    pub flatpak_versions: BTreeMap<String, String>,
    /// OCI image references reported by `podman images`.
    pub oci_images: Vec<String>,
    /// Whether a `flatpak` binary was found and answered.
    pub flatpak_available: bool,
    /// Whether a `podman` binary was found and answered.
    pub podman_available: bool,
}

/// Run `program args...`, returning stdout as a String, or `None` if the
/// binary is missing, exits non-zero, or doesn't answer within `timeout`.
fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let program = program.to_string();
    let args: Vec<String> = args.iter().map(std::string::ToString::to_string).collect();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let out = Command::new(&program)
            .args(&args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        // The receiver may already have timed out and dropped; ignore.
        let _ = tx.send(out);
    });
    match rx.recv_timeout(timeout) {
        Ok(v) => v,
        Err(_) => None,
    }
}

/// Probe the system. Bounded to a couple seconds total.
pub fn probe_system() -> ProbeResult {
    let mut result = ProbeResult::default();

    // `flatpak list --app --columns=application,version` → tab-separated
    // `app-id\tversion` lines.
    if let Some(out) = run_with_timeout(
        "flatpak",
        &["list", "--app", "--columns=application,version"],
        Duration::from_millis(1500),
    ) {
        result.flatpak_available = true;
        for line in out.lines() {
            let mut parts = line.split('\t');
            if let Some(id) = parts.next() {
                let id = id.trim();
                if id.is_empty() {
                    continue;
                }
                let version = parts.next().unwrap_or("").trim().to_string();
                result.flatpak_versions.insert(id.to_string(), version);
            }
        }
    }

    // `podman images --format {{.Repository}}:{{.Tag}}`.
    if let Some(out) = run_with_timeout(
        "podman",
        &["images", "--format", "{{.Repository}}:{{.Tag}}"],
        Duration::from_millis(1500),
    ) {
        result.podman_available = true;
        for line in out.lines() {
            let line = line.trim();
            if !line.is_empty() && line != "<none>:<none>" {
                result.oci_images.push(line.to_string());
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_binary_times_out_to_none() {
        // A binary that doesn't exist must degrade to None, not hang.
        let out = run_with_timeout(
            "definitely-not-a-real-binary-xyz",
            &["--version"],
            Duration::from_millis(500),
        );
        assert!(out.is_none());
    }

    #[test]
    fn probe_never_panics() {
        // Whatever the host has (or doesn't), this must return cleanly.
        let _ = probe_system();
    }
}
