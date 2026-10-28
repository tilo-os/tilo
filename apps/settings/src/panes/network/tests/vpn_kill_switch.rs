//! A VPN profile's kill switch on its page: the switch and its action write
//! the person's choice, the session's block shows as the profile's state,
//! and turning the profile off asks the session to lift it. And what a
//! `WireGuard` tunnel routes is a list, each address added and removed on
//! its own.

use std::rc::Rc;
use std::sync::Arc;

use settings_store::device_vpn::{
    self, DeviceVpnStatus, PROFILE_KILL_SWITCHES, PROFILE_RELEASE, ProfileBlockStatus,
    ProfileKillSwitch, ProfileRelease,
};
use ui::widget::Widget;

use super::super::{NetworkService, SAMPLE_VPN_HOME, SampleNetwork};
use super::{FakeNetwork, hosted_model, mount_network, rendered_enabled, settle_until, work_vpn};
use crate::i18n::t;
use crate::settings_app::SettingsApp;

type App = ui::framework::testing::TestApp<crate::SettingsShell>;

fn document(app: &mut App) -> ui::storage::SettingsDocument {
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

/// Whether the switch `id` is drawn on, where it is drawn.
fn checked(widget: &dyn Widget, id: &str) -> Option<bool> {
    if widget.test_id() == Some(id) {
        return widget
            .accessibility()
            .map(|node| node.state.checked == Some(true));
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| checked(child, id))
}

/// Whether the page says `text`.
fn draws(app: &mut App, text: &str) -> bool {
    app.surface::<SettingsApp, _>(|settings| {
        let model = super::super::area(settings).unwrap().model();
        let model = model.borrow();
        super::shown(&model).iter().any(|line| line.contains(text))
    })
    .unwrap()
}

/// The window over `fake`, on the page of the "Work" tunnel, its session's
/// status followed by hand.
fn on_the_profile(fake: &Arc<FakeNetwork>) -> (App, Rc<crate::watch::Watched>) {
    let vpn = work_vpn();
    *fake.vpns.lock().unwrap() = vec![vpn.clone()];
    fake.vpn_settings.lock().unwrap().insert(
        vpn.uuid.clone(),
        SampleNetwork::vpn_settings(SAMPLE_VPN_HOME).unwrap(),
    );
    let mut app = App::new().unwrap();
    let mut observed = hosted_model(&mut app, Arc::clone(fake) as Arc<dyn NetworkService>);
    let watched = Rc::new(crate::watch::Watched::by_hand());
    observed.vpn.devices.follow_by_hand(&watched);
    mount_network(&mut app, observed).unwrap();
    app.harness().resize((1600, 6000)).unwrap();
    app.call("open-pane", serde_json::json!({"pane": "network"}))
        .unwrap();
    app.call_on(
        &format!("vpn.connection:{}", vpn.uuid),
        "show-vpn-profile",
        serde_json::json!({}),
    )
    .unwrap();
    settle_until(&mut app, |model| model.vpn.settings.contains_key(&vpn.uuid));
    app.settle();
    (app, watched)
}

/// The kill switch is off until the person turns it on, by its switch or
/// its action; while on, "Allow local network" is offered and kept when the
/// switch is turned on again by the action alone; and off forgets it.
#[test]
fn a_vpns_kill_switch_is_off_until_turned_on_and_offers_the_local_network() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.device_vpn.lock().unwrap() = Some(DeviceVpnStatus::default());
    let (mut app, _watched) = on_the_profile(&fake);
    let uuid = work_vpn().uuid;
    let row = format!("vpn.connection:{uuid}");
    let document = document(&mut app);
    let switches = |document: &ui::storage::SettingsDocument| {
        PROFILE_KILL_SWITCHES.from_section(&document.section(device_vpn::SECTION))
    };
    let switch = format!("settings.network.vpn.kill-switch.{uuid}");
    let local = format!("settings.network.vpn.allow-local.{uuid}");

    assert_eq!(checked(app.harness().root_widget(), &switch), Some(false));
    assert_eq!(
        rendered_enabled(app.harness().root_widget(), &switch),
        Some(true)
    );
    assert_eq!(
        rendered_enabled(app.harness().root_widget(), &local),
        None,
        "the local network is offered only while the switch is on"
    );
    assert!(switches(&document).is_empty());

    app.harness().click_named(&switch).unwrap();
    app.settle();
    assert_eq!(
        switches(&document).get(&uuid),
        Some(&ProfileKillSwitch { allow_local: false })
    );
    assert_eq!(checked(app.harness().root_widget(), &switch), Some(true));
    assert_eq!(checked(app.harness().root_widget(), &local), Some(false));

    app.harness().click_named(&local).unwrap();
    app.settle();
    assert_eq!(
        switches(&document).get(&uuid),
        Some(&ProfileKillSwitch { allow_local: true })
    );

    // The action, on the profile's row: on again keeps the local network.
    app.call_on(&row, "set-kill-switch", serde_json::json!({"on": true}))
        .unwrap();
    app.settle();
    assert_eq!(
        switches(&document).get(&uuid),
        Some(&ProfileKillSwitch { allow_local: true })
    );
    app.call_on(
        &row,
        "set-kill-switch",
        serde_json::json!({"on": true, "allow_local": false}),
    )
    .unwrap();
    app.settle();
    assert_eq!(
        switches(&document).get(&uuid),
        Some(&ProfileKillSwitch { allow_local: false })
    );
    app.call_on(&row, "set-kill-switch", serde_json::json!({"on": false}))
        .unwrap();
    app.settle();
    assert!(switches(&document).is_empty());
    assert_eq!(checked(app.harness().root_widget(), &switch), Some(false));
    assert!(
        app.call(
            "set-kill-switch",
            serde_json::json!({"connection": "Nowhere", "on": true}),
        )
        .is_err(),
        "a VPN that is not there has no kill switch"
    );
}

