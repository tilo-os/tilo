//! Integration tests for the Workspace Settings permission panel.
//!
//! Every test drives the chrome/overlay-host wiring end-to-end: build a
//! workspace with a real owner (members and capabilities via `grant_as`),
//! open the modal at the tab under test, render once so
//! `draw_ws_settings` populates control rects, read those rects back and
//! synthesize a click, then assert against `Workspace::capabilities` and
//! the decision function — never a widget's own cache. Full
//! build/layout/draw cycle, no GPU.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::state::WorkspaceAuthority as _;

use taffy::{Style, TaffyTree};
use vello::Scene;
use vello::kurbo::Affine;

use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::chrome_state::{Chrome, WsTab};
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    Access, CapOverride, Capability, Expiry, GRANT_ALWAYS, GRANT_SESSION, GovernanceRequest,
    ResourceRef, Scope, Subject, Workspace, WorkspaceId, WorkspaceTree,
};
use tilo_desktop::workspace_settings::{self, GrantDraft, SubjectKind};

use ui::presence::PresenceCtx;
use ui::synthetic_input::{cursor_moved, mouse_press, mouse_release};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use winit::event::{Modifiers, WindowEvent};

use crate::support::host::{dispatch, lay_out, paint};

const W: f32 = 1600.0;
const H: f32 = 900.0;

fn tree_with_grants(entries: Vec<(&str, Vec<Capability>)>) -> WorkspaceTree {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::from(1);
    let mut ws = Workspace::new(id, "perm-test");
    // Revoking is an administrator's act, and `Workspace::new` leaves a room
    // ownerless — install the identity the chrome acts as, so the click
    // exercises the authority check.
    ws.set_owner(shell::shell::state::local_user_id_for_room(&ws.room()));
    for (app_id, caps) in entries {
        // Seed through the production granting path — `grants()` is a *derived*
        // view rebuilt from `capabilities` and returned by value, so inserting
        // into it would seed nothing.
        for cap in &caps {
            ws.grant_app(app_id, cap);
        }
    }
    tree.desktop.workspaces.push(ws);
    tree.set_active(id);
    tree
}

/// Returns the host too, so the caller can read back hit rects and
/// dispatch synthesized events.
fn render_at_tab(tree: &WorkspaceTree, tab: WsTab) -> (ShellOverlayHost, Chrome, Scene) {
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(tree);
    {
        let mut st = chrome.borrow_mut();
        st.overlays.ws_settings = true;
        st.ws_settings_tab = tab;
    }
    lay_out(&mut host, W, H);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    host.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &presence,
    );
    (host, chrome, scene)
}

fn dispatch_to_host(host: &mut ShellOverlayHost, event: &WindowEvent) {
    let mut tree = TaffyTree::<()>::new();
    let node = tree.new_leaf(Style::default()).unwrap();
    dispatch(host, &tree, node, Modifiers::default(), event);
}

/// A cursor move to seed `last_cursor`, then a left press that lands on
/// the rect.
fn click(host: &mut ShellOverlayHost, rect: vello::kurbo::Rect) {
    let at = rect.center();
    dispatch_to_host(host, &cursor_moved(at.x, at.y));
    dispatch_to_host(host, &mouse_press());
    dispatch_to_host(host, &mouse_release());
}

/// Memberships go in through `grant_as`, so the fixture itself exercises
/// the authority check.
fn tree_with_members() -> WorkspaceTree {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::from(1);
    let mut ws = Workspace::new(id, "perm-test");
    let owner = shell::shell::state::local_user_id_for_room(&ws.room());
    ws.set_owner(owner.clone());
    for person in ["ada", "bo"] {
        ws.govern_as_at(
            &owner,
            tilo_action_policy::Change::AddMember {
                person: person.into(),
                role: tilo_action_policy::Role::Viewer,
            },
            tilo_clock::unix_now(),
        )
        .expect("the owner may write a membership");
    }
    tree.desktop.workspaces.push(ws);
    tree.set_active(id);
    tree
}

