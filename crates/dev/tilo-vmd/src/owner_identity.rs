//! Read-only inspection of the current session account's device-owner chain.

use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inspection {
    pub uid: u32,
    pub owns_device: bool,
}

/// Run only the existing read-only identity verifier as its account.
///
/// # Errors
/// The process UID differs or the owner certificate/device chain is invalid.
pub fn inspect_current(
    uid: u32,
    identity: &Path,
    device_root: &Path,
) -> std::io::Result<Inspection> {
    // SAFETY: geteuid takes no arguments and cannot fail.
    let actual = unsafe { libc::geteuid() };
    if actual != uid {
        return Err(std::io::Error::other("owner inspector credentials differ"));
    }
    Ok(Inspection {
        uid: actual,
        owns_device: tilo_identity_host::provisioning::active_identity_owns_device(
            identity,
            &tilo_user_identity::IdentityPaths::under(device_root),
        )?,
    })
}

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| std::io::Error::other("owner response unavailable"))?
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Inspect with the same record/certificate/device checks as the compositor.
///
/// # Errors
/// The current owner chain cannot be observed or validated.
pub fn inspect_as_user(
    executable: &Path,
    user: &str,
    uid: u32,
    identity: &Path,
    device_root: &Path,
) -> std::io::Result<bool> {
    let output = Output::default();
    let mut command = Command::new("/usr/sbin/runuser");
    command
        .env_clear()
        .args(["-u", user, "--"])
        .arg(executable)
        .args([
            "__device-owner-inspect",
            "--uid",
            &uid.to_string(),
            "--identity",
        ])
        .arg(identity)
        .arg("--device-root")
        .arg(device_root);
    let ended = crate::exec::run_bounded(
        command,
        Duration::from_secs(5),
        4096,
        output.clone(),
        std::io::sink(),
    )?;
    if ended != crate::exec::Ended::Exited(0) {
        return Err(std::io::Error::other(
            "owner inspector did not complete successfully",
        ));
    }
    let bytes = output
        .0
        .lock()
        .map_err(|_| std::io::Error::other("owner response unavailable"))?;
    let inspection: Inspection = serde_json::from_slice(&bytes)
        .map_err(|_| std::io::Error::other("invalid owner inspector response"))?;
    drop(bytes);
    if inspection.uid != uid {
        return Err(std::io::Error::other(
            "owner inspector response credentials differ",
        ));
    }
    Ok(inspection.owns_device)
}
