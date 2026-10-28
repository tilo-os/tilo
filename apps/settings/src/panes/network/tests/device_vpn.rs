//! The person's devices as VPNs in the VPN list: what the session publishes
//! is shown, and the switches write the person's choices.

use std::rc::Rc;
use std::sync::Arc;

use settings_store::device_vpn::{
    self, ClientState, DeviceVpnStatus, EXIT, ExitUser, OfferedExit, STOP, StopRequest, THROUGH,
    Through,
};

use super::super::NetworkService;
use super::{FakeNetwork, hosted_model, mount_network, rendered_enabled, settle_until};
use crate::settings_app::SettingsApp;

const DESK: &str = "desk-id";
const PHONE: &str = "phone-id";

fn status() -> DeviceVpnStatus {
    DeviceVpnStatus {
        exit_available: true,
        offered: vec![OfferedExit {
            device: DESK.into(),
            label: "Desk PC".into(),
        }],
        exit_users: vec![ExitUser {
            device: PHONE.into(),
            label: "Pixel".into(),
            since_unix_ms: 1,
        }],
        ..DeviceVpnStatus::default()
    }
}

fn document(
    app: &mut ui::framework::testing::TestApp<crate::SettingsShell>,
) -> ui::storage::SettingsDocument {
    app.surface::<SettingsApp, _>(|settings| {
        super::super::area(settings)
            .unwrap()
            .cx
            .storage()
            .settings_document()
            .clone()
    })
    .unwrap()
}

fn shown(app: &mut ui::framework::testing::TestApp<crate::SettingsShell>, id: &str) -> bool {
    rendered_enabled(app.harness().root_widget(), id) == Some(true)
}

/// Each device sharing its connection is a "Through" row whose switch
/// writes the route; this device's own switch writes whether it shares;
/// Stop asks the session to stop a device using it, each request after the
/// last; and a change to the session's status is read again.
#[test]
fn the_vpn_list_routes_through_and_shares_with_the_persons_devices() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.device_vpn.lock().unwrap() = Some(status());
    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
    let mut observed = hosted_model(&mut app, Arc::clone(&fake) as Arc<dyn NetworkService>);
    let watched = Rc::new(crate::watch::Watched::by_hand());
    observed.vpn.devices.follow_by_hand(&watched);
    mount_network(&mut app, observed).unwrap();
    app.harness().resize((1600, 6000)).unwrap();
    app.call("open-pane", serde_json::json!({"pane": "network"}))
        .unwrap();
    app.call("show-vpns", serde_json::json!({})).unwrap();
    app.settle();
    let document = document(&mut app);
    let section = |document: &ui::storage::SettingsDocument| document.section(device_vpn::SECTION);

    let switch = format!("settings.network.vpn.device.switch.{DESK}");
    let stop = format!("settings.network.vpn.exit-user.stop.{PHONE}");
    for id in [switch.as_str(), "settings.network.vpn.exit", stop.as_str()] {
        assert!(shown(&mut app, id), "`{id}` is a live control");
    }

    // Through the desk PC: the route is written and shown as connecting
    // until the session says otherwise.
    app.harness().click_named(&switch).unwrap();
    settle_until(&mut app, |model| {
        model
            .vpn
            .devices
            .status
            .as_ref()
            .is_some_and(|status| status.client.is_some())
    });
    assert_eq!(
        THROUGH.from_section(&section(&document)),
        Some(Through::device(DESK))
    );
    let state = app
        .surface::<SettingsApp, _>(|settings| {
            let model = super::super::area(settings).unwrap().model();
            let model = model.borrow();
            model.vpn.devices.status.as_ref().unwrap().rows()[0]
                .state
                .clone()
        })
        .unwrap();
    assert_eq!(state, Some(ClientState::Connecting));

    // Off again, by the row's action.
    app.call_on(
        &format!("vpn.device:{DESK}"),
        "set-device-vpn",
        serde_json::json!({"on": false}),
    )
    .unwrap();
    app.settle();
    assert_eq!(THROUGH.from_section(&section(&document)), None);

    // This device shares its connection.
    app.harness()
        .click_named("settings.network.vpn.exit")
        .unwrap();
    app.settle();
    assert!(EXIT.from_section(&section(&document)));

    // Stop the phone, after a request made before.
    let mut earlier = serde_json::Map::new();
    earlier.insert(
        STOP.name().into(),
        serde_json::to_value(Some(StopRequest {
            id: 4,
            device: "another".into(),
        }))
        .unwrap(),
    );
    document
        .update_section(device_vpn::SECTION, earlier)
        .unwrap();
    app.harness().click_named(&stop).unwrap();
    app.settle();
    assert_eq!(
        STOP.from_section(&section(&document)),
        Some(StopRequest {
            id: 5,
            device: PHONE.into(),
        })
    );
    assert!(!shown(&mut app, &stop), "the stopped device's row goes");

    // The session publishes the route up: it is read again and shown.
    let mut connected = status();
    connected.client = Some(device_vpn::ClientStatus {
        device: DESK.into(),
        label: "Desk PC".into(),
        state: ClientState::Connected { since_unix_ms: 2 },
        ..device_vpn::ClientStatus::default()
    });
    *fake.device_vpn.lock().unwrap() = Some(connected.clone());
    watched.touch();
    settle_until(&mut app, |model| {
        model.vpn.devices.status.as_ref() == Some(&connected)
    });
}

