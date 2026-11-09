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

// A host admits its members' records through an Automerge worker: this binary is that worker, as it
// is the session's own.
tilo_automerge_isolate::test_worker!();

mod common;

use serde_json::json;

/// The invitation the desk hands out admits the person it was made for: their device joins the
/// workspace the desk shows and holds it, and the site records them as a member.
#[test]
fn the_desks_invitation_admits_the_person_it_names() {
    let (site, _ada, bob, ada_laptop) = common::people::a_site_with_files_for_one_laptop();
    let invitation = common::people::shared_invitation(&ada_laptop, "editor", bob.public);
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
    let (site, _ada, bob, _ada_laptop) = common::people::a_site_with_files_for_one_laptop();
    let mut bob_laptop = bob.device("Bob's laptop");
    bob_laptop.install_files();
    let invitation = site.invite(
        bob.public,
        tilo_relay_protocol::workspaces::WorkspaceRole::Viewer,
    );
    bob_laptop.join(&invitation);
    bob_laptop.wait_holding(&site.workspace);
    site.wait_for_member(bob.public);

    let answer = common::people::desk_answer(&bob_laptop, &json!({"role": "editor"}));
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
