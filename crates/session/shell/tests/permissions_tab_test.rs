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
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::state::WorkspaceAuthority as _;
use std::collections::HashMap;

use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::chrome_state::{Chrome, WsTab};
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    CapOverride, Capability, Expiry, GRANT_ALWAYS, GRANT_SESSION, ResourceRef, Scope, Subject,
    Verb, Workspace, WorkspaceId, WorkspaceTree,
};
use tilo_desktop::workspace_settings::{self, GrantDraft, SubjectKind};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};

const W: f32 = 1600.0;
const H: f32 = 900.0;

fn synth_device() -> DeviceId {
    // SAFETY: `DeviceId` is opaque; integration-test scaffolding
    // already uses this pattern in
    // `tests/permission_prompt_pump.rs` and several other shell
    // integration tests.
    unsafe { std::mem::zeroed() }
}

fn cursor_at(p: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: synth_device(),
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    }
}

fn left_press() -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: synth_device(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    }
}

fn left_release() -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: synth_device(),
        state: ElementState::Released,
        button: MouseButton::Left,
    }
}

fn tree_with_grants(entries: Vec<(&str, Vec<Capability>)>) -> WorkspaceTree {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::from(1);
    let mut ws = Workspace::new(id, "perm-test");
    // Revoking is an administrator's act, and `Workspace::new` leaves a room
    // ownerless — install the identity the chrome acts as, so the click
    // exercises the authority check.
    ws.set_owner(shell::shell::state::local_user_id_for_room(&ws.room));
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
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root_node = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(W),
                    height: Dimension::length(H),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root_node,
            Size {
                width: AvailableSpace::Definite(W),
                height: AvailableSpace::Definite(H),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    host.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    host.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    (host, chrome, scene)
}

fn dispatch_to_host(host: &mut ShellOverlayHost, event: &WindowEvent) {
    use automerge::AutoCommit;
    let mut tree = TaffyTree::<()>::new();
    let node = tree.new_leaf(Style::default()).unwrap();
    let mut handled = false;
    let mut focus_target = None;
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    host.handle_event(
        event,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &winit::event::Modifiers::default(),
            state: &mut doc,
            path: &root,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: node,
            taffy: &tree,
        },
    );
}

/// A cursor move to seed `last_cursor`, then a left press that lands on
/// the rect.
fn click(host: &mut ShellOverlayHost, rect: vello::kurbo::Rect) {
    dispatch_to_host(host, &cursor_at(rect.center()));
    dispatch_to_host(host, &left_press());
    dispatch_to_host(host, &left_release());
}

/// Re-render so the host's control-rect caches match the current state.
fn repaint(host: &mut ShellOverlayHost) {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    host.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
}