/// While the session holds a profile's block, its page says "Blocking
/// traffic" with its switch on; turning the switch off asks the session to
/// lift the block, each request after the last, and brings nothing up or
/// down; and the session failing to block is said.
#[test]
fn a_blocking_vpn_shows_it_and_turning_it_off_lifts_the_block() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.device_vpn.lock().unwrap() = Some(DeviceVpnStatus::default());
    let (mut app, watched) = on_the_profile(&fake);
    let uuid = work_vpn().uuid;
    let row = format!("vpn.connection:{uuid}");
    let document = document(&mut app);
    app.call_on(&row, "set-kill-switch", serde_json::json!({"on": true}))
        .unwrap();
    app.settle();
    let vpn_switch = format!("settings.network.vpn.switch.{uuid}");
    assert!(!draws(&mut app, &t("net-vpn-blocking")));
    assert_eq!(
        checked(app.harness().root_widget(), &vpn_switch),
        Some(false)
    );

    // The tunnel dropped, not on purpose: the session blocks.
    let blocking = DeviceVpnStatus {
        profile_block: ProfileBlockStatus {
            blocking: vec![uuid.clone()],
            failed: false,
        },
        ..DeviceVpnStatus::default()
    };
    *fake.device_vpn.lock().unwrap() = Some(blocking.clone());
    watched.touch();
    settle_until(&mut app, |model| {
        model.vpn.devices.status.as_ref() == Some(&blocking)
    });
    app.settle();
    assert!(draws(&mut app, &t("net-vpn-blocking")), "the state says so");
    assert_eq!(
        checked(app.harness().root_widget(), &vpn_switch),
        Some(true),
        "a blocking VPN is still set to connect"
    );
    assert!(!draws(&mut app, &t("net-vpn-kill-switch-failed")));

    // Off, on purpose: the session is asked to lift the block, after a
    // request made before.
    let mut earlier = serde_json::Map::new();
    earlier.insert(
        PROFILE_RELEASE.name().into(),
        serde_json::to_value(Some(ProfileRelease {
            id: 6,
            profile: "another".into(),
        }))
        .unwrap(),
    );
    document
        .update_section(device_vpn::SECTION, earlier)
        .unwrap();
    app.harness().click_named(&vpn_switch).unwrap();
    // The session lifts it and says so, read whenever it is read again.
    *fake.device_vpn.lock().unwrap() = Some(DeviceVpnStatus::default());
    settle_until(&mut app, |model| model.in_flight == 0);
    assert_eq!(
        PROFILE_RELEASE.from_section(&document.section(device_vpn::SECTION)),
        Some(ProfileRelease {
            id: 7,
            profile: uuid,
        })
    );
    assert!(
        fake.changes.lock().unwrap().is_empty(),
        "a tunnel that is down is not brought down again"
    );
    app.settle();
    assert!(!draws(&mut app, &t("net-vpn-blocking")));
    assert_eq!(
        checked(app.harness().root_widget(), &vpn_switch),
        Some(false)
    );

    // The session could not block.
    let failed = DeviceVpnStatus {
        profile_block: ProfileBlockStatus {
            blocking: Vec::new(),
            failed: true,
        },
        ..DeviceVpnStatus::default()
    };
    *fake.device_vpn.lock().unwrap() = Some(failed.clone());
    watched.touch();
    settle_until(&mut app, |model| {
        model.vpn.devices.status.as_ref() == Some(&failed)
    });
    app.settle();
    assert!(draws(&mut app, &t("net-vpn-kill-switch-failed")));
}

