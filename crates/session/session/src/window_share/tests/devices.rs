//! What the desk calls the workspace's other devices: the names this session heard them give
//! themselves, kept while their windows are on the desk, and what a window open on one of them
//! says with them.

use std::collections::BTreeSet;

use collab_types::presence::{Payload, Presence};

use super::super::HeardDevices;
use super::*;

/// What a window open on a device this one cannot name says.
const ELSEWHERE: &str = "Open on another device. Share the window there to see it on this one.";
/// What a window open on the device called Framework says.
const ON_FRAMEWORK: &str = "Open on Framework. Share the window there to see it on this one.";

/// The salt `device` mints its ids under.
fn salt_of(device: &EndpointId) -> u16 {
    tilo_desktop_model::ids::device_salt_of_key(device.as_bytes())
}

/// `device` heard calling itself `name`.
fn called(device: EndpointId, name: &str) -> (EndpointId, String) {
    (device, name.to_owned())
}

/// The salts of a desk that lists a window of each of `devices`.
fn listing(devices: &[EndpointId]) -> BTreeSet<u16> {
    devices.iter().map(salt_of).collect()
}

/// One of the devices whose keys fold to one salt: `twin(28)` and `twin(60)` are two of them.
fn twin(n: u16) -> EndpointId {
    let mut seed = [0x5A_u8; 32];
    seed[..2].copy_from_slice(&n.to_le_bytes());
    iroh::SecretKey::from_bytes(&seed).public()
}

/// What the desk calls the device of a window open elsewhere comes from the devices heard in the
/// workspaces: each under the salt its key derives. A device heard with no name has none, this
/// device's own salt names nothing, and neither does a salt the keys of two devices fold to.
#[test]
fn the_devices_heard_are_named_by_the_salt_each_mints_under() {
    let own = salt_of(&device(0x20));
    let framework = (salt_of(&device(0x21)), "Framework".to_owned());
    let desk = listing(&[
        device(0x20),
        device(0x21),
        device(0x22),
        device(0x23),
        twin(28),
    ]);
    let heard = [
        called(device(0x21), "Framework"),
        // Heard with no name, and with a name of nothing but space.
        called(device(0x22), ""),
        called(device(0x23), "  "),
        // Heard in a second workspace too, one of them with the name it lacked in the first.
        called(device(0x21), "Framework"),
        called(device(0x22), "Phone"),
        // A window minted under this device's own salt is not another device's.
        called(device(0x20), "This one"),
    ];
    assert_eq!(
        HeardDevices::default().named(own, &heard, &desk),
        BTreeMap::from([
            framework.clone(),
            (salt_of(&device(0x22)), "Phone".to_owned()),
        ]),
    );
    assert!(
        HeardDevices::default().named(own, &[], &desk).is_empty(),
        "nobody heard, nobody named"
    );

    // Two devices whose keys fold to one salt: the desk could not tell whose a window is, so it
    // names neither, and still names the device beside them.
    let (laptop, tablet) = (twin(28), twin(60));
    assert_ne!(laptop, tablet, "two devices");
    assert_eq!(
        salt_of(&laptop),
        salt_of(&tablet),
        "whose keys fold to one salt"
    );
    assert_eq!(
        HeardDevices::default().named(
            own,
            &[
                called(laptop, "Laptop"),
                called(device(0x21), "Framework"),
                called(tablet, "Tablet"),
            ],
            &desk,
        ),
        BTreeMap::from([framework]),
    );
}

/// A device heard once is still that device when it goes silent: asleep or offline, the window
/// open on it says which device to wake, pass after pass, for as long as the session runs.
#[test]
fn a_device_heard_and_then_silent_keeps_its_name() {
    let own = salt_of(&device(0x20));
    let framework = device(0x21);
    let desk = listing(&[framework]);
    let named = BTreeMap::from([(salt_of(&framework), "Framework".to_owned())]);
    let mut devices = HeardDevices::default();
    assert_eq!(
        devices.named(own, &[called(framework, "Framework")], &desk),
        named
    );
    assert_eq!(
        devices.named(own, &[], &desk),
        named,
        "a device nobody hears now is still the device it was"
    );
    assert_eq!(
        devices.named(own, &[called(framework, "")], &desk),
        named,
        "heard again before it said what it is called, it keeps the name it gave"
    );
    assert_eq!(devices.named(own, &[], &desk), named, "pass after pass");
}

