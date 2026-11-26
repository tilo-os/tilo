//! Deleting a workspace: the person is asked in a question of their own,
//! wherever deleting was asked for, and only their yes deletes it.

#![cfg(target_os = "linux")]

use std::time::Duration;

use tilo_app_test_host::TestApp;
use tilo_session::Session;
use tilo_session_core::desktop::i18n::t;
use tilo_sheets::SheetState as _;
use tilo_sheets::delete_workspace::DeleteWorkspaceEffect;
use tilo_sheets::workspace_settings::WorkspaceSettingsApp;

use crate::common::held::{WORKSPACE, hosted_session, this_computer, workspace_held_here};

fn settle(session: &mut Session) {
    for _ in 0..6 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
}

/// The workspace shown on the shell, by id.
fn front(session: &Session) -> String {
    session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
        .expect("a workspace in front")
}

/// The name the workspace held here is founded under, which its governance keeps.
const SHARED_NAME: &str = "Shared browser workspace";

/// The session's delete question, as it asks it now.
fn asked(session: &Session) -> String {
    session
        .delete_workspace_sheet()
        .expect("the session hosts the delete question")
        .asked()
        .name
}

fn delete_sheet_open(session: &Session) -> bool {
    session
        .delete_workspace_sheet()
        .is_some_and(|state| state.presentation().is_open())
}

/// Whether the shell is asking the person anything on a banner.
fn asking_on_a_banner(session: &Session) -> bool {
    let centre = session.notifications();
    let log = centre.log();
    log.active()
        .iter()
        .any(tilo_session_core::desktop::state::is_host_offer)
}

/// The session keeps the workspace until the person says yes.
#[test]
fn deleting_a_workspace_asks_in_its_own_question_and_keep_deletes_nothing() {
    let _settled = tilo_ui_base::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_identity, key, cert) = this_computer(home.env());
    let state = tempfile::tempdir().expect("a state directory");
    workspace_held_here(state.path(), WORKSPACE, &cert, &key);
    let mut session = hosted_session(state.path(), &cert.user_public_key);
    settle(&mut session);
    let shown = front(&session);

    // An agent asks the shell to delete it without having answered anything: the person's own
    // question stands instead, and nothing is deleted.
    session
        .invoke(
            tilo_session_core::desktop::verbs::CHROME_RESOURCE,
            "delete-workspace",
            &serde_json::Map::new(),
        )
        .expect("the shell is asked to delete the workspace");
    settle(&mut session);
    assert_eq!(front(&session), shown, "the workspace stays");
    // The workspace is named as its governance names it: the one its host founded it under.
    assert_eq!(
        asked(&session),
        SHARED_NAME,
        "the question names the workspace"
    );
    assert!(delete_sheet_open(&session), "the question stands");
    assert!(
        !asking_on_a_banner(&session),
        "deleting is a question of its own, not a banner"
    );
    crate::common::shot(&mut session, "delete-asked");

    // Keep: the question goes and the workspace stays, with everything in it.
    let state = session
        .delete_workspace_sheet()
        .expect("the delete question")
        .clone();
    state.answer(DeleteWorkspaceEffect::Keep);
    settle(&mut session);
    assert_eq!(front(&session), shown, "keeping deletes nothing");
    assert!(!delete_sheet_open(&session), "the question is answered");
}

/// The person's yes is what deletes the workspace, from here and from this device's record.
#[test]
fn the_persons_yes_deletes_the_workspace() {
    let _settled = tilo_ui_base::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_identity, key, cert) = this_computer(home.env());
    let state = tempfile::tempdir().expect("a state directory");
    workspace_held_here(state.path(), WORKSPACE, &cert, &key);
    let mut session = hosted_session(state.path(), &cert.user_public_key);
    settle(&mut session);
    let shown = front(&session);

    session
        .invoke(
            tilo_session_core::desktop::verbs::CHROME_RESOURCE,
            "delete-workspace",
            &serde_json::Map::new(),
        )
        .expect("the shell is asked to delete the workspace");
    settle(&mut session);
    session
        .delete_workspace_sheet()
        .expect("the delete question")
        .answer(DeleteWorkspaceEffect::Delete);
    settle(&mut session);

    assert!(
        session.handle().read(|tree| tree
            .workspaces
            .iter()
            .all(|held| held.id().to_string() != shown)),
        "the workspace is gone from the shell"
    );
    assert!(!delete_sheet_open(&session), "the question is answered");
    let centre = session.notifications();
    let log = centre.log();
    let told = log
        .history()
        .iter()
        .chain(log.active().iter())
        .any(|notice| notice.title.contains(SHARED_NAME));
    assert!(told, "the person is told the workspace is deleted");
}

