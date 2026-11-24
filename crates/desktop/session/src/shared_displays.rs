//! The person's screens as their devices say they stand: this device keeps its outputs in the
//! record of the person's devices, and every device's screens are read back from that record as
//! the shared placement lays them out.
//! The display sheet and Settings read the published view
//! ([`settings_store::person_displays::PersonDisplays`]); the session publishes it in memory and on
//! disk whenever what the person's devices show changed, so a screen that appears on the tablet or
//! the desktop appears in the placement without either device asking.

use std::collections::BTreeSet;

use settings_store::hardware::{DisplayHardwareSnapshot, DisplaySet};
use settings_store::person_displays::{
    DeviceOutput, MAX_DEVICE_OUTPUTS, PersonDevice, PersonDisplays,
};
use tilo_workspace_service::workspaces::person_workspace::devices::{Devices, KeyKind};

use crate::owner_session::OwnerDevices;

/// The fact under which a device keeps the outputs it shows in its entry of the record of the
/// person's devices: [`DeviceOutput`]s, postcard in hex.
pub(crate) const DISPLAYS: &str = "displays";

/// What the hardware behind each of this device's outputs says: the monitor's name, the mode it
/// runs at and where it stands among the others, in the device's own logical coordinates. Empty
/// when the compositor published no display.
pub(crate) fn outputs(snapshot: Option<&DisplayHardwareSnapshot>) -> Vec<DeviceOutput> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    if snapshot.outputs.len() > MAX_DEVICE_OUTPUTS {
        return Vec::new();
    }
    let identities = DisplaySet::of(
        snapshot
            .outputs
            .iter()
            .map(|output| (&output.descriptor).into()),
    );
    snapshot
        .outputs
        .iter()
        .filter_map(|output| {
            if !output.scale.is_finite() || !(0.5..=4.0).contains(&output.scale) {
                return None;
            }
            let connector = &output.descriptor.connector;
            let placement = snapshot
                .layout
                .iter()
                .find(|placement| &placement.connector == connector);
            let name = output
                .descriptor
                .monitor
                .as_ref()
                .map(settings_store::hardware::MonitorIdentity::display_name)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| output.descriptor.label().to_owned());
            let facts = DeviceOutput {
                id: identities.members.get(connector)?.clone(),
                name,
                builtin: output.builtin,
                x: placement.map_or(0, |placement| placement.x),
                y: placement.map_or(0, |placement| placement.y),
                width: placement.map_or(0, |placement| placement.width),
                height: placement.map_or(0, |placement| placement.height),
                mode_width: output.current_mode.width,
                mode_height: output.current_mode.height,
                refresh_mhz: output.current_mode.refresh_mhz,
                scale_milli: format!("{:.0}", output.scale * 1000.0).parse().ok()?,
                connected: true,
                enabled: placement.is_some(),
            };
            facts.valid().then_some(facts)
        })
        .collect()
}

/// `outputs` as the record keeps them: postcard in hex.
fn encoded(outputs: &[DeviceOutput]) -> Option<String> {
    postcard::to_allocvec(outputs)
        .inspect_err(|error| tracing::warn!(%error, "this device's outputs do not encode"))
        .ok()
        .map(hex::encode)
}

/// The outputs a device keeps in `value`, where they hold: no more than a device has, each one an
/// output could be, none named twice.
pub(crate) fn decoded(value: &str) -> Option<Vec<DeviceOutput>> {
    let outputs: Vec<DeviceOutput> = crate::platform::fabric::decoded(value)?;
    let ids = outputs
        .iter()
        .map(|output| output.id.as_str())
        .collect::<BTreeSet<_>>();
    (outputs.len() <= MAX_DEVICE_OUTPUTS
        && ids.len() == outputs.len()
        && outputs.iter().all(DeviceOutput::valid))
    .then_some(outputs)
}

/// The person's devices `record` keeps, each with the outputs it says it shows: their devices
/// whose certificates their root signed and did not take back, `local` (hex) this one and `online`
/// (hex) those on a session with it now.
pub(crate) fn person_devices(
    record: &Devices,
    person: &[u8; 32],
    local: &str,
    online: &BTreeSet<String>,
) -> Vec<PersonDevice> {
    record
        .listed(person)
        .into_iter()
        .filter(|listed| {
            listed.device.kind == KeyKind::Device && !listed.device.revoked(&listed.cert)
        })
        .map(|listed| PersonDevice {
            label: listed.device.called(&listed.cert),
            local: listed.key == local,
            online: listed.key == local || online.contains(&listed.key),
            outputs: listed
                .device
                .facts
                .get(DISPLAYS)
                .and_then(|value| decoded(value))
                .unwrap_or_default(),
            device: listed.key,
        })
        .collect()
}

