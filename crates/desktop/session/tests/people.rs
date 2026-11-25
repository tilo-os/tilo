//! Two people on two devices each, in one workspace at a site, as real
//! sessions: one harness for the collaboration proofs, and its first users.
//!
//! A person is their root; their devices are keys their root certified, each
//! running a session of its own with its own home, identity and state
//! ([`common::people`]). The workspace is a site's: a server of this machine
//! hosts it and admits people to it at an entry a site serves.
//!
//! What this proves: a person joins a workspace at its site's entry and lands
//! in it; each further device of theirs, and of someone they invite, joins by
//! an invitation the workspace's host issues and only that person can redeem,
//! over the collaboration endpoint; and a person the workspace never admitted
//! is refused and keeps nothing; and two people's own desktops sharing a
//! workspace one of them hosts, with no site at all, see each other come and
//! go on the workspace's live session.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the devices live \
              until their sessions end"
)]

use crate::common::people::{Person, Site, join_together};
use tilo_server_protocol::workspaces::Role;

#[test]
fn two_people_on_two_devices_each_meet_in_one_workspace() {
    let ada = Person::new("Ada");
    let bob = Person::new("Bob");
    // Ada's site: her home peer, which hosts her workspace.
    let site = Site::start(&ada, "Studio");
    // The workspace is public to its entry, where anyone the site's own entry admits joins as an
    // editor.
    site.publish(Role::Editor);
    let mut ada_laptop = ada.device("Ada's laptop");
    let mut ada_phone = ada.device("Ada's phone");
    let mut bob_laptop = bob.device("Bob's laptop");
    let mut bob_phone = bob.device("Bob's phone");

    // Ada joins her workspace at the site's entry, as a person joining a workspace a site lists
    // does, and lands in it. Her second device joins by the invitation her site issues for her, and
    // so does each of Bob's, by the invitation it issues for him; every session boots before any is
    // waited for, as no join needs another device of the four.
    let invitation = site.admit(ada.public);
    let her_phone = site.invite(ada.public, Role::Editor);
    let his_laptop = site.invite(bob.public, Role::Editor);
    let his_phone = site.invite(bob.public, Role::Editor);
    join_together(
        &mut [
            &mut ada_laptop,
            &mut ada_phone,
            &mut bob_laptop,
            &mut bob_phone,
        ],
        &[
            invitation.as_str(),
            her_phone.as_str(),
            his_laptop.as_str(),
            his_phone.as_str(),
        ],
    );
    for device in [&ada_laptop, &ada_phone, &bob_laptop, &bob_phone] {
        device.wait_holding(&site.workspace);
    }

    // The workspace's own governance names Ada as its owner and Bob in the part the invitation gave
    // him, and no device of either person is a person of its own: the owner is a member like any
    // other, also where her own device hosts the workspace.
    site.wait_for_member(bob.public);
    let members = site.members();
    assert_eq!(
        members
            .get(&hex::encode(ada.public))
            .map(|mark| mark.role.as_str()),
        Some("owner"),
        "Ada owns the workspace and is listed among its members: {members:?}"
    );
    assert_eq!(
        members
            .get(&hex::encode(bob.public))
            .map(|mark| mark.role.as_str()),
        Some("editor"),
        "Bob is an editor: {members:?}"
    );
    assert_eq!(
        members.len(),
        2,
        "two people are in the workspace, not their four devices: {members:?}"
    );

    // Each of the four sessions keeps the workspace, shows it on its shell,
    // and has it in front: a person who joins a workspace lands in it, so
    // the shell shows that workspace rather than the one it started on.
    for device in [&ada_laptop, &ada_phone, &bob_laptop, &bob_phone] {
        assert!(
            device.held().iter().any(|held| held == &site.workspace),
            "{} holds the workspace it joined, beside the one its own shell made: {:?}",
            device.label,
            device.held()
        );
        device.wait_showing(&site.workspace);
        assert_eq!(
            device.showing().as_deref(),
            Some(site.workspace.as_str()),
            "{} shows the workspace it joined in front",
            device.label
        );
    }

    // Every session says who is here, both ways: each of the other devices in the workspace, by
    // the publisher it goes by, and no row for itself — a roster lists the others, and
    // the person at this device is not one of them.
    let everyone = [&ada_laptop, &ada_phone, &bob_laptop, &bob_phone];
    let publishers: Vec<String> = everyone
        .iter()
        .filter_map(|device| device.publisher())
        .collect();
    assert_eq!(
        publishers.len(),
        everyone.len(),
        "every device says which publisher it is: {publishers:?}"
    );
    for device in everyone {
        let mine = device.publisher().expect("this device's publisher");
        for other in publishers.iter().filter(|publisher| **publisher != mine) {
            device.wait_present(&site.workspace, other);
        }
        let present = device.present(&site.workspace);
        let mut here: Vec<String> = present
            .iter()
            .map(|(publisher, _)| publisher.clone())
            .collect();
        here.sort();
        let mut wanted: Vec<String> = publishers
            .iter()
            .filter(|publisher| **publisher != mine)
            .cloned()
            .collect();
        wanted.sort();
        assert_eq!(
            here, wanted,
            "{} says exactly the other devices are here, once each: {present:?}",
            device.label
        );
        for (publisher, label) in &present {
            assert!(
                !label.trim().is_empty(),
                "{} says what the workspace calls {publisher}: {present:?}",
                device.label
            );
        }
    }
}

