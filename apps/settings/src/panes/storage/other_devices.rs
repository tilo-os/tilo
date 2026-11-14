//! The storage summaries of the person's other devices, each asked of the
//! device itself. The session's reading of the person's devices
//! (`session:devices`, the list Sync between devices and Quick Settings show)
//! names them; each one that is not this device is asked for its
//! `storage-summary` through the host's own-device route, which carries
//! Settings' own action to that device and brings its answer back.

use runtime_fs::storage::ResidencySummary;
use ui::collaboration::SessionResource;
use ui::tilo_actions::remote::own_device;
use ui::tilo_resource::Resource as _;

use crate::panes::device_mesh::{self, PersonalDevice};

use super::{DeviceResidency, Fleet, StorageEffect};

struct Asked {
    id: String,
    call: ui::InvocationCall,
}

pub(super) struct OtherDevices {
    remote: ui::RemoteActions,
    /// The session's devices, followed: the window is woken at each reading.
    followed: Option<SessionResource>,
    /// The person's other devices as last read.
    others: Vec<PersonalDevice>,
    asked: Vec<Asked>,
}

impl OtherDevices {
    pub(super) fn new(remote: ui::RemoteActions) -> Self {
        let followed = device_mesh::follow_devices()
            .inspect_err(|error| {
                tracing::warn!(
                    target: "tilo::settings",
                    %error,
                    "the session's devices are not followed"
                );
            })
            .ok();
        Self {
            remote,
            followed,
            others: Vec::new(),
            asked: Vec::new(),
        }
    }

    /// Ask every other device again that is not still answering.
    pub(super) fn refresh(&mut self) {
        ask(&self.remote, &mut self.asked, &self.others);
    }

    /// Take in the newest reading and the answers that came; `true` when
    /// `fleet` changed.
    pub(super) fn poll(&mut self, fleet: &mut Fleet) -> bool {
        let mut changed = false;
        if let Some(devices) = self
            .followed
            .as_ref()
            .and_then(device_mesh::newest_devices)
            .and_then(Result::ok)
        {
            let others: Vec<PersonalDevice> =
                devices.into_iter().filter(|device| !device.local).collect();
            let fresh: Vec<PersonalDevice> = others
                .iter()
                .filter(|device| newly_reachable(&self.others, device))
                .cloned()
                .collect();
            ask(&self.remote, &mut self.asked, &fresh);
            let listed: Vec<String> = others.iter().map(|device| device.id.clone()).collect();
            fleet.keep_listed(&listed);
            self.others = others;
            changed = true;
        }
        if self.asked.is_empty() {
            return changed;
        }
        self.remote.poll();
        let others = &self.others;
        self.asked
            .retain(|asked| match crate::device_fleet::settled(&asked.call) {
                None => true,
                Some(Ok(output)) => {
                    if let Some(device) = residency_of(others, &asked.id, output.value) {
                        fleet.adopt(device);
                        changed = true;
                    }
                    false
                }
                Some(Err(_)) => false,
            });
        changed
    }
}

/// Ask each of `devices` for its summary that is not still answering.
fn ask(remote: &ui::RemoteActions, asked: &mut Vec<Asked>, devices: &[PersonalDevice]) {
    for device in devices {
        if !asked.iter().any(|known| known.id == device.id) {
            asked.push(Asked {
                id: device.id.clone(),
                call: remote.invoke(
                    own_device::target(),
                    summary_request(&device.id).argument(),
                    None,
                ),
            });
        }
    }
}

/// Whether `device` is to be asked now: it was not listed before, or it
/// just came online or nearby, so it can answer where it could not.
fn newly_reachable(before: &[PersonalDevice], device: &PersonalDevice) -> bool {
    before
        .iter()
        .find(|known| known.id == device.id)
        .is_none_or(|known| !(known.online || known.nearby) && (device.online || device.nearby))
}

fn summary_request(id: &str) -> own_device::Invocation {
    own_device::Invocation {
        device: id.to_owned(),
        app: <crate::resource::SettingsShell as ui::framework::ResourceApp>::ID.to_owned(),
        resource: crate::resource::Settings::KIND.to_owned(),
        action: StorageEffect::STORAGE_SUMMARY.id.to_owned(),
        arguments: None,
    }
}

fn residency_of(
    others: &[PersonalDevice],
    id: &str,
    value: Option<serde_json::Value>,
) -> Option<DeviceResidency> {
    let device = others.iter().find(|device| device.id == id)?;
    let summary: ResidencySummary = serde_json::from_value(value?).ok()?;
    Some(DeviceResidency {
        key: device.id.clone(),
        label: device.label.clone(),
        current: false,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::{newly_reachable, residency_of, summary_request};
    use crate::panes::device_mesh::PersonalDevice;
    use runtime_fs::storage::{ResidencySplit, ResidencySummary, WorkspaceResidency};

    fn device(id: &str, label: &str, local: bool, online: bool) -> PersonalDevice {
        PersonalDevice {
            id: id.into(),
            label: label.into(),
            online,
            nearby: false,
            local,
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

    tilo_test_env::scenario! {
        fn another_devices_storage_is_asked_and_answered_by_the_listed_device;

        phase a_device_is_asked_again_when_it_comes_back;
        // A device is asked when it first appears and again when it comes
        // back online, not at every reading.
        {
            let asked = |before: bool, now: (&str, bool)| {
                newly_reachable(
                    &[device("bb", "", false, before)],
                    &device(now.0, "", false, now.1),
                )
            };
            assert!(
                asked(false, ("cc", false)),
                "a device listed for the first time"
            );
            assert!(!asked(false, ("bb", false)), "a device still offline");
            assert!(asked(false, ("bb", true)), "a device that came back online");
            assert!(!asked(true, ("bb", true)), "a device that stayed online");
        }

        phase an_answer_from_a_device_outside_the_list_is_ignored;
        // An answer is taken in only for a device the session lists as one of
        // the person's other devices; one from any other key is ignored.
        {
            let others = [device("bb", "Laptop", false, true)];
            let card = residency_of(&others, "bb", Some(summary())).expect("a listed device");
            assert_eq!(card.label, "Laptop");
            assert_eq!(card.key, "bb");
            assert!(!card.current);
            assert_eq!(card.summary.workspaces[0].split.only_here_bytes, 7);
            assert!(residency_of(&others, "zz", Some(summary())).is_none());
            assert!(residency_of(&others, "bb", Some(serde_json::json!("x"))).is_none());
            assert!(residency_of(&others, "bb", None).is_none());
        }
    }

    #[test]
    fn the_request_asks_the_device_for_settings_own_summary() {
        let request = summary_request(&hex::encode([2; 32]));
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
