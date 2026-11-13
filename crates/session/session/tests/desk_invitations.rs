//! A person shares a workspace by handing out an invitation into it, and the apps they run share
//! what they show the same way: the desk makes the link, and it admits exactly the person it names,
//! at the part it grants. A member whose own part does not let them invite is refused, so an app
//! can only share as far as the person using it may.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use crate::common::people::{Person, shared_invitation};
use serde_json::json;

/// The invitation the desk hands out admits the person it was made for: their device joins the
/// workspace the desk shows and holds it, and the site records them as a member.
#[test]
fn the_desks_invitation_admits_the_person_it_names() {
    let (site, _ada, bob, ada_laptop) = crate::common::people::a_site_with_files_for_one_laptop();
    let invitation = crate::common::people::shared_invitation(&ada_laptop, "editor", bob.public);
    let mut bob_laptop = bob.device("Bob's laptop");
    bob_laptop.install_files();
    bob_laptop.join(&invitation);
    bob_laptop.wait_holding(&site.workspace);
    site.wait_for_member(bob.public);
}

/// A member whose part is to read is refused the invitation an app asks the desk for: sharing is
/// inviting into the workspace, and only what the person using the app may do reaches another
/// person through it.
#[test]
fn a_member_whose_part_does_not_let_them_invite_is_refused() {
    let (site, _ada, bob, _ada_laptop) = crate::common::people::a_site_with_files_for_one_laptop();
    let mut bob_laptop = bob.device("Bob's laptop");
    bob_laptop.install_files();
    let invitation = site.invite(bob.public, tilo_relay_protocol::workspaces::Role::Viewer);
    bob_laptop.join(&invitation);
    bob_laptop.wait_holding(&site.workspace);
    site.wait_for_member(bob.public);

    let answer = crate::common::people::desk_answer(&bob_laptop, &json!({"role": "editor"}));
    assert_eq!(
        answer["err"]["code"],
        json!("refused"),
        "a reader's invitation request is refused: {answer}\n{}",
        bob_laptop.diagnostic()
    );
    assert!(
        answer.to_string().contains("may not invite"),
        "the refusal says which part is missing: {answer}"
    );
}

/// The link a device hands out right after its session starts names the relay it is reached
/// through: an address signed before its endpoint reached one names only the network the device
/// stands in, and reaches nobody the link is given to. Nothing else is in the workspace, so making
/// the link is what makes the device reachable at all.
#[test]
fn a_link_made_right_after_start_names_the_relay_the_device_is_reached_through() {
    let ada = Person::new("Ada");
    let mut laptop = ada.device("Ada's laptop");
    laptop.start();
    // Made on this desk, so its record names this device as its own host and its person may
    // invite into it.
    let made = laptop.act_wait("actions", "workspace.create", &json!({}));
    assert_eq!(
        made["answer"]["receipt"]["update"]["state"],
        json!("succeeded"),
        "{} makes a workspace: {made}\n{}",
        laptop.label,
        laptop.diagnostic()
    );
    let link = shared_invitation(&laptop, "editor", ada.public);
    let join = tilo_workspace_join::decode_and_verify_invitation_text(
        &link,
        None,
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap_or_else(|error| {
        panic!(
            "{}'s link is an invitation: {error}\n{}",
            laptop.label,
            laptop.diagnostic()
        )
    });
    let hint =
        tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1::from_transport_hint(
            &join.grant().transport[0],
        )
        .expect("the invitation names its host's address");
    let relay = test_relay::loopback_url();
    assert!(
        hint.endpoint_addr
            .relay_urls()
            .any(|url| url.as_str() == relay),
        "the link names the relay this device is reached through ({relay}): {:?}\n{}",
        hint.endpoint_addr,
        laptop.diagnostic()
    );
}