/// Keeps the person's published devices as this session last wrote them, so a pass over unchanged
/// screens writes nothing.
#[derive(Default)]
pub(crate) struct Publisher {
    seen: Option<Vec<PersonDevice>>,
    /// What the last pass read: the record's reading, the devices online and the snapshot's
    /// time. A pass that reads the same does nothing.
    read: Option<(u64, BTreeSet<String>, Option<u64>)>,
}

impl Publisher {
    /// Keep this device's outputs in the record of the person's devices, and publish their devices
    /// as the record now has them, `online` (hex) those on a session with this one: once per
    /// change, on the pass that saw it.
    pub(crate) fn follow(
        &mut self,
        devices: &OwnerDevices,
        online: &BTreeSet<String>,
        snapshot: Option<&DisplayHardwareSnapshot>,
    ) {
        let read = (
            devices.revision(),
            online.clone(),
            snapshot.map(|snapshot| snapshot.generated_unix_ms),
        );
        if self.read.as_ref() == Some(&read) {
            return;
        }
        self.read = Some(read);
        if snapshot.is_some()
            && let Some(value) = encoded(&outputs(snapshot))
        {
            devices.keep_own(DISPLAYS, &value);
        }
        let (person, local) = devices.who();
        let listed = person_devices(&devices.devices(), &person, &local, online);
        self.publish(&local, listed);
    }

