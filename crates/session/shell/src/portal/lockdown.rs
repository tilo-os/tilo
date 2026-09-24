//! Durable, compositor-owned policy reflected through the Lockdown portal.
//!
//! Zbus 4 property setters cannot receive a message header. Accepting a
//! writable D-Bus property without authenticating its sender would let any
//! session client weaken policy, so D-Bus setters reject all writes. Tilo
//! Settings uses [`LockdownBackend::set_policy`] in-process.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use zbus::interface;

#[cfg(target_os = "linux")]
use super::PortalContext;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockdownPolicy {
    pub disable_printing: bool,
    pub disable_save_to_disk: bool,
    pub disable_application_handlers: bool,
    pub disable_location: bool,
    pub disable_camera: bool,
    pub disable_microphone: bool,
    pub disable_sound_output: bool,
}

#[derive(Clone)]
pub struct LockdownBackend {
    policy: Arc<Mutex<LockdownPolicy>>,
    path: Arc<PathBuf>,
}

impl LockdownBackend {
    #[cfg(target_os = "linux")]
    pub fn new(_ctx: PortalContext) -> Self {
        Self::at_path(default_path())
    }
    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let policy = atomic_file::json::load_or_default(&path);
        Self {
            policy: Arc::new(Mutex::new(policy)),
            path: Arc::new(path),
        }
    }
    pub fn policy(&self) -> LockdownPolicy {
        self.policy.lock().map(|v| v.clone()).unwrap_or_default()
    }
    /// Trusted in-process policy update; intentionally not a D-Bus property mutation.
    pub fn set_policy(&self, policy: LockdownPolicy) -> zbus::fdo::Result<()> {
        let mut held = self
            .policy
            .lock()
            .map_err(|_| failed("lockdown policy lock poisoned"))?;
        atomic_file::json::write_private(&self.path, &policy)
            .map_err(|e| failed(&format!("cannot commit lockdown policy: {e}")))?;
        *held = policy;
        Ok(())
    }
}

/// Read the current shell policy at the point of use.  Portal operations must
/// enforce Lockdown themselves; merely exposing these properties would let a
/// policy controller report a restriction while privileged work still runs.
pub fn current_policy() -> LockdownPolicy {
    atomic_file::json::load_or_default(&default_path())
}

#[interface(name = "org.freedesktop.impl.portal.Lockdown")]
impl LockdownBackend {
    #[zbus(property, name = "disable-printing")]
    fn disable_printing(&self) -> bool {
        self.policy().disable_printing
    }
    #[zbus(property, name = "disable-printing")]
    fn set_disable_printing(&self, _value: bool) -> zbus::Result<()> {
        Err(denied())
    }
    #[zbus(property, name = "disable-save-to-disk")]
    fn disable_save_to_disk(&self) -> bool {
        self.policy().disable_save_to_disk
    }
    #[zbus(property, name = "disable-save-to-disk")]
    fn set_disable_save_to_disk(&self, _value: bool) -> zbus::Result<()> {
        Err(denied())
    }
    #[zbus(property, name = "disable-application-handlers")]
    fn disable_application_handlers(&self) -> bool {
        self.policy().disable_application_handlers
    }
    #[zbus(property, name = "disable-application-handlers")]
    fn set_disable_application_handlers(&self, _value: bool) -> zbus::Result<()> {
        Err(denied())
    }
    #[zbus(property, name = "disable-location")]
    fn disable_location(&self) -> bool {
        self.policy().disable_location
    }
    #[zbus(property, name = "disable-location")]
    fn set_disable_location(&self, _value: bool) -> zbus::Result<()> {
        Err(denied())
    }
    #[zbus(property, name = "disable-camera")]
    fn disable_camera(&self) -> bool {
        self.policy().disable_camera
    }
    #[zbus(property, name = "disable-camera")]
    fn set_disable_camera(&self, _value: bool) -> zbus::Result<()> {
        Err(denied())
    }
    #[zbus(property, name = "disable-microphone")]
    fn disable_microphone(&self) -> bool {
        self.policy().disable_microphone
    }
    #[zbus(property, name = "disable-microphone")]
    fn set_disable_microphone(&self, _value: bool) -> zbus::Result<()> {
        Err(denied())
    }
    #[zbus(property, name = "disable-sound-output")]
    fn disable_sound_output(&self) -> bool {
        self.policy().disable_sound_output
    }
    #[zbus(property, name = "disable-sound-output")]
    fn set_disable_sound_output(&self, _value: bool) -> zbus::Result<()> {
        Err(denied())
    }
}

fn default_path() -> PathBuf {
    super::private_state_root()
        .unwrap_or_else(|| PathBuf::from("/dev/null"))
        .join("tilo/portal/lockdown.json")
}
fn failed(message: &str) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(message.into())
}
fn denied() -> zbus::Error {
    zbus::Error::from(zbus::fdo::Error::AccessDenied(
        "Lockdown policy is controlled by Tilo Settings".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn trusted_policy_persists() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("policy.json");
        let b = LockdownBackend::at_path(&p);
        b.set_policy(LockdownPolicy {
            disable_camera: true,
            ..LockdownPolicy::default()
        })
        .unwrap();
        assert!(LockdownBackend::at_path(p).policy().disable_camera);
        assert_eq!(
            fs::metadata(dir.path().join("policy.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    #[test]
    fn dbus_setter_is_denied() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            LockdownBackend::at_path(dir.path().join("lockdown.json"))
                .set_disable_camera(false)
                .is_err()
        );
    }
}
