//! What the desktop refuses: the person's lockdown policy.
//!
//! Every portal capability the person can switch off is one leaf of one stored value, so a
//! controller that changes two switches at once commits them together. Portal operations read it at
//! the point of use ([`LockdownPolicy::get`]).

use crate::Key;

pub const SECTION: &str = "lockdown";

pub const POLICY: Key<LockdownPolicy> = Key::new(SECTION, "policy", LockdownPolicy::default);

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent persisted switches, one per portal capability"
)]
pub struct LockdownPolicy {
    pub disable_printing: bool,
    pub disable_save_to_disk: bool,
    pub disable_application_handlers: bool,
    pub disable_location: bool,
    pub disable_camera: bool,
    pub disable_microphone: bool,
    pub disable_sound_output: bool,
}

impl LockdownPolicy {
    #[must_use]
    pub fn get() -> Self {
        POLICY.get()
    }

    pub fn set(&self) -> std::io::Result<()> {
        POLICY.set(self.clone())
    }
}

/// Call `on_change` with the policy as it now stands, and again whenever the
/// person changes it; dropping the watch stops this.
///
/// # Errors
/// The settings cannot be watched.
#[cfg(feature = "watch")]
pub fn watch(
    on_change: impl Fn(&LockdownPolicy) + Send + Sync + 'static,
) -> std::io::Result<crate::watch::SettingsWatch> {
    let on_change = std::sync::Arc::new(on_change);
    let rung = std::sync::Arc::clone(&on_change);
    let watch = crate::watch::watch_settings(move || rung(&LockdownPolicy::get()))?;
    on_change(&LockdownPolicy::get());
    Ok(watch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_policy_reads_and_writes_as_one_answer() {
        let dir = crate::test_dir();
        let store = crate::profile(dir.path());
        let _scoped = crate::scoped_source(Some(store));
        assert_eq!(LockdownPolicy::get(), LockdownPolicy::default());
        LockdownPolicy {
            disable_camera: true,
            disable_printing: true,
            ..LockdownPolicy::default()
        }
        .set()
        .unwrap();
        assert!(LockdownPolicy::get().disable_camera);
        let stored = crate::load_section(SECTION);
        assert_eq!(
            stored["policy"]["disable_printing"],
            serde_json::json!(true)
        );
        assert_eq!(stored["policy"]["disable_camera"], serde_json::json!(true));
        assert_eq!(
            stored["policy"]["disable_microphone"],
            serde_json::json!(false)
        );
    }
}