/// Two people on their own desktops, neither of them a host, share a workspace one of them hosts:
/// the device hosting it relays its live session, so each sees the other present, and sees them leave.
#[test]
fn two_peoples_own_desktops_see_each_other_come_and_go_without_a_site() {
    let ada = Person::new("Ada");
    let bob = Person::new("Bob");
    let mut ada_desktop = ada.device("Ada's desktop");
    let mut bob_desktop = bob.device("Bob's desktop");
    ada_desktop.start();
    tilo_test_env::wait_until("Ada's shell shows the workspace it made", || {
        ada_desktop.showing().is_some()
    });
    let workspace = ada_desktop
        .showing()
        .expect("the workspace Ada's shell made");
    // The device founds it on its own workspace service off the loop: its invitations are the
    // service's to make once it holds the workspace.
    ada_desktop.wait_holding(&workspace);
    let invitation = crate::common::people::shared_invitation(&ada_desktop, "editor", bob.public);
    bob_desktop.join(&invitation);
    bob_desktop.wait_holding(&workspace);
    bob_desktop.wait_showing(&workspace);

    let ada_publisher = ada_desktop.publisher().expect("Ada's publisher");
    let bob_publisher = bob_desktop.publisher().expect("Bob's publisher");
    ada_desktop.wait_present(&workspace, &bob_publisher);
    bob_desktop.wait_present(&workspace, &ada_publisher);

    bob_desktop.stop();
    tilo_test_env::wait_until(format!("Ada's desktop says Bob left {workspace}"), || {
        !ada_desktop
            .present(&workspace)
            .iter()
            .any(|(publisher, _)| *publisher == bob_publisher)
    });
}

#[test]
fn a_person_the_workspace_never_admitted_is_refused_and_keeps_nothing() {
    let ada = Person::new("Ada");
    let carol = Person::new("Carol");
    let site = Site::start(&ada, "Studio");
    let mut ada_laptop = ada.device("Ada's laptop");
    let mut carol_laptop = carol.device("Carol's laptop");

    // The workspace is not public: its entry admits nobody.
    let refusal = site
        .try_admit(carol.public)
        .expect_err("the entry of a workspace nobody published admits no one");
    assert!(
        refusal.contains("not public"),
        "the entry says why it refuses: {refusal}"
    );

    // Ada, whose site the workspace is, invites Carol, and Carol's device joins.
    site.publish(Role::Editor);
    let ada_joined = site.admit(ada.public);
    ada_laptop.join(&ada_joined);
    ada_laptop.wait_holding(&site.workspace);
    let carol_joined = site.invite(carol.public, Role::Editor);

    // A device that redeems an invitation the workspace issued for someone else is refused, and
    // keeps nothing.
    let stranger = Person::new("Carl");
    let mut stranger_laptop = stranger.device("Carl's laptop");
    stranger_laptop.join(&carol_joined);
    tilo_test_env::wait_until("Carl's session refuses the invitation", || {
        stranger_laptop.log().contains("the invitation")
    });
    assert!(
        stranger_laptop
            .log()
            .contains("that invitation is not for you"),
        "the session says the invitation is not this person's:\n{}",
        stranger_laptop.log()
    );
    assert!(
        !stranger_laptop
            .held()
            .iter()
            .any(|held| held == &site.workspace),
        "and the device keeps nothing of the invitation: {:?}",
        stranger_laptop.held()
    );

    // Carol, whom it was issued for, joins with it.
    carol_laptop.join(&carol_joined);
    carol_laptop.wait_holding(&site.workspace);
    site.wait_for_member(carol.public);
}