fn capabilities(chrome: &Chrome) -> Vec<CapOverride> {
    let st = chrome.borrow();
    let tree = st.tree();
    tree.desktop
        .active_workspace()
        .map(|ws| ws.sandbox.clone())
        .unwrap_or_default()
}

/// The active workspace's whole authority: its policy and its sandbox.
fn authority(chrome: &Chrome) -> (Option<tilo_action_policy::RoomPolicy>, Vec<CapOverride>) {
    let st = chrome.borrow();
    let tree = st.tree();
    (
        tree.desktop.active_workspace().map(|ws| ws.policy.clone()),
        tree.desktop
            .active_workspace()
            .map(|ws| ws.sandbox.clone())
            .unwrap_or_default(),
    )
}

/// What the chrome asked the active room's governor for. Nothing a member
/// control picks changes the room here: its governor's commit does, which
/// the one-host scenario of the member controls drives.
fn asked_of_governor(chrome: &Chrome) -> Vec<GovernanceRequest> {
    let st = chrome.borrow();
    let tree = st.tree();
    tree.room_outbox
        .iter()
        .filter_map(|entry| match &entry.change {
            shell::shell::state::RoomChange::Governance(request) => Some(request.clone()),
            shell::shell::state::RoomChange::Receipt(_) => None,
        })
        .collect()
}

/// Whether `person` may change the active room's contents outright.
fn may_change(ws: &Workspace, person: &str) -> bool {
    ws.person_allowed(
        person,
        &tilo_action_policy::TreePath::root(),
        tilo_action_policy::ActionKind::Undoable,
    )
}

// The Permissions tab is always reachable

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "st's only use is producing t via tree(); the read-guard's lifetime \
is tied to st's borrow, so it can't be dropped before t's last use"
)]
fn the_permissions_tab_is_reachable_renders_its_rows_and_keeps_the_draft() {
    // Phase `the_permissions_pill_is_painted_on_a_workspace_with_no_grants`.
    {
        // The pill must be on screen for an empty workspace, so a host can
        // grant proactively rather than only in response to a portal prompt.
        let tree = tree_with_grants(vec![]);
        let (host, chrome, _scene) = render_at_tab(&tree, WsTab::General);
        {
            let st = chrome.borrow();
            let t = st.tree();
            assert!(
                !t.desktop.active_workspace().unwrap().has_app_grants(),
                "precondition: no grants"
            );
        }
        assert!(
            host.ws_settings_tab_pill_rect(WsTab::Permissions).area() > 0.0,
            "the Permissions pill must be painted even with zero grants"
        );
        for tab in [WsTab::General, WsTab::Members, WsTab::Permissions] {
            assert!(
                host.ws_settings_tab_pill_rect(tab).area() > 0.0,
                "{tab:?} must have a clickable pill"
            );
        }
    }

    // Phase `clicking_the_permissions_pill_switches_to_it`.
    {
        let tree = tree_with_grants(vec![]);
        let (mut host, chrome, _scene) = render_at_tab(&tree, WsTab::General);
        let pill = host.ws_settings_tab_pill_rect(WsTab::Permissions);
        click(&mut host, pill);
        assert_eq!(chrome.borrow().ws_settings_tab, WsTab::Permissions);
    }

    // Phase `permissions_tab_renders_zero_revoke_rects_when_no_grants`.
    {
        let tree = tree_with_grants(vec![]);
        let (host, _chrome, _scene) = render_at_tab(&tree, WsTab::Permissions);
        assert!(
            host.permissions_revoke_rects().is_empty(),
            "no grants ⇒ no revoke-button rects, got {}",
            host.permissions_revoke_rects().len()
        );
        assert!(
            host.grant_composer_rects().is_some(),
            "the composer must be on screen even with nothing granted"
        );
    }

    // Phase `permissions_tab_renders_one_row_per_capability`.
    {
        let tree = tree_with_grants(vec![(
            "firefox",
            vec![
                Capability::Camera {
                    scope: GRANT_ALWAYS,
                },
                Capability::Network {
                    hosts: None,
                    scope: GRANT_SESSION,
                },
            ],
        )]);
        let (host, chrome, _scene) = render_at_tab(&tree, WsTab::Permissions);
        let rects = host.permissions_revoke_rects();
        assert_eq!(
            rects.len(),
            2,
            "expected 2 revoke rects for 2 capabilities, got {}",
            rects.len()
        );
        // Each rect is keyed by a capability that really exists on the
        // workspace, so a click cannot name something that isn't there.
        let live: Vec<String> = capabilities(&chrome).into_iter().map(|c| c.id).collect();
        for (cap_id, r) in rects {
            assert!(r.area() > 0.0, "revoke rect must have non-zero area");
            assert!(
                live.contains(cap_id),
                "revoke rect keyed by {cap_id}, which is not a live capability"
            );
        }
    }

    // Phase `the_draft_survives_a_tab_round_trip`.
    {
        // The draft lives on the chrome, not the overlay host, so leaving
        // and returning to the tab must not silently reset a half-composed
        // grant.
        let tree = tree_with_members();
        let (mut host, chrome, _scene) = render_at_tab(&tree, WsTab::Permissions);
        let rect = host.grant_composer_rects().unwrap().capability;
        click(&mut host, rect);
        let composed = chrome.borrow().ws_grant_draft;
        assert_ne!(composed, GrantDraft::default());

        paint(&mut host);
        let general = host.ws_settings_tab_pill_rect(WsTab::General);
        click(&mut host, general);
        paint(&mut host);
        assert!(
            host.grant_composer_rects().is_none(),
            "the composer's rects are cleared when its tab is not on screen"
        );
        let permissions = host.ws_settings_tab_pill_rect(WsTab::Permissions);
        click(&mut host, permissions);

        assert_eq!(chrome.borrow().ws_grant_draft, composed);
    }
}