/// Memberships go in through `grant_as`, so the fixture itself exercises
/// the authority check.
fn tree_with_members() -> WorkspaceTree {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::from(1);
    let mut ws = Workspace::new(id, "perm-test");
    let owner = shell::shell::state::local_user_id_for_room(&ws.room);
    ws.set_owner(owner.clone());
    let room = ws.id.raw().to_string();
    for (person, verb) in [("ada", Verb::Read), ("bo", Verb::Read)] {
        ws.grant_as(
            &owner,
            CapOverride::membership(Subject::person(person), room.clone(), verb),
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
        .map(|ws| ws.capabilities.clone())
        .unwrap_or_default()
}

// The Permissions tab is always reachable

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

        repaint(&mut host);
        let general = host.ws_settings_tab_pill_rect(WsTab::General);
        click(&mut host, general);
        repaint(&mut host);
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
    // Phase `clicking_grant_writes_a_capability_the_decision_function_honours`.
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
            repaint(&mut host);
        }
        assert_eq!(chrome.borrow().ws_grant_draft.subject_index, target);

        // Drive the capability picker to "Microphone".
        while chrome.borrow().ws_grant_draft.choice().label() != "Microphone" {
            let rect = host.grant_composer_rects().unwrap().capability;
            click(&mut host, rect);
            repaint(&mut host);
        }

        // Pick the "Session" expiry segment (index 1 of the picker).
        let session = host.grant_composer_rects().unwrap().expiry[1];
        click(&mut host, session);
        repaint(&mut host);
        assert_eq!(chrome.borrow().ws_grant_draft.expiry, Expiry::EndOfSession);

        // Precondition read off the model, not the widget.
        {
            let st = chrome.borrow();
            let t = st.tree();
            let ws = t.desktop.active_workspace().unwrap();
            assert!(!ws.allows(
                &ws.actor_for_app("firefox"),
                &ResourceRef::Device("microphone".into()),
                Verb::Read
            ));
        }

        let submit = host.grant_composer_rects().unwrap().submit;
        click(&mut host, submit);

        let st = chrome.borrow();
        let t = st.tree();
        let ws = t.desktop.active_workspace().expect("active workspace");
        assert!(
            ws.allows(
                &ws.actor_for_app("firefox"),
                &ResourceRef::Device("microphone".into()),
                Verb::Read
            ),
            "the grant must start working, not merely start rendering"
        );
        let written = ws
            .capabilities
            .iter()
            .find(|c| matches!(&c.scope, Scope::Device(d) if d == "microphone"))
            .expect("the capability is on the workspace");
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
        repaint(&mut host);
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
        for (user, role_rect, remove_rect) in rows {
            assert!(roster.iter().any(|(u, _)| u == user), "{user} is a member");
            assert!(role_rect.area() > 0.0);
            assert!(remove_rect.area() > 0.0);
        }
    }

    // Phase `role_chip_opens_an_explicit_choice_before_changing_what_the_member_may_do`.
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
                ws.role_of(&"ada".to_string()),
                Some(shell::shell::state::CapRole::Viewer)
            );
            assert!(!ws.allows(
                &ws.actor_for_person(&"ada".to_string()),
                &ResourceRef::Workspace,
                Verb::Write
            ));
        }

        // Opening the chooser is inert; selecting Editor is the deliberate
        // mutation. The role options are Viewer / Commenter / Editor.
        click(&mut host, ada.1);
        repaint(&mut host);
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
        let editor = host.member_sheet_option_rects_for_tests()[2];
        click(&mut host, editor);

        let st = chrome.borrow();
        let t = st.tree();
        let ws = t.desktop.active_workspace().unwrap();
        assert_eq!(
            ws.role_of(&"ada".to_string()),
            Some(shell::shell::state::CapRole::Editor)
        );
        assert!(
            ws.allows(
                &ws.actor_for_person(&"ada".to_string()),
                &ResourceRef::Workspace,
                Verb::Write
            ),
            "promoting to Editor must actually grant write authority"
        );
    }

    // Phase `removing_a_member_takes_their_capabilities_with_them`.
    {
        let mut tree = tree_with_members();
        let owner = shell::shell::state::local_user_id_for_room(
            &tree
                .desktop
                .active_workspace()
                .expect("fixture workspace")
                .room,
        );
        if let Some(ws) = tree.desktop.active_workspace_mut() {
            ws.grant_as(
                &owner,
                CapOverride::grant(
                    Subject::person("ada"),
                    Scope::FsPath("/var/workspace".into()),
                    Verb::Write,
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

        click(&mut host, ada.2);
        assert!(host.member_sheet_is_open_for_tests());
        assert!(
            capabilities(&chrome)
                .iter()
                .any(|cap| cap.subject == Subject::person("ada")),
            "the first click only opens the removal confirmation"
        );
        repaint(&mut host);
        let confirm = host.member_sheet_option_rects_for_tests()[2];
        click(&mut host, confirm);

        let st = chrome.borrow();
        let t = st.tree();
        let ws = t.desktop.active_workspace().unwrap();
        assert_eq!(ws.role_of(&"ada".to_string()), None, "no longer a member");
        assert!(
            !ws.allows(
                &ws.actor_for_person(&"ada".to_string()),
                &ResourceRef::FsPath("/var/workspace/notes".into()),
                Verb::Write
            ),
            "removal must revoke what they held, not just hide the row"
        );
    }

    // Phase `a_non_owner_cannot_change_a_role_or_remove_a_member`.
    {
        let tree = tree_with_members();
        let (mut host, chrome, _scene) = render_at_tab(&tree, WsTab::Members);
        chrome.borrow_mut().test_only_owner_override = Some(false);
        let before = capabilities(&chrome);
        let rows = host.member_control_rects().to_vec();
        assert!(!rows.is_empty());

        for (_, role_rect, remove_rect) in &rows {
            click(&mut host, *role_rect);
            click(&mut host, *remove_rect);
        }

        assert_eq!(
            capabilities(&chrome),
            before,
            "a non-owner driving the member controls must change nothing"
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
                .room,
        );
        let (mut host, chrome, _scene) = render_at_tab(&tree, WsTab::Members);
        let row = host
            .member_control_rects()
            .iter()
            .find(|(u, _, _)| *u == owner)
            .cloned()
            .expect("the owner is in the roster");
        let before = capabilities(&chrome);

        click(&mut host, row.1);
        click(&mut host, row.2);

        assert_eq!(capabilities(&chrome), before);
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
    let owner = shell::shell::state::local_user_id_for_room(&ws.room);
    ws.set_owner(owner.clone());
    let freeform = Scope::Action {
        app: "so.tilo.agent".into(),
        action: "freeform".into(),
    };
    let granted = ws
        .grant_as(
            &owner,
            CapOverride::grant(Subject::app("so.tilo.shortcuts"), freeform, Verb::Write),
        )
        .expect("the owner may grant");
    let room = ws.room.to_string();
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
    repaint(&mut host);
    let confirm = host.member_sheet_option_rects_for_tests()[2];
    click(&mut host, confirm);

    assert!(
        !capabilities(&chrome).iter().any(|c| c.id == granted),
        "revoking in Settings removes the grant"
    );
    let still_allowed = chrome
        .borrow()
        .tree()
        .desktop
        .active_workspace()
        .is_some_and(|ws| {
            ws.allows(
                &shell::shell::state::Actor::app("so.tilo.shortcuts").in_room(room),
                &ResourceRef::Action {
                    app: "so.tilo.agent".into(),
                    action: "freeform".into(),
                },
                Verb::Write,
            )
        });
    assert!(!still_allowed, "the app may no longer use the action");
}
