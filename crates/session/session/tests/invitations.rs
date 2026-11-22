//! A person shares a workspace by handing out an invitation into it, and the apps they run share
//! what they show the same way: the shell makes the link, and it admits exactly the person it names,
//! at the part it grants. A member whose own part does not let them invite is refused, so an app
//! can only share as far as the person using it may.

#![cfg(target_os = "linux")]

use crate::common::people::{Person, new_workspace, shared_invitation};
use serde_json::json;

/// The invitation the shell hands out admits the person it was made for: their device joins the
/// workspace the shell shows, holds it, and shows it on their own shell.
#[test]
fn the_shells_invitation_admits_the_person_it_names() {
    let ada = Person::new("Ada");
    let bob = Person::new("Bob");
    let mut laptop = ada.device("Ada's laptop");
    let mut bob_laptop = bob.device("Bob's laptop");
    laptop.start();
    // A workspace the person made here is held on this device's own workspace service, so its
    // invitations are this shell's to hand out.
    tilo_test_env::wait_until("Ada's shell shows the workspace it made", || {
        laptop.showing().is_some()
    });
    let workspace = laptop.showing().expect("the workspace Ada's shell made");
    // The device founded it on its own workspace service off the loop; its invitations are made
    // once the service holds it.
    laptop.wait_holding(&workspace);
    let invitation = shared_invitation(&laptop, "editor", bob.public);
    bob_laptop.install_files();
    bob_laptop.join(&invitation);
    bob_laptop.wait_holding(&workspace);
    bob_laptop.wait_showing(&workspace);
}

/// A workspace this session makes is on its own workspace service before the session writes its
/// first record into it: the layout the shell shows is the workspace's record, and it reads back
/// through the service this device is a member of, not from a copy the session keeps alone.
#[test]
fn a_made_workspace_is_on_the_service_before_its_first_record_write() {
    let ada = Person::new("Ada");
    let mut laptop = ada.device("Ada's laptop");
    laptop.start();
    tilo_test_env::wait_until("Ada's shell shows the workspace it made", || {
        laptop.showing().is_some()
    });
    let workspace = laptop.showing().expect("the workspace Ada's shell made");
    // Held as this device's member: the service took the workspace the session made.
    laptop.wait_holding(&workspace);
    let answer = laptop.ask(&json!({
        "op": "resource_read",
        "workspace": workspace,
        "app": tilo_desktop_model::ownership::DESKTOP_APP,
        "records": [tilo_desktop_model::ownership::DESKTOP_DOCUMENT],
    }));
    assert!(
        answer.get("err").is_none(),
        "{} reads the made workspace's layout: {answer}\n{}",
        laptop.label,
        laptop.diagnostic()
    );
    assert!(
        !answer["answer"]["records"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "the layout record the session wrote is the service's: {answer}\n{}",
        laptop.diagnostic()
    );
}

/// A member whose part is to read is refused the invitation an app asks the shell for: sharing is
/// inviting into the workspace, and only what the person using the app may do reaches another
/// person through it.
#[test]
fn a_member_whose_part_does_not_let_them_invite_is_refused() {
    let (site, _ada, bob, _ada_laptop) = crate::common::people::a_site_with_files_for_one_laptop();
    let mut bob_laptop = bob.device("Bob's laptop");
    bob_laptop.install_files();
    let invitation = site.invite(bob.public, tilo_server_protocol::workspaces::Role::Viewer);
    bob_laptop.join(&invitation);
    bob_laptop.wait_holding(&site.workspace);
    site.wait_for_member(bob.public);

    let answer = crate::common::people::shell_answer(&bob_laptop, &json!({"role": "editor"}));
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

/// The link a device hands out right after its session starts names the server it is reached
/// through: the compact link is read off the signed invitation as it is handed over, so an
/// invitation signed before its endpoint reached a relay names only the network the device stands
/// in, and reaches nobody the link is given to. Nothing else is in the workspace, so making the
/// link is what makes the device reachable at all.
#[test]
fn a_link_made_right_after_start_names_the_relay_the_device_is_reached_through() {
    let ada = Person::new("Ada");
    let mut laptop = ada.device("Ada's laptop");
    laptop.start();
    // Made on this session, so its record names this device as its own host and its person may
    // invite into it.
    let made = laptop.act_wait(&new_workspace(), &json!({}));
    assert_eq!(
        made["update"]["state"],
        json!("succeeded"),
        "{} makes a workspace: {made}\n{}",
        laptop.label,
        laptop.diagnostic()
    );
    // The workspace is founded on this device's own workspace service, off the loop: an invitation
    // into it is the service's to make, once it holds the workspace.
    let workspace = laptop
        .showing()
        .expect("the workspace the laptop made is in front");
    laptop.wait_holding(&workspace);
    let link = shared_invitation(&laptop, "editor", ada.public);
    // The link is the compact one a device opens: it names the workspace's invitation, the key
    // that resolves it and where to dial, so a device handed it fetches the signed invitation
    // from the holder that made it.
    let locator = tilo_workspace_join::native::InvitationLink::parse(&link).unwrap_or_else(|| {
        panic!(
            "{}'s link is a native invitation link: {link:?}\n{}",
            laptop.label,
            laptop.diagnostic()
        )
    });
    assert_eq!(
        locator.reference.workspace.to_string(),
        workspace,
        "the link names the workspace the laptop made: {link:?}"
    );
    let server = test_server::loopback_url();
    assert_eq!(
        locator.relay.as_deref(),
        Some(server.as_str()),
        "the link names the relay this device is reached through ({server}): {link:?}\n{}",
        laptop.diagnostic()
    );
}
