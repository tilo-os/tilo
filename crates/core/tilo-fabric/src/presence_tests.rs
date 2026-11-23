//! What a device says of itself while it is there: its screens and how it is woken.

use super::*;

/// What the hardware behind connector `DP-1` says, for a screen at `x`.
fn observed(output_id: &str, x: i32, scale_milli: u16) -> DisplayOutputObservation {
    DisplayOutputObservation {
        connector: "DP-1".into(),
        output: settings_store::person_displays::DeviceOutput {
            id: output_id.into(),
            name: "Panel".into(),
            builtin: false,
            x,
            y: 0,
            width: 1000,
            height: 800,
            mode_width: 2000,
            mode_height: 1600,
            refresh_mhz: 59_940,
            scale_milli,
            connected: true,
            enabled: true,
        },
    }
}

fn placed_connector(x: i32, name: &str) -> DisplayConnectorPlacement {
    DisplayConnectorPlacement {
        connector: name.into(),
        label: "Panel".into(),
        rect: DisplayRect {
            x,
            y: 0,
            width: 1000,
            height: 800,
        },
        scale_milli: 1000,
    }
}

/// One panel at `x`, placed and observed alike.
fn display(x: i32) -> DeviceDisplay {
    DeviceDisplay {
        rect: DisplayRect {
            x,
            y: 0,
            width: 1000,
            height: 800,
        },
        scale_milli: 1000,
        connectors: vec![placed_connector(x, "DP-1")],
        outputs: vec![observed("monitor:panel", x, 1000)],
    }
}

#[test]
fn a_display_keeps_its_outputs_and_placements_describing_each_other() {
    assert_eq!(display(0).validate(), Ok(()));

    let mut resized = display(0);
    resized.outputs[0].output.width = 1001;
    assert_eq!(
        resized.validate(),
        Err(FabricError::InvalidDisplayPlacement),
        "the output is not where the connector places it"
    );

    let mut rescaled = display(0);
    rescaled.outputs[0].output.scale_milli = 1500;
    assert_eq!(
        rescaled.validate(),
        Err(FabricError::InvalidDisplayPlacement)
    );

    let mut unobserved = display(0);
    unobserved.outputs.clear();
    assert_eq!(
        unobserved.validate(),
        Err(FabricError::InvalidDisplayPlacement),
        "a placed connector with no active output"
    );

    let mut inactive = display(0);
    inactive.outputs[0].output.connected = false;
    assert_eq!(
        inactive.validate(),
        Err(FabricError::InvalidDisplayPlacement),
        "a placed connector whose output is not active"
    );

    let mut stray = display(0);
    stray.outputs.push(DisplayOutputObservation {
        connector: "HDMI-1".into(),
        ..observed("monitor:other", 0, 1000)
    });
    assert_eq!(
        stray.validate(),
        Err(FabricError::InvalidDisplayPlacement),
        "an active output no connector places"
    );
}

#[test]
fn a_display_refuses_bounds_it_cannot_place_and_repeats() {
    let mut flat = display(0);
    flat.rect.height = 0;
    assert_eq!(flat.validate(), Err(FabricError::InvalidDisplayPlacement));

    let mut tiny = display(0);
    tiny.scale_milli = 499;
    assert_eq!(tiny.validate(), Err(FabricError::InvalidDisplayPlacement));

    let mut huge = display(0);
    huge.connectors[0].scale_milli = 4001;
    assert_eq!(huge.validate(), Err(FabricError::InvalidDisplayPlacement));

    let mut unreadable = display(0);
    unreadable.connectors[0].label = "line\nbreak".into();
    assert_eq!(
        unreadable.validate(),
        Err(FabricError::InvalidDisplayPlacement)
    );

    let mut nameless = display(0);
    nameless.connectors[0].connector = String::new();
    assert_eq!(
        nameless.validate(),
        Err(FabricError::InvalidDisplayPlacement)
    );

    let mut crowded = display(0);
    crowded.connectors = (0..33)
        .map(|n| placed_connector(0, &format!("DP-{n}")))
        .collect();
    assert_eq!(
        crowded.validate(),
        Err(FabricError::InvalidDisplayPlacement)
    );

    let mut overfull = display(0);
    overfull.outputs = (0..=settings_store::person_displays::MAX_DEVICE_OUTPUTS)
        .map(|n| DisplayOutputObservation {
            connector: format!("DP-{n}"),
            ..observed(&format!("monitor:{n}"), 0, 1000)
        })
        .collect();
    assert_eq!(
        overfull.validate(),
        Err(FabricError::InvalidDisplayPlacement)
    );

    let mut twice_placed = display(0);
    twice_placed.connectors.push(placed_connector(0, "DP-1"));
    assert_eq!(
        twice_placed.validate(),
        Err(FabricError::DuplicateItem),
        "one connector is placed once"
    );

    let mut twice_observed = display(0);
    twice_observed
        .outputs
        .push(observed("monitor:panel", 0, 1000));
    assert_eq!(
        twice_observed.validate(),
        Err(FabricError::DuplicateItem),
        "one connector is observed once"
    );

    let mut same_panel = display(0);
    same_panel.connectors.push(placed_connector(1000, "DP-2"));
    same_panel.outputs.push(DisplayOutputObservation {
        connector: "DP-2".into(),
        ..observed("monitor:panel", 1000, 1000)
    });
    assert_eq!(
        same_panel.validate(),
        Err(FabricError::DuplicateItem),
        "one output id names one screen"
    );
}

#[test]
fn a_device_says_only_wake_routes_and_screens_it_could_have() {
    let route = |mac: [u8; 6]| WakeRoute {
        mac,
        kind: WakeInterfaceKind::Ethernet,
        magic_packet: true,
        subnet: None,
    };
    let presence = DevicePresence {
        wake_routes: vec![route([0x02; 6])],
        session: OwnerSession::default(),
        display: Some(display(0)),
    };
    assert_eq!(presence.validate(), Ok(()));

    let mut crowded = presence.clone();
    crowded.wake_routes = vec![route([0x02; 6]); MAX_WAKE_ROUTES + 1];
    assert_eq!(crowded.validate(), Err(FabricError::TooLarge));

    let mut multicast = presence.clone();
    multicast.wake_routes = vec![route([0x01; 6])];
    assert_eq!(multicast.validate(), Err(FabricError::InvalidWakeRoute));

    let mut repeated = presence.clone();
    repeated.wake_routes = vec![route([0x02; 6]), route([0x02; 6])];
    assert_eq!(repeated.validate(), Err(FabricError::DuplicateItem));

    let mut unplaceable = presence;
    unplaceable.display.as_mut().unwrap().rect.width = 0;
    assert_eq!(
        unplaceable.validate(),
        Err(FabricError::InvalidDisplayPlacement)
    );
}
