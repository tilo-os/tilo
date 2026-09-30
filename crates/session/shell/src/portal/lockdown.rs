//! Durable, compositor-owned policy reflected through the Lockdown portal.
//!
//! Zbus 4 property setters cannot receive a message header. Accepting a
//! writable D-Bus property without authenticating its sender would let any
//! session client weaken policy, so D-Bus setters reject all writes. Tilo
//! Settings uses [`LockdownBackend::set_policy`] in-process.

use std::path::PathBuf;

use zbus::interface;

#[cfg(target_os = "linux")]
use super::PortalContext;

pub use tilo_lockdown_policy::LockdownPolicy;
use tilo_lockdown_policy::PolicyStore;
pub(crate) use tilo_lockdown_policy::watch_policy_at;

pub(crate) fn watch_policy(
    listener: &std::sync::Arc<dyn Fn(&LockdownPolicy) + Send + Sync>,
) -> std::io::Result<()> {
    watch_policy_at(default_path(), listener)
}

#[derive(Clone)]
pub struct LockdownBackend {
    state: PolicyStore,
}
impl LockdownBackend {
    #[cfg(target_os = "linux")]
    pub fn new(_ctx: PortalContext) -> Self {
        Self::at_path(default_path())
    }
    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        Self {
            state: PolicyStore::at_path(path),
        }
    }
    pub fn policy(&self) -> LockdownPolicy {
        self.state.policy()
    }
    pub fn set_policy(&self, policy: LockdownPolicy) -> zbus::fdo::Result<()> {
        self.state
            .set_policy(policy)
            .map_err(|error| failed(&error.to_string()))
    }
}

/// Read the current shell policy at the point of use.  Portal operations must
/// enforce Lockdown themselves; merely exposing these properties would let a
/// policy controller report a restriction while privileged work still runs.
pub fn current_policy() -> LockdownPolicy {
    tilo_files::native::atomic::json::load_or_default(&default_path())
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
    fn lockdown_policy_is_trusted_persistent_and_not_dbus_settable() {
        // Phase `trusted_policy_persists`.
        {
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
        // Phase `dbus_setter_is_denied`.
        {
            let dir = tempfile::tempdir().unwrap();
            assert!(
                LockdownBackend::at_path(dir.path().join("lockdown.json"))
                    .set_disable_camera(false)
                    .is_err()
            );
        }
    }
}