/// Without a session publishing, the list offers none of this, and the
/// effects refuse.
#[test]
fn without_a_session_the_vpn_list_offers_no_devices() {
    let fake = Arc::new(FakeNetwork::default());
    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
    let observed = hosted_model(&mut app, Arc::clone(&fake) as Arc<dyn NetworkService>);
    mount_network(&mut app, observed).unwrap();
    app.harness().resize((1600, 6000)).unwrap();
    app.call("open-pane", serde_json::json!({"pane": "network"}))
        .unwrap();
    app.call("show-vpns", serde_json::json!({})).unwrap();
    app.settle();
    assert_eq!(
        rendered_enabled(app.harness().root_widget(), "settings.network.vpn.exit"),
        None
    );
    assert!(
        app.call("set-device-exit", serde_json::json!({"on": true}))
            .is_err()
    );
}

/// Two tunnels carrying everything: "Home", up, and "Work"; and "Split",
/// up, which carries only its own networks.
fn tunnels(fake: &FakeNetwork) {
    let tunnel = |name: &str, uuid: &str, active: bool, all_traffic: bool| {
        fake.vpn_settings.lock().unwrap().insert(
            uuid.into(),
            os_actions::network_settings::VpnProfileSettings {
                name: name.into(),
                kind: "wireguard".into(),
                all_traffic,
                ..os_actions::network_settings::VpnProfileSettings::default()
            },
        );
        super::super::VpnInfo {
            name: name.into(),
            uuid: uuid.into(),
            kind: "wireguard".into(),
            service: None,
            active,
        }
    };
    *fake.vpns.lock().unwrap() = vec![
        tunnel("Home", "11111111-1111-4111-8111-111111111111", true, true),
        tunnel("Work", "22222222-2222-4222-8222-222222222222", false, true),
        tunnel("Split", "33333333-3333-4333-8333-333333333333", true, false),
    ];
}

/// The window over `fake`, on the VPN list.
fn on_the_vpn_list(
    fake: &Arc<FakeNetwork>,
) -> ui::framework::testing::TestApp<crate::SettingsShell> {
    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
    let observed = hosted_model(&mut app, Arc::clone(fake) as Arc<dyn NetworkService>);
    mount_network(&mut app, observed).unwrap();
    app.harness().resize((1600, 6000)).unwrap();
    app.call("open-pane", serde_json::json!({"pane": "network"}))
        .unwrap();
    app.call("show-vpns", serde_json::json!({})).unwrap();
    app.settle();
    app
}

/// One tunnel carries everything at a time: turning one on turns off the
/// other that does, and the route through a device; a tunnel carrying only
/// its own networks stays.
#[test]
fn a_vpn_carrying_everything_turns_off_the_other_that_does() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.device_vpn.lock().unwrap() = Some(status());
    tunnels(&fake);
    let mut app = on_the_vpn_list(&fake);
    let document = document(&mut app);
    let mut through = serde_json::Map::new();
    through.insert(
        THROUGH.name().into(),
        serde_json::to_value(Some(Through::device(DESK))).unwrap(),
    );
    document
        .update_section(device_vpn::SECTION, through)
        .unwrap();

    app.harness()
        .click_named("settings.network.vpn.switch.22222222-2222-4222-8222-222222222222")
        .unwrap();
    settle_until(&mut app, |model| model.in_flight == 0);
    assert_eq!(
        fake.changes.lock().unwrap().as_slice(),
        ["vpn Home false", "vpn Work true"]
    );
    let active: Vec<String> = fake
        .vpns
        .lock()
        .unwrap()
        .iter()
        .filter(|vpn| vpn.active)
        .map(|vpn| vpn.name.clone())
        .collect();
    assert_eq!(active, ["Work", "Split"]);
    assert_eq!(
        THROUGH.from_section(&document.section(device_vpn::SECTION)),
        None,
        "the route through the desk PC stopped"
    );
}

/// Routing everything through one of the person's devices turns off the
/// tunnel that carries everything; the kill switch then blocks traffic
/// while the route is down.
#[test]
fn routing_through_a_device_turns_off_the_vpn_carrying_everything() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.device_vpn.lock().unwrap() = Some(status());
    tunnels(&fake);
    let mut app = on_the_vpn_list(&fake);
    let document = document(&mut app);
    app.harness()
        .click_named(&format!("settings.network.vpn.device.switch.{DESK}"))
        .unwrap();
    settle_until(&mut app, |model| model.in_flight == 0);
    assert_eq!(fake.changes.lock().unwrap().as_slice(), ["vpn Home false"]);

    // The session publishes the route it now keeps.
    if let Some(status) = fake.device_vpn.lock().unwrap().as_mut() {
        status.client = Some(device_vpn::ClientStatus {
            device: DESK.into(),
            label: "Desk PC".into(),
            state: ClientState::Connecting,
            ..device_vpn::ClientStatus::default()
        });
    }
    app.with_surface::<SettingsApp, _>(|settings| {
        super::super::area(settings)
            .unwrap()
            .model()
            .borrow_mut()
            .start_snapshot();
    })
    .unwrap();
    settle_until(&mut app, |model| {
        model.snapshot.is_none()
            && model
                .vpn
                .devices
                .status
                .as_ref()
                .is_some_and(|status| status.client.is_some())
    });
    app.settle();
    assert_eq!(
        rendered_enabled(
            app.harness().root_widget(),
            "settings.network.vpn.device.kill-switch"
        ),
        Some(true)
    );
    app.harness()
        .click_named("settings.network.vpn.device.kill-switch")
        .unwrap();
    app.settle();
    assert!(
        THROUGH
            .from_section(&document.section(device_vpn::SECTION))
            .is_some_and(|through| through.kill_switch && through.device == DESK)
    );
}
