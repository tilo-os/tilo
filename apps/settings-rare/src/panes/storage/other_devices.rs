//! The storage summaries of the person's other devices, each asked of the
//! device itself.
//!
//! The host's device list names the person's devices; each one that is
//! neither this device nor revoked is asked for its `storage-summary`
//! through the host's own-device route, which carries Settings' own action
//! to that device and brings its answer back. A device that is offline or
//! refuses simply shows no card. An answer is only taken in for a device the
//! latest list still names, never for a key it does not.

use runtime_fs::storage::ResidencySummary;
use ui::tilo_actions::remote::own_device;
use ui::tilo_resource::Resource as _;

use crate::device_fleet::{self, DeviceView};

use super::{DeviceResidency, StorageEffect};

/// One device asked for its summary, and its answer when it comes.
struct Asked {
    key: [u8; 32],
    call: ui::InvocationCall,
}

/// One round of asking: the device list, then each device on it.
pub(super) struct OtherDevices {
    remote: ui::RemoteActions,
    list_call: Option<ui::InvocationCall>,
    /// The person's devices as the host last listed them.
    fleet: Vec<DeviceView>,
    asked: Vec<Asked>,
}

/// What a round brought in.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Arrived {
    /// The devices the host listed, when a new list arrived: cards of
    /// devices it no longer names go.
    pub listed: Option<Vec<String>>,
    /// Summaries that arrived, one per device.
    pub summaries: Vec<DeviceResidency>,
}

impl OtherDevices {
    pub(super) fn new(remote: ui::RemoteActions) -> Self {
        Self {
            remote,
            list_call: None,
            fleet: Vec::new(),
            asked: Vec::new(),
        }
    }

    /// Ask again, unless a round is still running.
    pub(super) fn refresh(&mut self) {
        if self.list_call.is_none() && self.asked.is_empty() {
            self.list_call = Some(device_fleet::list(&self.remote));
        }
    }

    /// Take in whatever answers settled.
    pub(super) fn poll(&mut self) -> Arrived {
        let mut arrived = Arrived::default();
        if self.list_call.is_none() && self.asked.is_empty() {
            return arrived;
        }
        self.remote.poll();
        if let Some(answer) = self.list_call.as_ref().and_then(device_fleet::settled) {
            self.list_call = None;
            if let Ok(devices) = answer.and_then(|output| device_fleet::devices(output.value)) {
                self.asked = asked_of(&devices)
                    .map(|device| Asked {
                        key: device.key,
                        call: ask(&self.remote, &device.key),
                    })
                    .collect();
                arrived.listed = Some(
                    asked_of(&devices)
                        .map(|device| hex::encode(device.key))
                        .collect(),
                );
                self.fleet = devices;
            }
        }
        let fleet = &self.fleet;
        self.asked
            .retain(|asked| match device_fleet::settled(&asked.call) {
                None => true,
                Some(Ok(output)) => {
                    arrived
                        .summaries
                        .extend(residency_of(fleet, &asked.key, output.value));
                    false
                }
                Some(Err(_)) => false,
            });
        arrived
    }
}

/// The devices on `fleet` to ask: neither this one nor revoked.
fn asked_of(fleet: &[DeviceView]) -> impl Iterator<Item = &DeviceView> {
    fleet
        .iter()
        .filter(|device| !device.current && !device.revoked)
}

/// Ask device `key` for its storage summary.
fn ask(remote: &ui::RemoteActions, key: &[u8; 32]) -> ui::InvocationCall {
    remote.invoke(own_device::target(), summary_request(key).argument(), None)
}

/// The own-device invocation that asks device `key` for Settings' storage
/// summary.
fn summary_request(key: &[u8; 32]) -> own_device::Invocation {
    own_device::Invocation {
        device: hex::encode(key),
        app: <crate::resource::SettingsShell as ui::framework::ResourceApp>::ID.to_owned(),
        // Settings publishes its effects on its one resource.
        resource: crate::resource::Settings::KIND.to_owned(),
        action: StorageEffect::STORAGE_SUMMARY.id.to_owned(),
        arguments: None,
    }
}

/// The card for what device `key` answered, if `fleet` names it as one of
/// the person's other devices and the answer is a storage summary.
fn residency_of(
    fleet: &[DeviceView],
    key: &[u8; 32],
    value: Option<serde_json::Value>,
) -> Option<DeviceResidency> {
    let device = asked_of(fleet).find(|device| &device.key == key)?;
    let summary: ResidencySummary = serde_json::from_value(value?).ok()?;
    Some(DeviceResidency {
        key: hex::encode(device.key),
        label: device.label.clone(),
        current: false,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::{asked_of, residency_of, summary_request};
    use crate::device_fleet::DeviceView;
    use runtime_fs::storage::{ResidencySplit, ResidencySummary, WorkspaceResidency};

    fn device(key: u8, label: &str, current: bool, revoked: bool) -> DeviceView {
        DeviceView {
            label: label.into(),
            key: [key; 32],
            fingerprint: String::new(),
            current,
            revoked,
            can_host: false,
        }
    }

    fn summary() -> serde_json::Value {
        serde_json::to_value(ResidencySummary {
            workspaces: vec![WorkspaceResidency {
                workspace: "notes".into(),
                kept_here: true,
                split: ResidencySplit {
                    only_here_bytes: 7,
                    ..ResidencySplit::default()
                },
            }],
        })
        .unwrap()
    }

    /// Only the person's other devices are asked: not this one, not a
    /// revoked one.
    #[test]
    fn only_other_unrevoked_devices_are_asked() {
        let fleet = [
            device(1, "This", true, false),
            device(2, "Laptop", false, false),
            device(3, "Lost phone", false, true),
        ];
        let asked: Vec<_> = asked_of(&fleet).map(|device| device.key[0]).collect();
        assert_eq!(asked, [2]);
    }

    /// An answer is taken in only for a device the list names as one of
    /// the person's other devices; one from any other key is ignored.
    #[test]
    fn an_answer_from_a_key_outside_the_fleet_is_ignored() {
        let fleet = [
            device(1, "This", true, false),
            device(2, "Laptop", false, false),
            device(3, "Lost phone", false, true),
        ];
        let card = residency_of(&fleet, &[2; 32], Some(summary())).expect("a fleet device");
        assert_eq!(card.label, "Laptop");
        assert_eq!(card.key, hex::encode([2; 32]));
        assert!(!card.current);
        assert_eq!(card.summary.workspaces[0].split.only_here_bytes, 7);
        for key in [[9; 32], [1; 32], [3; 32]] {
            assert!(
                residency_of(&fleet, &key, Some(summary())).is_none(),
                "{key:?} is not one of the person's other devices"
            );
        }
        assert!(residency_of(&fleet, &[2; 32], Some(serde_json::json!("x"))).is_none());
        assert!(residency_of(&fleet, &[2; 32], None).is_none());
    }

    /// The request names Settings' own storage-summary action on the
    /// device, which is all the route lets Settings reach.
    #[test]
    fn the_request_asks_the_device_for_settings_own_summary() {
        let request = summary_request(&[2; 32]);
        assert_eq!(request.app, "so.tilo.settings");
        assert_eq!(request.device, hex::encode([2; 32]));
        let checked = request.check().expect("a valid own-device invocation");
        let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
        let published = app
            .actions()
            .unwrap()
            .into_iter()
            .find(|action| action.address == checked.address);
        assert!(
            published.is_some(),
            "Settings publishes {:?}, which the other device is asked for",
            checked.address
        );
    }
}