// Rendering the capability list

// Granting

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "st's only use is producing t via tree(); the read-guard's lifetime \
is tied to st's borrow, so it can't be dropped before t's last use"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn grant_and_revoke_clicks_are_owner_only_and_deliberate() {
    // Phase `clicking_grant_asks_the_governor_for_exactly_the_chosen_capability`.
    {
        let tree = tree_with_grants(vec![(
            "firefox",
            vec![Capability::Camera {
                scope: GRANT_ALWAYS,
            }],
        )]);
        let (mut host, chrome, _scene) = render_at_tab(&tree, WsTab::Permissions);

        // Drive the subject picker to `firefox` by clicking it, exactly as a
        // user would, rather than by writing the draft directly.
        let subjects = {
            let st = chrome.borrow();
            let t = st.tree();
            workspace_settings::grant_subjects(t.desktop.active_workspace().unwrap())
        };
        let target = subjects
            .iter()
            .position(|s| s.kind == SubjectKind::App && s.id == "firefox")
            .expect("firefox is a candidate subject");
        for _ in 0..target {
            let rect = host.grant_composer_rects().unwrap().subject;
            click(&mut host, rect);
            paint(&mut host);
        }
        assert_eq!(chrome.borrow().ws_grant_draft.subject_index, target);

        while chrome.borrow().ws_grant_draft.choice().label() != "Microphone" {
            let rect = host.grant_composer_rects().unwrap().capability;
            click(&mut host, rect);
            paint(&mut host);
        }

        // Pick the "Session" expiry segment (index 1 of the picker).
        let session = host.grant_composer_rects().unwrap().expiry[1];
        click(&mut host, session);
        paint(&mut host);
        assert_eq!(chrome.borrow().ws_grant_draft.expiry, Expiry::EndOfSession);

        // Precondition read off the model, not the widget.
        {
            let st = chrome.borrow();
            let t = st.tree();
            let ws = t.desktop.active_workspace().unwrap();
            assert!(!ws.allows(
                &ws.actor_for_app("firefox"),
                &ResourceRef::Device("microphone".into()),
                Access::Read
            ));
        }

        let submit = host.grant_composer_rects().unwrap().submit;
        click(&mut host, submit);

        let asked = asked_of_governor(&chrome);
        let [GovernanceRequest::GrantSandbox(written)] = asked.as_slice() else {
            panic!("submitting asks the room's governor for one capability: {asked:?}");
        };
        assert_eq!(written.subject, Subject::app("firefox"));
        assert!(
            matches!(&written.scope, Scope::Device(d) if d == "microphone"),
            "the chosen capability is asked for"
        );
        assert_eq!(
            written.expiry,
            Expiry::EndOfSession,
            "chosen expiry is kept"
        );
        assert!(
            written.granted_by.is_some(),
            "grant_as records the grantor; a grant with no recorded author \
             did not go through the authority-checked path"
        );
    }

    // Phase `a_non_owner_clicking_grant_changes_nothing`.
    {
        let tree = tree_with_grants(vec![(
            "firefox",
            vec![Capability::Camera {
                scope: GRANT_ALWAYS,
            }],
        )]);
        let (mut host, chrome, _scene) = render_at_tab(&tree, WsTab::Permissions);
        let before = capabilities(&chrome);
        chrome.borrow_mut().test_only_owner_override = Some(false);

        let composer = *host.grant_composer_rects().expect("composer on screen");
        click(&mut host, composer.subject);
        click(&mut host, composer.capability);
        click(&mut host, composer.expiry[0]);
        click(&mut host, composer.submit);

        assert_eq!(
            capabilities(&chrome),
            before,
            "a non-owner driving the composer must not change the capability list"
        );
        // And the modal is still open: the click was consumed, not fallen
        // through to the dismiss path.
        assert_eq!(chrome.borrow().ws_settings_tab, WsTab::Permissions);
    }

    // Phase `revoke_requires_a_deliberate_confirmation_for_the_capability_it_was_painted_for`.
    {
        let tree = tree_with_grants(vec![(
            "firefox",
            vec![
                Capability::Camera {
                    scope: GRANT_ALWAYS,
                },
                Capability::Network {
                    hosts: None,
                    scope: GRANT_SESSION,
                },
            ],
        )]);
        let (mut host, chrome, _scene) = render_at_tab(&tree, WsTab::Permissions);
        let (target_id, target_rect) = host.permissions_revoke_rects()[0].clone();
        let before = capabilities(&chrome);
        assert!(before.iter().any(|c| c.id == target_id));

        click(&mut host, target_rect);

        assert!(host.member_sheet_is_open_for_tests());
        assert_eq!(
            capabilities(&chrome),
            before,
            "opening a revoke confirmation must not mutate a capability"
        );
        paint(&mut host);
        let confirm = host.member_sheet_option_rects_for_tests()[2];
        click(&mut host, confirm);

        let after = capabilities(&chrome);
        assert!(
            !after.iter().any(|c| c.id == target_id),
            "the clicked row's capability must be gone"
        );
        assert_eq!(
            after.len(),
            before.len() - 1,
            "exactly one capability is removed"
        );
    }

    // Phase `revoke_button_is_inert_for_a_non_owner`.
    {
        let tree = tree_with_grants(vec![(
            "firefox",
            vec![Capability::Camera {
                scope: GRANT_ALWAYS,
            }],
        )]);
        let (mut host, chrome, _scene) = render_at_tab(&tree, WsTab::Permissions);
        chrome.borrow_mut().test_only_owner_override = Some(false);
        let before = capabilities(&chrome);
        let rects = host.permissions_revoke_rects().to_vec();
        assert_eq!(rects.len(), 1);

        click(&mut host, rects[0].1);

        assert_eq!(
            capabilities(&chrome),
            before,
            "a non-owner click on Revoke must be a no-op"
        );
    }
}

