//! The person's screens as their devices say they stand: this device's outputs offered to the
//! others, and every device's screens read back as the shared arrangement lays them out.
//! The display sheet and Settings read the published view
//! ([`settings_store::person_displays::PersonDisplays`]); the session publishes it in memory and on
//! disk whenever what the person's devices show changed, so a screen that appears on the tablet or
//! the desktop appears in the arrangement without either device asking.

use settings_store::hardware::{DisplayHardwareSnapshot, DisplaySet};
use settings_store::person_displays::{
    Arrangement, DeviceOutput, Layout, PersonDevice, PersonDisplays,
};
use tilo_device_fabric::link::SharedLink;
use tilo_network_actor::fabric::{
    DisplayConnectorPlacement, DisplayOutputObservation, DisplayRect,
};

/// Where this device's outputs stand and what their hardware says, as the person's other devices
/// hear it: the plane they make, and one entry per output with the place it holds on it. The plane
/// is in the device's own logical coordinates, so it is said at 1:1 while each output carries the
/// scale it runs at. `None` when the compositor published no display to offer.
pub(crate) fn observation(
    snapshot: Option<&DisplayHardwareSnapshot>,
) -> Option<(
    DisplayRect,
    u16,
    Vec<DisplayConnectorPlacement>,
    Vec<DisplayOutputObservation>,
)> {
    let snapshot = snapshot?;
    let plane = plane(snapshot)?;
    let connectors = connectors(snapshot);
    let outputs = outputs(snapshot);
    Some((plane, 1000, connectors, outputs))
}

/// The plane this device's outputs make: the rectangle that holds all of them, in the device's own
/// logical coordinates.
fn plane(snapshot: &DisplayHardwareSnapshot) -> Option<DisplayRect> {
    let mut bounds: Option<(i32, i32, i32, i32)> = None;
    for placement in &snapshot.layout {
        let right = placement.x.checked_add_unsigned(placement.width)?;
        let bottom = placement.y.checked_add_unsigned(placement.height)?;
        bounds = Some(match bounds {
            None => (placement.x, placement.y, right, bottom),
            Some((x, y, right0, bottom0)) => (
                x.min(placement.x),
                y.min(placement.y),
                right.max(right0),
                bottom.max(bottom0),
            ),
        });
    }
    let (x, y, right, bottom) = bounds?;
    Some(DisplayRect {
        x,
        y,
        width: u32::try_from(right.checked_sub(x)?).ok()?,
        height: u32::try_from(bottom.checked_sub(y)?).ok()?,
    })
}

/// Where each of this device's outputs stands, as the compositor laid them out.
fn connectors(snapshot: &DisplayHardwareSnapshot) -> Vec<DisplayConnectorPlacement> {
    snapshot
        .layout
        .iter()
        .filter_map(|placement| {
            let output = snapshot
                .outputs
                .iter()
                .find(|output| output.descriptor.connector == placement.connector)?;
            if !output.scale.is_finite() || !(0.5..=4.0).contains(&output.scale) {
                return None;
            }
            let scale_milli = format!("{:.0}", output.scale * 1000.0)
                .parse::<u16>()
                .ok()?;
            Some(DisplayConnectorPlacement {
                connector: placement.connector.clone(),
                label: output.descriptor.label().to_owned(),
                rect: DisplayRect {
                    x: placement.x,
                    y: placement.y,
                    width: placement.width,
                    height: placement.height,
                },
                scale_milli,
            })
        })
        .take(32)
        .collect()
}

/// What the hardware behind each of this device's outputs says: the monitor's name, the mode it
/// runs at and where it stands among the others.
fn outputs(snapshot: &DisplayHardwareSnapshot) -> Vec<DisplayOutputObservation> {
    if snapshot.outputs.len() > settings_store::person_displays::MAX_DEVICE_OUTPUTS {
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
            facts.valid().then(|| DisplayOutputObservation {
                connector: connector.clone(),
                output: facts,
            })
        })
        .collect()
}

/// Keeps the person's published devices as this session last wrote them, so a pass over unchanged
/// screens writes nothing.
#[derive(Default)]
pub(crate) struct Publisher {
    seen: Option<Vec<PersonDevice>>,
}

impl Publisher {
    /// Offer this device's displays, and publish the person's devices as the fabric now has them:
    /// once per change, on the pass that saw it.
    pub(crate) fn follow(
        &mut self,
        fabric: &SharedLink,
        snapshot: Option<&DisplayHardwareSnapshot>,
    ) -> bool {
        let mut moved = false;
        if let Some((rect, scale_milli, connectors, outputs)) = observation(snapshot) {
            moved |= fabric.share_display(rect, scale_milli, connectors, outputs);
        }
        let devices = fabric.person_displays();
        if self.seen.as_ref() == Some(&devices) {
            return moved;
        }
        self.seen = Some(devices.clone());
        let Some((local, _)) = fabric.local_device_fabric_identity() else {
            return moved;
        };
        let Ok((publisher_pid, publisher_start_ticks)) =
            settings_store::hardware::publisher_identity()
        else {
            return moved;
        };
        let view = PersonDisplays {
            schema_version: settings_store::person_displays::SCHEMA_VERSION,
            publisher_pid,
            publisher_start_ticks,
            local_device: local.as_str().to_owned(),
            devices,
        };
        if let Err(error) = view.save() {
            tracing::warn!(%error, "the person's displays are not published");
        }
        moved
    }
}

