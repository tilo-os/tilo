//! `org.freedesktop.impl.portal.Lockdown`: the person's durable policy
//! (no printing, no saving to disk, no camera...) as the frontend reads it.
//!
//! The policy is the `lockdown` section of the person's settings: what the
//! person stores there is what every portal operation enforces, at the point
//! of use.
//!
//! Zbus 4 property setters cannot receive a message header. Accepting a
//! writable D-Bus property without authenticating its sender would let any
//! session client weaken policy, so D-Bus setters reject all writes. Tilo
//! Settings writes [`LockdownPolicy::set`] in-process.

use zbus::interface;

pub use settings_store::lockdown::LockdownPolicy;

/// Called with the policy, now and whenever the person changes it.
pub type PolicyListener = dyn Fn(&LockdownPolicy) + Send + Sync;

/// Follows the policy while it is held; dropping it stops the delivery.
pub type PolicyWatch = settings_store::watch::SettingsWatch;

#[derive(Clone, Default)]
pub struct LockdownBackend;

impl LockdownBackend {
    /// The policy of this person's session.
    #[must_use]
    pub fn for_session() -> Self {
        Self
    }

    pub fn policy(&self) -> LockdownPolicy {
        LockdownPolicy::get()
    }

    pub fn set_policy(&self, policy: LockdownPolicy) -> zbus::fdo::Result<()> {
        policy.set().map_err(|error| failed(&error.to_string()))
    }

    /// Follow every policy the person commits: `listener` hears the current
    /// one first and then each change, so a service that must act on the
    /// policy while it runs — stop the microphone, silence output — keeps up
    /// with what the person chose. The returned watch keeps the delivery
    /// alive; dropping it stops the listener hearing anything more.
    ///
    /// # Errors
    /// The settings cannot be watched.
    pub fn watch(&self, listener: &std::sync::Arc<PolicyListener>) -> std::io::Result<PolicyWatch> {
        let listener = std::sync::Arc::clone(listener);
        settings_store::lockdown::watch(move |policy| listener(policy))
    }
}

/// Read the current policy at the point of use. Portal operations must
/// enforce Lockdown themselves; merely exposing these properties would let a
/// policy controller report a restriction while privileged work still runs.
pub fn current_policy() -> LockdownPolicy {
    LockdownPolicy::get()
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
    use settings_store::SettingsSource as _;
    #[test]
    fn lockdown_policy_is_persistent_and_not_dbus_settable() {
        let dir = tempfile::tempdir().unwrap();
        let store = settings_store::Store::open(dir.path()).unwrap();
        let _scoped = settings_store::scoped_source(Some(store));
        let backend = LockdownBackend::for_session();
        assert_eq!(backend.policy(), LockdownPolicy::default());
        backend
            .set_policy(LockdownPolicy {
                disable_camera: true,
                ..LockdownPolicy::default()
            })
            .unwrap();
        assert!(LockdownBackend::for_session().policy().disable_camera);
        assert!(current_policy().disable_camera);
        let watched = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&watched);
        let listener: std::sync::Arc<PolicyListener> =
            std::sync::Arc::new(move |policy: &LockdownPolicy| {
                seen.lock().unwrap().push(policy.disable_microphone);
            });
        let _watch = backend.watch(&listener).unwrap();
        assert_eq!(watched.lock().unwrap().as_slice(), [false]);
        LockdownPolicy {
            disable_microphone: true,
            ..LockdownPolicy::default()
        }
        .set()
        .unwrap();
        assert_eq!(watched.lock().unwrap().as_slice(), [false, true]);
        assert!(
            backend.set_disable_camera(false).is_err(),
            "D-Bus must not weaken the policy"
        );
        settings_store::flush_pending_writes();
        let reopened = settings_store::Store::follow(dir.path()).unwrap();
        assert_eq!(
            reopened.load()["lockdown"]["policy"]["disable_microphone"],
            serde_json::json!(true),
            "the policy persisted"
        );
    }
}