// Revoking

// Members

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "st's only use is producing t via tree(); the read-guard's lifetime \
is tied to st's borrow, so it can't be dropped before t's last use"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn members_tab_role_and_removal_controls_are_explicit_and_owner_only() {
    // Phase `the_members_tab_renders_the_roster_with_controls`.
    {
        let tree = tree_with_members();
        let (host, chrome, _scene) = render_at_tab(&tree, WsTab::Members);
        let rows = host.member_control_rects();
        let roster = {
            let st = chrome.borrow();
            let t = st.tree();
            t.desktop.active_workspace().unwrap().roster()
        };
        assert_eq!(
            rows.len(),
            roster.len(),
            "one row per member of the real roster"
        );
        let owner = shell::shell::state::local_user_id_for_room(
            &tree
                .desktop
                .active_workspace()
                .expect("fixture workspace")
                .room(),
        );
        for (user, role_rect, remove_rect) in rows {
            assert!(roster.iter().any(|(u, _)| u == user), "{user} is a member");
            if *user == owner {
                assert_eq!(role_rect.area(), 0.0, "the owner's role is not a choice");
                assert_eq!(remove_rect.area(), 0.0, "no one removes themselves here");
            } else {
                assert!(role_rect.area() > 0.0, "{user}'s role is a choice");
                assert!(remove_rect.area() > 0.0, "{user} can be removed");
            }
        }
    }

    // Phase `role_chip_opens_an_explicit_choice_before_asking_for_the_role`.
    {
        let tree = tree_with_members();
        let (mut host, chrome, _scene) = render_at_tab(&tree, WsTab::Members);
        let ada = host
            .member_control_rects()
            .iter()
            .find(|(u, _, _)| u == "ada")
            .cloned()
            .expect("ada is in the roster");

        {
            let st = chrome.borrow();
            let t = st.tree();
            let ws = t.desktop.active_workspace().unwrap();
            assert_eq!(
                ws.role_of("ada"),
                Some(shell::shell::state::CapRole::Viewer)
            );
            assert!(!may_change(ws, "ada"));
        }

        // Opening the chooser is inert; selecting Editor is the deliberate
        // mutation. The role options are Viewer / Commenter / Editor.
        click(&mut host, ada.1);
        paint(&mut host);
        assert!(host.member_sheet_is_open_for_tests());
        assert_eq!(
            host.member_sheet_option_rects_for_tests().len(),
            3,
            "the chooser presents all three positive roles at once"
        );
        let accessibility = host
            .accessibility()
            .expect("shell has an accessibility tree");
        assert!(
            accessibility.children.iter().any(|node| {
                node.role == ui::a11y::Role::Dialog
                    && node.name.as_deref() == Some("Workspace permission decision")
                    && node.state.modal
            }),
            "assistive technology must learn that a permission decision now owns focus"
        );
        assert!(
            asked_of_governor(&chrome).is_empty(),
            "opening the chooser asks for nothing"
        );
        let editor = host.member_sheet_option_rects_for_tests()[2];
        click(&mut host, editor);

        assert_eq!(
            asked_of_governor(&chrome),
            vec![GovernanceRequest::Policy(
                tilo_action_policy::Change::SetRole {
                    person: "ada".into(),
                    role: tilo_action_policy::Role::Editor,
                }
            )],
            "choosing Editor asks the room's governor for exactly that role"
        );
    }

    // Phase `removing_a_member_asks_for_it_and_narrows_what_they_reached_here`.
    {
        let mut tree = tree_with_members();
        let owner = shell::shell::state::local_user_id_for_room(
            &tree
                .desktop
                .active_workspace()
                .expect("fixture workspace")
                .room(),
        );
        if let Some(ws) = tree.desktop.active_workspace_mut() {
            ws.grant_as(
                &owner,
                CapOverride::grant(
                    Subject::person("ada"),
                    Scope::FsPath("/var/workspace".into()),
                    Access::Write,
                ),
            )
            .expect("the owner may grant");
        }
        let (mut host, chrome, _scene) = render_at_tab(&tree, WsTab::Members);
        let ada = host
            .member_control_rects()
            .iter()
            .find(|(u, _, _)| u == "ada")
            .cloned()
            .expect("ada is in the roster");

        // What confirming asks of the room's governor: the member removed,
        // with each capability they hold now.
        let mut expected = vec![GovernanceRequest::Policy(
            tilo_action_policy::Change::RemoveMember {
                person: "ada".into(),
            },
        )];
        expected.extend(
            capabilities(&chrome)
                .into_iter()
                .filter(|cap| cap.subject == Subject::person("ada"))
                .map(|cap| GovernanceRequest::RevokeSandbox(cap.id)),
        );
        click(&mut host, ada.2);
        assert!(host.member_sheet_is_open_for_tests());
        assert!(
            capabilities(&chrome)
                .iter()
                .any(|cap| cap.subject == Subject::person("ada"))
                && asked_of_governor(&chrome).is_empty(),
            "the first click only opens the removal confirmation"
        );
        paint(&mut host);
        let confirm = host.member_sheet_option_rects_for_tests()[2];
        click(&mut host, confirm);

        assert_eq!(
            asked_of_governor(&chrome),
            expected,
            "confirming asks the room's governor to remove the member and what they held"
        );
        let st = chrome.borrow();
        let t = st.tree();
        let ws = t.desktop.active_workspace().unwrap();
        assert!(
            !ws.allows(
                &ws.actor_for_person(&"ada".to_string()),
                &ResourceRef::FsPath("/var/workspace/notes".into()),
                Access::Write
            ),
            "removal must revoke what they held, not just hide the row"
        );
    }

    // Phase `a_non_owner_cannot_change_a_role_or_remove_a_member`.
    {
        let tree = tree_with_members();
        let (mut host, chrome, _scene) = render_at_tab(&tree, WsTab::Members);
        chrome.borrow_mut().test_only_owner_override = Some(false);
        let before = authority(&chrome);
        let rows = host.member_control_rects().to_vec();
        assert!(!rows.is_empty());

        for (_, role_rect, remove_rect) in &rows {
            for control in [role_rect, remove_rect] {
                if control.area() > 0.0 {
                    click(&mut host, *control);
                }
            }
        }

        assert_eq!(
            authority(&chrome),
            before,
            "a non-owner driving the member controls must change nothing"
        );
        assert!(
            asked_of_governor(&chrome).is_empty(),
            "and asks the governor for nothing"
        );
    }

    // Phase `a_delegated_admin_can_change_roles_through_the_same_check`.
    {
        // Administration delegated as a grant, not ownership: the click
        // must pass the same check `member.role.set` does.
        let mut tree = WorkspaceTree::default();
        let id = WorkspaceId::from(1);
        let mut ws = Workspace::new(id, "perm-test");
        let local = shell::shell::state::local_user_id_for_room(&ws.room());
        let owner = "someone-else".to_string();
        ws.set_owner(owner.clone());
        let now = tilo_clock::unix_now();
        ws.govern_as_at(
            &owner,
            tilo_action_policy::Change::AddMember {
                person: local.clone(),
                role: tilo_action_policy::Role::Editor,
            },
            now,
        )
        .expect("the owner admits the delegate");
        let set_role = tilo_action_policy::roles::governance_action(
            &ws.room().to_string(),
            tilo_action_policy::roles::governance::SET_ROLE,
        );
        ws.govern_as_at(
            &owner,
            tilo_action_policy::Change::Grant(tilo_action_policy::Grant::new(
                "delegate-set-role",
                tilo_action_policy::Subject::person(local),
                set_role,
                tilo_action_policy::KindSet::ALL,
                tilo_action_policy::Answer::Allow,
                tilo_action_policy::Subject::person(owner.clone()),
            )),
            now,
        )
        .expect("the owner may delegate administration");
        ws.govern_as_at(
            &owner,
            tilo_action_policy::Change::AddMember {
                person: ("ada").to_string(),
                role: tilo_action_policy::Role::Viewer,
            },
            tilo_clock::unix_now(),
        )
        .expect("the owner may write a membership");
        tree.desktop.workspaces.push(ws);
        tree.set_active(id);

        let (mut host, chrome, _scene) = render_at_tab(&tree, WsTab::Members);
        let ada = host
            .member_control_rects()
            .iter()
            .find(|(u, _, _)| u == "ada")
            .cloned()
            .expect("ada is in the roster");
        click(&mut host, ada.1);
        paint(&mut host);
        assert!(host.member_sheet_is_open_for_tests());
        let editor = host.member_sheet_option_rects_for_tests()[2];
        click(&mut host, editor);

        assert_eq!(
            asked_of_governor(&chrome),
            vec![GovernanceRequest::Policy(
                tilo_action_policy::Change::SetRole {
                    person: "ada".into(),
                    role: tilo_action_policy::Role::Editor,
                }
            )],
            "a delegated administrator's choice is asked of the governor"
        );
    }

    // Phase `the_owner_row_offers_no_role_change_and_no_removal`.
    {
        // The recorded owner is the workspace's root of trust and `role_of`
        // short-circuits to Owner for them, so a control that appeared to
        // change it would be a control that lies.
        let tree = tree_with_members();
        let owner = shell::shell::state::local_user_id_for_room(
            &tree
                .desktop
                .active_workspace()
                .expect("fixture workspace")
                .room(),
        );
        let (mut host, chrome, _scene) = render_at_tab(&tree, WsTab::Members);
        let row = host
            .member_control_rects()
            .iter()
            .find(|(u, _, _)| *u == owner)
            .cloned()
            .expect("the owner is in the roster");
        let before = authority(&chrome);

        // The row offers no control to press at all.
        assert_eq!(row.1.area(), 0.0, "no role control on the owner's row");
        assert_eq!(row.2.area(), 0.0, "no Remove on the owner's row");
        click(&mut host, row.1);
        click(&mut host, row.2);

        assert_eq!(authority(&chrome), before);
        let st = chrome.borrow();
        let t = st.tree();
        assert_eq!(
            t.desktop.active_workspace().unwrap().role_of(&owner),
            Some(shell::shell::state::CapRole::Owner)
        );
    }
}

