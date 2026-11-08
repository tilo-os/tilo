//! Two people on two devices each, in one workspace at a site, as real
//! sessions: one harness for the collaboration proofs, and its first users.
//!
//! A person is their root; their devices are keys their root certified, each
//! running a session of its own with its own home, identity and state
//! ([`common::people`]). The workspace is a site's: a relay of this machine
//! governs it, hosts it and admits people to it at a door a site serves.
//!
//! What this proves: a person joins a workspace at its site's door and lands
//! in it; each further device of theirs, and of someone they invite, joins by
//! an invitation the workspace's host issues and only that person can redeem,
//! over the collaboration endpoint; and a person the workspace never admitted
//! is refused and keeps nothing.

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

mod common;

// A site admits its members' records through an Automerge worker: this binary is that worker, as
// the session's own is.
tilo_automerge_isolate::test_worker!();

use common::people::{Person, Site};
use tilo_relay_protocol::workspaces::WorkspaceRole;

#[test]
fn two_people_on_two_devices_each_meet_in_one_workspace() {
    let ada = Person::new("Ada");
    let bob = Person::new("Bob");
    // Ada's site: her home peer, which governs her workspace and hosts it.
    let site = Site::start(&ada, "Studio");
    // The workspace is public to its door, where anyone the site's own door admits joins as an
    // editor.
    site.publish(WorkspaceRole::Editor);
    let mut ada_laptop = ada.device("Ada's laptop");
    let mut ada_phone = ada.device("Ada's phone");
    let mut bob_laptop = bob.device("Bob's laptop");
    let mut bob_phone = bob.device("Bob's phone");

    // Ada joins her workspace at the site's door, as a person joining a workspace a site lists
    // does, and lands in it.
    let invitation = site.admit(ada.public);
    ada_laptop.join(&invitation);
    ada_laptop.wait_holding(&site.workspace);

    // Her second device joins by the invitation her site issues for her, and so does each of Bob's,
    // by the invitation it issues for him.
    let her_phone = site.invite(ada.public, WorkspaceRole::Editor);
    ada_phone.join(&her_phone);
    ada_phone.wait_holding(&site.workspace);
    for device in [&mut bob_laptop, &mut bob_phone] {
        let theirs = site.invite(bob.public, WorkspaceRole::Editor);
        device.join(&theirs);
        device.wait_holding(&site.workspace);
    }

    // The workspace's own governance names Bob, in the part the invitation gave him, and no device
    // of either person is a person of its own: the workspace's owner is its own, not one of its
    // members.
    site.wait_for_member(bob.public);
    let members = site.members();
    assert_eq!(
        members
            .get(&hex::encode(ada.public))
            .map(|mark| mark.role.as_str()),
        None,
        "Ada's site owns the workspace rather than holding a member's part: {members:?}"
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
        1,
        "one person joined the workspace, not their two devices: {members:?}"
    );

    // Each of the four sessions keeps the workspace, shows it on its desk,
    // and has it in front: a person who joins a workspace lands in it, so
    // the desk shows that workspace rather than the one it started on.
    for device in [&ada_laptop, &ada_phone, &bob_laptop, &bob_phone] {
        assert!(
            device.held().iter().any(|held| held == &site.workspace),
            "{} holds the workspace it joined, beside the one its own desk made: {:?}",
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
}

#[test]
fn a_person_the_workspace_never_admitted_is_refused_and_keeps_nothing() {
    let ada = Person::new("Ada");
    let carol = Person::new("Carol");
    let site = Site::start(&ada, "Studio");
    let mut ada_laptop = ada.device("Ada's laptop");
    let mut carol_laptop = carol.device("Carol's laptop");

    // The workspace is not public: its door admits nobody.
    let refusal = site
        .try_admit(carol.public)
        .expect_err("the door of a workspace nobody published admits no one");
    assert!(
        refusal.contains("not public"),
        "the door says why it refuses: {refusal}"
    );

    // Ada, whose site the workspace is, invites Carol, and Carol's device joins.
    site.publish(WorkspaceRole::Editor);
    let ada_joined = site.admit(ada.public);
    ada_laptop.join(&ada_joined);
    ada_laptop.wait_holding(&site.workspace);
    let carol_joined = site.invite(carol.public, WorkspaceRole::Editor);

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