/// A device heard again under a new name is called by the new one, and keeps that one when it
/// goes silent again.
#[test]
fn a_device_heard_again_under_a_new_name_is_renamed() {
    let own = salt_of(&device(0x20));
    let framework = device(0x21);
    let desk = listing(&[framework]);
    let renamed = BTreeMap::from([(salt_of(&framework), "Workshop".to_owned())]);
    let mut devices = HeardDevices::default();
    assert_eq!(
        devices.named(own, &[called(framework, "Framework")], &desk),
        BTreeMap::from([(salt_of(&framework), "Framework".to_owned())])
    );
    assert_eq!(
        devices.named(own, &[], &desk).len(),
        1,
        "silent, it keeps its name"
    );
    assert_eq!(
        devices.named(own, &[called(framework, "Workshop")], &desk),
        renamed,
        "the name it gives now takes the old one's place"
    );
    assert_eq!(
        devices.named(own, &[], &desk),
        renamed,
        "and is the one it keeps"
    );
}

/// Two devices whose keys fold to one salt name nobody once the session heard both, though
/// never together: a window under that salt is either's, and the desk never names the wrong one,
/// even when only one of them is heard now.
#[test]
fn two_devices_heard_under_one_salt_at_different_times_name_nobody_afterwards() {
    let own = salt_of(&device(0x20));
    let (laptop, tablet) = (twin(28), twin(60));
    let desk = listing(&[laptop]);
    let mut devices = HeardDevices::default();
    assert_eq!(
        devices.named(own, &[called(laptop, "Laptop")], &desk),
        BTreeMap::from([(salt_of(&laptop), "Laptop".to_owned())]),
        "one device heard under the salt is that device"
    );
    assert!(
        devices
            .named(own, &[called(tablet, "Tablet")], &desk)
            .is_empty(),
        "a second device under the salt: the window is either's"
    );
    assert!(
        devices
            .named(own, &[called(laptop, "Laptop")], &desk)
            .is_empty(),
        "only the first is heard now, and the window is still either's"
    );
    assert!(devices.named(own, &[], &desk).is_empty(), "and stays so");

    // The first was heard while the desk listed no window of its: it was heard all the same.
    let mut devices = HeardDevices::default();
    assert!(
        devices
            .named(own, &[called(laptop, "Laptop")], &BTreeSet::new())
            .is_empty()
    );
    assert!(
        devices
            .named(own, &[called(tablet, "Tablet")], &desk)
            .is_empty(),
        "the session heard both, so the salt names neither"
    );
}

/// What is remembered of a device goes with the last of its windows: a workspace that lists the
/// device's windows no longer, or one the person left, leaves nothing to say of it, and its
/// windows back on the desk name it again only once it is heard again.
#[test]
fn a_device_whose_windows_left_the_workspace_is_forgotten() {
    let own = salt_of(&device(0x20));
    let framework = device(0x21);
    let desk = listing(&[framework]);
    let nothing = BTreeSet::new();
    let named = BTreeMap::from([(salt_of(&framework), "Framework".to_owned())]);
    let mut devices = HeardDevices::default();
    assert_eq!(
        devices.named(own, &[called(framework, "Framework")], &desk),
        named
    );
    assert!(
        devices
            .named(own, &[called(framework, "Framework")], &nothing)
            .is_empty(),
        "no window on the desk is that device's to name, heard or not"
    );
    assert!(
        devices.named(own, &[], &desk).is_empty(),
        "what was remembered went with its windows: back while it is silent, they name nobody"
    );
    assert_eq!(
        devices.named(own, &[called(framework, "Framework")], &desk),
        named,
        "heard again, it is named again"
    );

    // Silent, then its windows leave: nothing is kept for when they come back.
    assert_eq!(devices.named(own, &[], &desk), named);
    assert!(devices.named(own, &[], &nothing).is_empty());
    assert!(
        devices.named(own, &[], &desk).is_empty(),
        "a silent device's windows that left and came back name nobody"
    );
}

/// What the accessibility tree under `widget` describes itself as: a notice says there what it
/// says on screen.
fn described(widget: &dyn ui::widget::Widget, out: &mut Vec<String>) {
    fn node(access: &ui::a11y::AccessNode, out: &mut Vec<String>) {
        out.extend(access.description.clone());
        for child in &access.children {
            node(child, out);
        }
    }
    if let Some(access) = widget.accessibility() {
        node(&access, out);
    }
    widget.for_each_child(&mut |child| described(child, out));
}

/// Step `member` 's session until its desk says `notice` somewhere, or for a while: what the
/// desk says then.
fn until_said(member: &mut Member, notice: &str) -> Vec<String> {
    let mut said = Vec::new();
    for _ in 0..500 {
        member.session.step(Duration::from_millis(1)).expect("step");
        said = member.session.desktop().with_widgets(|widgets| {
            widgets.relayout();
            let mut said = Vec::new();
            described(widgets.host_mut().root_widget(), &mut said);
            said
        });
        if said.iter().any(|text| text == notice) {
            break;
        }
    }
    said
}