    /// Publish `devices`, `local` (hex) among them this one, unless it is what was published last.
    /// Whether it published them.
    fn publish(&mut self, local: &str, devices: Vec<PersonDevice>) -> bool {
        if self.seen.as_ref() == Some(&devices) {
            return false;
        }
        self.seen = Some(devices.clone());
        let Ok((publisher_pid, publisher_start_ticks)) =
            settings_store::hardware::publisher_identity()
        else {
            return false;
        };
        let view = PersonDisplays {
            schema_version: settings_store::person_displays::SCHEMA_VERSION,
            publisher_pid,
            publisher_start_ticks,
            local_device: local.to_owned(),
            devices,
        };
        if let Err(error) = view.save() {
            tracing::warn!(%error, "the person's displays are not published");
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use settings_store::hardware::{
        DisplayHardwareSnapshot, DisplayOutputSnapshot, LiveOutputPlacement, MonitorIdentity,
        OutputDescriptor, OutputMode,
    };
    use tilo_user_identity::{DeviceCapabilities, DeviceCert};

    /// One of this device's outputs at `x` , built-in when `builtin` , at `scale` .
    fn output(
        connector: &str,
        make: &str,
        x: i32,
        scale: f64,
        builtin: bool,
    ) -> (DisplayOutputSnapshot, LiveOutputPlacement) {
        let monitor = (!make.is_empty()).then(|| MonitorIdentity {
            make: make.to_owned(),
            model: "Panel".into(),
            serial: connector.to_owned(),
            physical_size_mm: None,
        });
        let descriptor = OutputDescriptor {
            connector: connector.to_owned(),
            connector_label: connector.to_owned(),
            modes: Vec::new(),
            hdr: false,
            vrr: false,
            color_profiles: Vec::new(),
            monitor,
        };
        (
            DisplayOutputSnapshot {
                descriptor,
                current_mode: OutputMode {
                    width: 1920,
                    height: 1200,
                    refresh_mhz: 60_000,
                },
                scale,
                builtin,
                sink_hdr: false,
                hdr_unavailable_reason: None,
                sink_vrr: false,
                vrr_unavailable_reason: None,
                unavailable_reason: None,
                color_profile_labels: std::collections::BTreeMap::default(),
            },
            LiveOutputPlacement {
                connector: connector.to_owned(),
                x,
                y: 0,
                width: 1920,
                height: 1200,
            },
        )
    }

    fn snapshot(
        outputs: Vec<(DisplayOutputSnapshot, LiveOutputPlacement)>,
    ) -> DisplayHardwareSnapshot {
        DisplayHardwareSnapshot {
            generated_unix_ms: 0,
            backend: "headless".into(),
            publisher_pid: 1,
            publisher_start_ticks: 0,
            layout: outputs.iter().map(|(_, place)| place.clone()).collect(),
            outputs: outputs.into_iter().map(|(output, _)| output).collect(),
        }
    }

    /// A device of the person whose root is `root`, labelled `label`, its key drawn from `seed`.
    fn device(root: &ed25519_dalek::SigningKey, label: &str, seed: u8) -> DeviceCert {
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.to_owned(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).expect("the root signs");
        cert
    }

    tilo_test_env::scenario! {
        fn this_devices_displays_are_published_where_they_stand;

        phase this_device_s_outputs_stand_where_the_compositor_put_them;
        {
            let hardware = snapshot(vec![
                output("eDP-1", "BOE", 0, 1.5, true),
                output("DP-2", "", 1920, 1.0, false),
            ]);
            let outputs = outputs(Some(&hardware));
            assert_eq!(
                outputs
                    .iter()
                    .map(|output| (output.name.as_str(), output.x, output.scale_milli))
                    .collect::<Vec<_>>(),
                [("BOE Panel", 0, 1500), ("DP-2", 1920, 1000)],
                "each says the monitor behind it, or the connector when it names none, where it \
                 stands and the scale it runs at: {outputs:?}"
            );
            assert!(outputs[0].builtin && !outputs[1].builtin);
            assert!(outputs.iter().all(|output| output.enabled));
        }

        phase every_device_s_screens_are_read_from_the_record;
        // What each device keeps in the record of the person's devices is what the placement
        // shows: this one's screens, and the tablet's, offline until it is on a session here.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
            let (laptop, tablet) = (device(&root, "Laptop", 5), device(&root, "Tablet", 6));
            let mut record = tilo_resource::Replica::<Devices>::new();
            let shows = |cert: &DeviceCert, x: i32| {
                let hardware = snapshot(vec![output("eDP-1", "BOE", x, 1.0, true)]);
                Devices::KEEP_FACT.with((
                    hex::encode(cert.device_public_key),
                    hex::encode(postcard::to_allocvec(cert).unwrap()),
                    DISPLAYS.to_owned(),
                    encoded(&outputs(Some(&hardware))).unwrap(),
                ))
            };
            record.apply(shows(&laptop, 0)).expect("the laptop keeps its screen");
            record.apply(shows(&tablet, 0)).expect("the tablet keeps its screen");
            let local = hex::encode(laptop.device_public_key);
            let listed = person_devices(
                &record.state().unwrap(),
                &root.verifying_key().to_bytes(),
                &local,
                &BTreeSet::new(),
            );
            let laptop_listed = listed.iter().find(|device| device.local).unwrap();
            assert!(laptop_listed.online, "this device is always online");
            assert_eq!(laptop_listed.label, "Laptop");
            assert_eq!(laptop_listed.outputs[0].name, "BOE Panel");
            let tablet_listed = listed.iter().find(|device| !device.local).unwrap();
            assert!(!tablet_listed.online && tablet_listed.outputs.len() == 1);

            let other = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
            record
                .apply(shows(&device(&other, "Theirs", 7), 0))
                .expect("a key the record does not vouch for is kept as written");
            assert_eq!(
                person_devices(
                    &record.state().unwrap(),
                    &root.verifying_key().to_bytes(),
                    &local,
                    &BTreeSet::new(),
                )
                .len(),
                2,
                "a device another root certified is none of the person's"
            );
        }

        phase the_placement_is_published_once_per_change;
        {
            let _home = tilo_test_env::TestHome::new().expect("a home");
            let hardware = snapshot(vec![output("eDP-1", "BOE", 0, 1.5, true)]);
            let devices = vec![PersonDevice {
                device: "laptop".into(),
                label: "Laptop".into(),
                local: true,
                online: true,
                outputs: outputs(Some(&hardware)),
            }];
            let mut publisher = Publisher::default();
            assert!(publisher.publish("laptop", devices.clone()), "the screen is news");
            let view = PersonDisplays::published().expect("the person's displays are published");
            assert_eq!(view.local_device, "laptop");
            assert_eq!(view.devices, devices);
            let generation = PersonDisplays::published_generation();
            assert!(!publisher.publish("laptop", devices), "an unchanged placement is not");
            assert_eq!(
                PersonDisplays::published_generation(),
                generation,
                "and it is not written again"
            );
        }

        phase a_device_that_published_no_display_offers_nothing;
        {
            assert!(outputs(None).is_empty());
            assert!(outputs(Some(&snapshot(Vec::new()))).is_empty());
        }
    }
}