// Grant draft plumbing

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use shell::shell::state::WorkspaceExt as _;
#[test]
fn an_app_allowed_another_apps_action_is_listed_under_app_actions_and_revocable() {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::from(1);
    let mut ws = Workspace::new(id, "perm-test");
    let owner = shell::shell::state::local_user_id_for_room(&ws.room());
    ws.set_owner(owner.clone());
    let granted = workspace_settings::apply_governance(
        &mut ws,
        &owner,
        workspace_settings::GovernanceOperation::GrantAppAction {
            app: "so.tilo.shortcuts".into(),
            target_app: "so.tilo.agent".into(),
            resource: "agent.transform:freeform".into(),
            action: "freeform".into(),
            lifetime: "always".into(),
        },
        tilo_clock::unix_now(),
    )
    .expect("the owner may grant")
    .edit
    .granted[0]
        .as_str()
        .to_owned();
    tree.desktop.workspaces.push(ws);
    tree.set_active(id);

    let pane = vello::kurbo::Rect::new(0.0, 0.0, 660.0, 480.0);
    let listed = workspace_settings::permissions_pane_layout(
        &tree.desktop.workspaces[0],
        pane,
        &shell::shell::launcher::ShellApps,
        "viewer-user",
    );
    let row = listed
        .rows
        .iter()
        .find(|row| row.cap_id == granted)
        .expect("the grant is listed");
    assert_eq!(row.subject.kind, SubjectKind::AppAction);
    assert_eq!(row.subject.kind.section_label(), "App actions");
    assert!(row.allows.contains("freeform"), "{}", row.allows);

    let (mut host, chrome, _scene) = render_at_tab(&tree, WsTab::Permissions);
    let (_, revoke) = host
        .permissions_revoke_rects()
        .iter()
        .find(|(cap, _)| *cap == granted)
        .cloned()
        .expect("the grant has a revoke button");
    click(&mut host, revoke);
    paint(&mut host);
    let confirm = host.member_sheet_option_rects_for_tests()[2];
    click(&mut host, confirm);

    assert_eq!(
        asked_of_governor(&chrome),
        vec![GovernanceRequest::Policy(
            tilo_action_policy::Change::Revoke(tilo_action_policy::GrantId::new(granted))
        )],
        "revoking in Settings asks the room's governor to take the grant back"
    );
}