/// Turning off a connected VPN whose kill switch is on asks the session to
/// lift its block before it goes down; one without a kill switch writes
/// nothing.
#[test]
fn turning_off_a_connected_vpn_with_its_kill_switch_releases_it() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.device_vpn.lock().unwrap() = Some(DeviceVpnStatus::default());
    let (mut app, _watched) = on_the_profile(&fake);
    let uuid = work_vpn().uuid;
    let row = format!("vpn.connection:{uuid}");
    let document = document(&mut app);
    let release = |document: &ui::storage::SettingsDocument| {
        PROFILE_RELEASE.from_section(&document.section(device_vpn::SECTION))
    };

    app.call_on(&row, "set-vpn-connected", serde_json::json!({"on": true}))
        .unwrap();
    settle_until(&mut app, |model| model.in_flight == 0);
    app.call_on(&row, "set-vpn-connected", serde_json::json!({"on": false}))
        .unwrap();
    settle_until(&mut app, |model| model.in_flight == 0);
    assert_eq!(release(&document), None, "no kill switch, nothing to lift");

    app.call_on(&row, "set-kill-switch", serde_json::json!({"on": true}))
        .unwrap();
    app.call_on(&row, "set-vpn-connected", serde_json::json!({"on": true}))
        .unwrap();
    settle_until(&mut app, |model| model.in_flight == 0);
    app.call_on(&row, "set-vpn-connected", serde_json::json!({"on": false}))
        .unwrap();
    settle_until(&mut app, |model| model.in_flight == 0);
    assert_eq!(
        release(&document).map(|release| release.profile),
        Some(uuid)
    );
    assert_eq!(
        fake.changes.lock().unwrap().as_slice(),
        [
            "vpn Work true",
            "vpn Work false",
            "vpn Work true",
            "vpn Work false"
        ]
    );
}

/// What a `WireGuard` tunnel routes is a list: each address is added on
/// its own, what is no address with a prefix is refused, and each is
/// removed by its own button.
#[test]
fn a_tunnels_routed_addresses_are_a_list() {
    let fake = Arc::new(FakeNetwork::default());
    let (mut app, _watched) = on_the_profile(&fake);
    let uuid = work_vpn().uuid;
    let row = format!("vpn.connection:{uuid}");
    let held = |fake: &FakeNetwork| fake.vpn_settings.lock().unwrap()[&uuid].allowed_ips.clone();
    assert_eq!(held(&fake), ["0.0.0.0/0", "::/0"]);
    assert_eq!(
        rendered_enabled(
            app.harness().root_widget(),
            &format!("network.vpn.allowed.add.{uuid}")
        ),
        Some(true),
        "the field adds one address"
    );

    for address in ["10.20.0.0/16", "fd00::/8", "192.0.2.7"] {
        app.call_on(
            &row,
            "add-vpn-routed-address",
            serde_json::json!({"address": address}),
        )
        .unwrap();
        settle_until(&mut app, |model| model.in_flight == 0);
    }
    assert_eq!(
        held(&fake),
        ["0.0.0.0/0", "::/0", "10.20.0.0/16", "fd00::/8", "192.0.2.7"]
    );
    for wrong in [
        "10.20.0.0/33",
        "fd00::/129",
        "vpn.example",
        "10.0.0.0/8, 10.1.0.0/16",
    ] {
        assert!(
            app.call_on(
                &row,
                "add-vpn-routed-address",
                serde_json::json!({"address": wrong}),
            )
            .is_err(),
            "{wrong} is refused"
        );
    }
    app.settle();
    app.harness()
        .click_named("settings.network.vpn.allowed.remove.0.0.0.0/0")
        .unwrap();
    settle_until(&mut app, |model| model.in_flight == 0);
    app.call_on(
        &row,
        "remove-vpn-routed-address",
        serde_json::json!({"address": "::/0"}),
    )
    .unwrap();
    settle_until(&mut app, |model| model.in_flight == 0);
    assert_eq!(held(&fake), ["10.20.0.0/16", "fd00::/8", "192.0.2.7"]);
}