/// The workspace settings of a workspace the person is alone in offer deleting it, and say why
/// leaving it is not on offer.
#[test]
fn the_settings_of_a_sole_members_workspace_offer_deleting_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_identity, key, cert) = this_computer(home.env());
    let state = tempfile::tempdir().expect("a state directory");
    workspace_held_here(state.path(), WORKSPACE, &cert, &key);
    let mut session = hosted_session(state.path(), &cert.user_public_key);
    settle(&mut session);

    session
        .invoke(
            tilo_session_core::desktop::verbs::CHROME_RESOURCE,
            "workspace-settings",
            &serde_json::Map::new(),
        )
        .expect("the settings open");
    settle(&mut session);
    let general = session
        .workspace_settings()
        .expect("workspace settings")
        .settings()
        .general;
    assert!(
        general.delete.is_some(),
        "a workspace the person is alone in can be deleted"
    );
    assert!(
        general.delete_note.is_none(),
        "and nothing says why it could not be done here"
    );
    assert!(!general.leavable, "leaving says nothing with nobody in it");
    assert_eq!(
        general.leave_note.as_deref(),
        Some(t("workspace-settings-leave-sole-member").as_str()),
        "and it says why"
    );

    // The row asks first, over the settings, and the yes is the shell's.
    let state = session
        .workspace_settings()
        .expect("workspace settings")
        .clone();
    let mut app = TestApp::<WorkspaceSettingsApp>::for_person_with_lent("person", state)
        .expect("the sheet draws");
    app.harness().resize((1280, 900)).expect("a screen");
    app.settle();
    for button in ["ws-general-delete", "ws-settings-decision.primary"] {
        app.harness()
            .perform_named(button, &tilo_ui_base::a11y::Action::Activate)
            .unwrap_or_else(|error| panic!("press {button}: {error}"));
        app.settle();
    }
    let shown = front(&session);
    settle(&mut session);
    assert!(
        session.handle().read(|tree| tree
            .workspaces
            .iter()
            .all(|held| held.id().to_string() != shown)),
        "the confirmed delete is the shell's, and it deleted the workspace"
    );
}

/// Whether `device` shows `workspace` on its shell, and whether it keeps it.
fn still_there(device: &crate::common::people::Device, workspace: &str) -> (bool, bool) {
    let state = device.state();
    let shown = state["state"]["workspace_tree"]["workspaces"]
        .as_array()
        .is_some_and(|shown| {
            shown
                .iter()
                .any(|held| held["id"].as_str() == Some(workspace))
        });
    (shown, device.held().iter().any(|held| held == workspace))
}

/// A device forgetting a deleted workspace, as a wait that gave up names it: what the device still
/// has of it then, and what it logged.
struct Forgetting<'a> {
    device: &'a crate::common::people::Device,
    workspace: &'a str,
}

impl std::fmt::Display for Forgetting<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (shown, kept) = still_there(self.device, self.workspace);
        write!(
            f,
            "{} forgets {} (it still shows it: {shown}, keeps it: {kept}; its shell shows {:?}); its \
             session logged:\n{}",
            self.device.label,
            self.workspace,
            self.device.on_shell(),
            self.device.log()
        )
    }
}

/// Wait until `device` neither shows nor keeps `workspace`, which its owner deleted, and said so.
fn forgotten(device: &crate::common::people::Device, workspace: &str) {
    tilo_test_env::wait_until(Forgetting { device, workspace }, || {
        still_there(device, workspace) == (false, false)
    });
    assert!(
        device
            .log()
            .contains("a workspace its owner deleted leaves this device"),
        "{} says why it forgot {workspace}:\n{}",
        device.label,
        device.log()
    );
}

/// The owner deletes the workspace at its site: a member's device in it takes it off the shell and
/// out of its record and tells the person, and a member's device that was off meanwhile does the
/// same once it is back.
#[test]
fn a_workspace_its_owner_deletes_leaves_every_members_device() {
    let (site, _ada, _bob, ada_laptop, mut bob_laptop) =
        crate::common::people::two_people_at_a_site();
    ada_laptop.wait_holding(&site.workspace);
    bob_laptop.wait_holding(&site.workspace);
    bob_laptop.stop();

    site.delete();
    forgotten(&ada_laptop, &site.workspace);

    bob_laptop.start();
    forgotten(&bob_laptop, &site.workspace);
}