/// The person's devices and the arrangement they make, as Settings and this session last published
/// them. `None` when no device of theirs published a view, which is every session that takes part
/// as nobody.
pub(crate) fn person() -> Option<(PersonDisplays, Layout)> {
    let view = PersonDisplays::published()
        .map(|view| view.as_ref().clone())
        .or_else(PersonDisplays::load)?;
    let arrangement = Arrangement::load();
    let layout = view.layout(&arrangement);
    Some((view, layout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use settings_store::hardware::{
        DisplayHardwareSnapshot, DisplayOutputSnapshot, LiveOutputPlacement, MonitorIdentity,
        OutputDescriptor, OutputMode,
    };

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

    #[test]
    fn this_device_s_outputs_stand_where_the_compositor_put_them() {
        let hardware = snapshot(vec![
            output("eDP-1", "BOE", 0, 1.5, true),
            output("DP-2", "", 1920, 1.0, false),
        ]);
        let (plane, scale_milli, connectors, outputs) =
            observation(Some(&hardware)).expect("two outputs are offered");
        assert_eq!(
            (plane.x, plane.y, plane.width, plane.height),
            (0, 0, 3840, 1200),
            "the plane holds both outputs"
        );
        assert_eq!(
            scale_milli, 1000,
            "the plane is in logical coordinates; each output says its own scale"
        );
        assert_eq!(connectors[0].scale_milli, 1500, "the panel runs at 1.5x");
        assert_eq!(
            connectors
                .iter()
                .map(|connector| (connector.connector.as_str(), connector.rect.x))
                .collect::<Vec<_>>(),
            [("eDP-1", 0), ("DP-2", 1920)],
            "each output keeps the place it holds: {connectors:?}"
        );
        assert_eq!(
            outputs
                .iter()
                .map(|fact| (fact.connector.as_str(), fact.output.name.as_str()))
                .collect::<Vec<_>>(),
            [("eDP-1", "BOE Panel"), ("DP-2", "DP-2")],
            "each says the monitor behind it, or the connector when it names none: {outputs:?}"
        );
        assert!(outputs[0].output.builtin && !outputs[1].output.builtin);
        assert!(outputs.iter().all(|fact| fact.output.enabled));
    }

    /// This session's fabric, as a device of the person's with one screen.
    fn fabric() -> std::sync::Arc<crate::fabric::SessionFabric> {
        let (outgoing, _frames) = tokio::sync::mpsc::unbounded_channel();
        let runtime = tilo_device_fabric::runtime::FabricRuntime::restored(
            Some((
                tilo_device_fabric::DeviceId::new("this-laptop").expect("a device id"),
                "Laptop".to_owned(),
            )),
            None,
        )
        .expect("a runtime");
        crate::fabric::SessionFabric::new(
            runtime,
            outgoing,
            [7; 32],
            ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
            crate::fabric::Continuity::default(),
        )
    }

    /// What this device shows is published for the shared arrangement: its own device, with the
    /// screens it shows, once per change.
    #[test]
    fn this_device_s_screens_are_published_for_the_arrangement() {
        let _home = tilo_test_env::TestHome::new().expect("a home");
        let fabric = fabric();
        let link: tilo_device_fabric::link::SharedLink = fabric;
        let hardware = snapshot(vec![output("eDP-1", "BOE", 0, 1.5, true)]);
        let mut publisher = Publisher::default();
        assert!(
            publisher.follow(&link, Some(&hardware)),
            "the screen is news for the person's other devices"
        );
        let view = PersonDisplays::published().expect("the person's displays are published");
        assert_eq!(view.local_device, "this-laptop");
        assert_eq!(view.devices.len(), 1);
        assert!(view.devices[0].local && view.devices[0].online);
        assert_eq!(
            view.devices[0]
                .outputs
                .iter()
                .map(|output| output.name.as_str())
                .collect::<Vec<_>>(),
            ["BOE Panel"]
        );
        let generation = PersonDisplays::published_generation();
        assert!(
            !publisher.follow(&link, Some(&hardware)),
            "an unchanged screen is not offered again"
        );
        assert_eq!(
            PersonDisplays::published_generation(),
            generation,
            "and the arrangement is not written again"
        );
    }

    #[test]
    fn a_device_that_published_no_display_offers_nothing() {
        assert!(observation(None).is_none());
        assert!(observation(Some(&snapshot(Vec::new()))).is_none());
    }
}