/// A window of the workspace open on another device says which device on this one's desk once
/// that device was heard in the workspace, by the name a shared window's host goes by: the tab
/// carries the id that device gave it, and nothing else says where the window is.
#[test]
fn a_window_open_on_another_device_names_the_device_heard_in_the_workspace() {
    let salt = tilo_desktop_model::ids::device_salt_of_key(device(0x21).as_bytes());
    let tab = tilo_desktop_model::ids::compose_id(salt, 1);
    let mut viewer = member_minting_under(
        0x22,
        desk(
            TileContent::PeerWindow {
                app_id: APP.to_owned(),
                label: "Notes".to_owned(),
                note: String::new(),
                missing: None,
            },
            tab,
        ),
        1,
    );
    let said = until_said(&mut viewer, ELSEWHERE);
    assert!(
        said.iter().any(|text| text == ELSEWHERE),
        "a device nobody heard is another device: {said:?}"
    );

    // The device is heard before its certificate is: it says whose it is, and gives no name of
    // its own. A person's name names them and none of their devices, so the window names none.
    viewer.sync.heard_presence(
        WORKSPACE,
        device(0x21),
        &Presence::desktop(
            WORKSPACE,
            Payload::Person {
                name: "Ada".to_owned(),
            },
        ),
    );
    assert_eq!(
        viewer.sync.workspace_devices(WORKSPACE),
        [(device(0x21), "Ada".to_owned())],
        "the device is heard, known by its person's name alone"
    );
    let said = said_after(&mut viewer, 40);
    assert!(
        said.iter().any(|text| text == ELSEWHERE),
        "a device that gave itself no name is another device: {said:?}"
    );
    assert!(
        !said.iter().any(|text| text.contains("Ada")),
        "and is never called by its person's name: {said:?}"
    );

    heard(&viewer, 0x21, "Framework");
    let said = until_said(&mut viewer, ON_FRAMEWORK);
    assert!(
        said.iter().any(|text| text == ON_FRAMEWORK),
        "the window names the device it is open on: {said:?}"
    );
    assert!(
        !said.iter().any(|text| text == ELSEWHERE),
        "and no longer says another device: {said:?}"
    );
}

/// What `member` 's desk says after `passes` more passes of its session.
fn said_after(member: &mut Member, passes: usize) -> Vec<String> {
    for _ in 0..passes {
        member.session.step(Duration::from_millis(1)).expect("step");
    }
    member.session.desktop().with_widgets(|widgets| {
        widgets.relayout();
        let mut said = Vec::new();
        described(widgets.host_mut().root_widget(), &mut said);
        said
    })
}

/// A window open on a device that went silent still says which device it is on: the device left
/// the workspace's plane, as one that sleeps does, and the name this session heard of it stays
/// for as long as its window is on the desk. The window gone and back while the device is
/// silent names no device: nothing was kept of a device with no window here.
#[test]
fn a_window_open_on_a_device_that_went_silent_still_names_it() {
    let salt = salt_of(&device(0x21));
    let tab = tilo_desktop_model::ids::compose_id(salt, 1);
    let elsewhere = || {
        desk(
            TileContent::PeerWindow {
                app_id: APP.to_owned(),
                label: "Notes".to_owned(),
                note: String::new(),
                missing: None,
            },
            tab,
        )
    };
    let mut viewer = member_minting_under(0x22, elsewhere(), 1);
    heard(&viewer, 0x21, "Framework");
    let said = until_said(&mut viewer, ON_FRAMEWORK);
    assert!(
        said.iter().any(|text| text == ON_FRAMEWORK),
        "the window names the device it is open on: {said:?}"
    );
    assert_eq!(
        viewer.sync.workspace_devices(WORKSPACE).len(),
        1,
        "the device is heard in the workspace"
    );

    // The device goes: the workspace hears it no longer.
    let ws = viewer
        .sync
        .workspace_topic(WORKSPACE)
        .expect("the workspace's topic");
    viewer.sync.hear(Event::PeerLeft {
        ws,
        peer: device(0x21),
    });
    assert!(
        viewer.sync.workspace_devices(WORKSPACE).is_empty(),
        "nobody is heard in the workspace now"
    );
    let said = said_after(&mut viewer, 40);
    assert!(
        said.iter().any(|text| text == ON_FRAMEWORK),
        "the window still names the device it is open on: {said:?}"
    );
    assert!(
        !said.iter().any(|text| text == ELSEWHERE),
        "and does not fall back to another device: {said:?}"
    );

    // Its window leaves the desk and comes back while the device is still silent.
    viewer.session.handle().replace_tree(own_desk());
    let said = said_after(&mut viewer, 40);
    assert!(
        !said.iter().any(|text| text == ON_FRAMEWORK),
        "the window is gone from the desk: {said:?}"
    );
    viewer.session.handle().replace_tree(elsewhere());
    let said = until_said(&mut viewer, ELSEWHERE);
    assert!(
        said.iter().any(|text| text == ELSEWHERE),
        "nothing was kept of a device with no window here: {said:?}"
    );
    assert!(
        !said.iter().any(|text| text == ON_FRAMEWORK),
        "so the window back names no device: {said:?}"
    );
}
