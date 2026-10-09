//! Integration tests for the permission-prompt pump (chrome <-> overlay-host
//! wiring, no render loop): queue a pending grant, tick to latch a
//! `PermissionPrompt`, click through the widget's own `handle_event`, tick
//! again to drain. Asserts queue state and, critically, that portal choices
//! never enter the collaborative workspace grant map. The portal backend owns
//! private Session/Always persistence after it receives the UI decision.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use shell::shell::state::WorkspaceAuthority as _;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use shell::shell::chrome_state::{ChromeState, OverlayFlags, PendingPrompt};
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{Capability, GRANT_ONCE, Workspace, WorkspaceId, WorkspaceTree};
use ui::widget::PromptChoice;

use taffy::{Style, TaffyTree};
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

fn cursor_at(p: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    }
}

use ui::synthetic_input::mouse_press;
use ui::testing::dispatch;

/// One workspace plus the requested app's pending grant. The workspace is
/// handed to the local user because an ownerless room has no administrators,
/// where "Always" is correctly refused.
fn chrome_with_pending_grant(
    app_id: &str,
    capability: Capability,
) -> (std::rc::Rc<std::cell::RefCell<ChromeState>>, WorkspaceId) {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::from(1);
    tree.desktop.workspaces.push(Workspace::new(id, "demo"));
    tree.set_active(id);
    let chrome = ChromeState::new(tree);
    {
        let mut st = chrome.borrow_mut();
        let room = st
            .tree()
            .desktop
            .active_workspace()
            .expect("fixture workspace")
            .room;
        let owner = st.local_user_for_room(&room);
        st.tree_mut().adopt_ownerless_workspaces(&owner);
        st.pending_grants
            .push_back(PendingPrompt::new(app_id.to_string(), capability));
    }
    (chrome, id)
}

/// Stub content widget — the pump needs no real strip tree.
struct StubContent {
    rect: Rect,
}
impl ui::widget::Widget for StubContent {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> taffy::NodeId {
        let n = taffy.new_leaf(Style::default()).unwrap();
        node_map.insert(n, std::ptr::from_mut::<dyn ui::widget::Widget>(self));
        n
    }
    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: taffy::NodeId, _origin: Point) {}
    fn draw(
        &mut self,
        _scene: &mut vello::Scene,
        _transform: vello::kurbo::Affine,
        _text_renderer: &mut ui::text_render::TextRenderer,
        _theme: &ui::theme::Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
    }
    fn name(&self) -> &'static str {
        "StubContent"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn host_with_chrome(chrome: std::rc::Rc<std::cell::RefCell<ChromeState>>) -> ShellOverlayHost {
    let stub = Box::new(StubContent { rect: Rect::ZERO });
    ShellOverlayHost::new(stub).with_chrome(chrome)
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the pending_grants assert and tree() both need st held; there is nothing \
to narrow between them"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn prompt_pump_drains_one_grant_per_choice_and_never_latches() {
    // Phase `pump_drains_pending_grant_after_choice`.
    {
        let (chrome, _ws_id) =
            chrome_with_pending_grant("org.example.app", Capability::Camera { scope: GRANT_ONCE });
        let mut host = host_with_chrome(chrome.clone());

        // First tick: latch the prompt widget.
        host.tick_permission_prompt_for_tests();
        assert!(
            host.permission_prompt_mut_for_tests().is_some(),
            "first tick must latch a prompt for the front of the queue",
        );

        // Synthesize a click on the Allow button (Once → don't persist).
        {
            let prompt = host
                .permission_prompt_mut_for_tests()
                .expect("prompt latched");
            // Known geometry so the button rects are deterministic.
            prompt.set_rect(Rect::new(0.0, 0.0, 600.0, 80.0));
            let center = prompt.allow_rect().center();
            dispatch(prompt, &cursor_at(center));
            dispatch(prompt, &mouse_press());
            assert_eq!(
                prompt.pending_choice,
                Some(PromptChoice::Allow),
                "widget must observe the Allow click as a pending choice",
            );
        }

        // Second tick: drain the choice + pop the queue.
        host.tick_permission_prompt_for_tests();

        let st = chrome.borrow();
        assert!(
            st.pending_grants.is_empty(),
            "queue must be empty after the user clicks Allow",
        );
        let tree = st.tree();
        let ws = tree.desktop.active_workspace().expect("active workspace");
        assert!(
            !ws.app_grants_snapshot().contains_key("org.example.app"),
            "Once doesn't persist on the workspace's grants map — \
             got: {:?}",
            ws.app_grants_snapshot().get("org.example.app"),
        );
    }

    // Phase `pump_does_not_latch_when_queue_is_empty`.
    {
        let chrome = ChromeState::new(WorkspaceTree::default());
        {
            let st = chrome.borrow_mut();
            let mut tree = st.tree_mut();
            tree.desktop
                .workspaces
                .push(Workspace::new(WorkspaceId::from(1), "demo"));
            let id = WorkspaceId::from(1);
            tree.set_active(id);
        }
        let mut host = host_with_chrome(chrome);
        host.tick_permission_prompt_for_tests();
        assert!(
            host.permission_prompt_mut_for_tests().is_none(),
            "no widget should latch when there's nothing to surface",
        );
    }

    // Phase `pump_advances_to_next_grant_after_first_drain`.
    {
        // Allow then Deny across two apps: both visible in sequence, nothing left.
        let mut tree = WorkspaceTree::default();
        let id = WorkspaceId::from(1);
        tree.desktop.workspaces.push(Workspace::new(id, "demo"));
        tree.set_active(id);
        let chrome = ChromeState::new(tree);
        {
            let mut st = chrome.borrow_mut();
            st.pending_grants.push_back(PendingPrompt::new(
                "org.example.app".into(),
                Capability::Camera { scope: GRANT_ONCE },
            ));
            st.pending_grants.push_back(PendingPrompt::new(
                "org.other.app".into(),
                Capability::Microphone { scope: GRANT_ONCE },
            ));
        }
        let mut host = host_with_chrome(chrome.clone());

        host.tick_permission_prompt_for_tests();
        {
            let prompt = host
                .permission_prompt_mut_for_tests()
                .expect("prompt latched");
            assert_eq!(prompt.app_id, "org.example.app");
            prompt.set_rect(Rect::new(0.0, 0.0, 600.0, 80.0));
            let center = prompt.allow_rect().center();
            dispatch(prompt, &cursor_at(center));
            dispatch(prompt, &mouse_press());
        }
        host.tick_permission_prompt_for_tests();

        {
            let prompt = host
                .permission_prompt_mut_for_tests()
                .expect("second prompt latched after first drain");
            assert_eq!(
                prompt.app_id, "org.other.app",
                "advance to the next queued grant after the first is drained",
            );
            prompt.set_rect(Rect::new(0.0, 0.0, 600.0, 80.0));
            let center = prompt.deny_rect().center();
            dispatch(prompt, &cursor_at(center));
            dispatch(prompt, &mouse_press());
        }
        host.tick_permission_prompt_for_tests();

        let st = chrome.borrow();
        assert!(st.pending_grants.is_empty());
        assert!(
            host_has_no_prompt(&host),
            "no widget remains after both grants are drained",
        );
    }
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the pending_grants assert and tree() both need st held; there is nothing \
to narrow between them"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn prompt_pump_persists_only_what_each_choice_allows() {
    // Phase `pump_keeps_session_choice_out_of_the_shared_workspace`.
    {
        let (chrome, _ws_id) =
            chrome_with_pending_grant("org.example.app", Capability::Camera { scope: GRANT_ONCE });
        let mut host = host_with_chrome(chrome.clone());

        host.tick_permission_prompt_for_tests();
        {
            let prompt = host
                .permission_prompt_mut_for_tests()
                .expect("prompt latched");
            prompt.set_rect(Rect::new(0.0, 0.0, 600.0, 80.0));
            let center = prompt.session_rect().center();
            dispatch(prompt, &cursor_at(center));
            dispatch(prompt, &mouse_press());
            assert_eq!(prompt.pending_choice, Some(PromptChoice::Session));
        }

        host.tick_permission_prompt_for_tests();

        let st = chrome.borrow();
        assert!(st.pending_grants.is_empty(), "queue must drain");
        let tree = st.tree();
        let ws = tree.desktop.active_workspace().expect("active workspace");
        assert!(
            !ws.app_grants_snapshot().contains_key("org.example.app"),
            "a local portal consent must not be replicated to workspace peers",
        );
    }

    // Phase `pump_keeps_always_choice_out_of_workspace_persistence`.
    {
        let (chrome, _ws_id) =
            chrome_with_pending_grant("org.gnome.gedit", Capability::Camera { scope: GRANT_ONCE });
        let state_file = scratch_dir().join("state.json");
        chrome.borrow().tree_mut().persist_path = Some(state_file.clone());

        let mut host = host_with_chrome(chrome.clone());

        host.tick_permission_prompt_for_tests();
        {
            let prompt = host
                .permission_prompt_mut_for_tests()
                .expect("prompt latched");
            // Only a room administrator may grant permanently; the local user owns it.
            assert!(prompt.is_owner);
            prompt.set_rect(Rect::new(0.0, 0.0, 600.0, 80.0));
            let center = prompt.always_rect().center();
            dispatch(prompt, &cursor_at(center));
            dispatch(prompt, &mouse_press());
            assert_eq!(prompt.pending_choice, Some(PromptChoice::Always));
        }
        host.tick_permission_prompt_for_tests();

        let has_shared_grant = {
            let st = chrome.borrow();
            let tree = st.tree();
            let ws = tree.desktop.active_workspace().expect("active workspace");
            let has = ws.app_grants_snapshot().contains_key("org.gnome.gedit");
            drop(tree);
            has
        };
        assert!(
            !has_shared_grant,
            "durable portal consent is private device state, not room state",
        );
        assert!(
            !state_file.exists(),
            "the UI pump must not persist the collaborative workspace merely because a portal prompt was answered",
        );
    }

    // Phase `pump_deny_drops_pending_grant_without_persisting`.
    {
        let (chrome, _ws_id) = chrome_with_pending_grant(
            "org.example.app",
            Capability::Microphone { scope: GRANT_ONCE },
        );
        let mut host = host_with_chrome(chrome.clone());

        host.tick_permission_prompt_for_tests();
        {
            let prompt = host
                .permission_prompt_mut_for_tests()
                .expect("prompt latched");
            prompt.set_rect(Rect::new(0.0, 0.0, 600.0, 80.0));
            let center = prompt.deny_rect().center();
            dispatch(prompt, &cursor_at(center));
            dispatch(prompt, &mouse_press());
            assert_eq!(prompt.pending_choice, Some(PromptChoice::Deny));
        }
        host.tick_permission_prompt_for_tests();

        let st = chrome.borrow();
        assert!(st.pending_grants.is_empty(), "queue must drain on Deny");
        let tree = st.tree();
        let ws = tree.desktop.active_workspace().expect("active workspace");
        assert!(
            !ws.app_grants_snapshot().contains_key("org.example.app"),
            "Deny must not persist any grant — got {:?}",
            ws.app_grants_snapshot().get("org.example.app"),
        );
    }
}

/// Per-test scratch dir under `target/`, off the user's real config dir.
fn scratch_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("target")
        .join("permission-prompt-pump")
        .join(format!("{}-{n}", std::process::id()));
    std::fs::create_dir_all(&base).expect("create scratch dir");
    base
}

/// Read the host's prompt slot without borrowing it for the rest of the scope.
fn host_has_no_prompt(host: &ShellOverlayHost) -> bool {
    // The accessor takes `&mut`; cast through a raw pointer to read the
    // discriminant without adding a parallel `&self` accessor. Read-only.
    let host_ptr: *const ShellOverlayHost = host;
    let host_mut: *mut ShellOverlayHost = host_ptr.cast_mut();
    let p = unsafe { (*host_mut).permission_prompt_mut_for_tests() };
    p.is_none()
}

// Imported for symmetry with the wider chrome-state API, unused here.
#[allow(
    dead_code,
    reason = "keeps imports shared with sibling fixtures referenced in this file"
)]
fn _suppress_unused() {
    let _ = OverlayFlags::default();
    let _ = Duration::from_secs(1);
    let _: VecDeque<u8> = VecDeque::new();
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use shell::shell::state::WorkspaceExt as _;

#[test]
fn latched_permission_choice_cannot_answer_a_new_request_from_the_same_app() {
    let (chrome, _) = chrome_with_pending_grant(
        "so.tilo.terminal",
        Capability::HostShell { scope: GRANT_ONCE },
    );
    let mut host = host_with_chrome(chrome.clone());
    host.tick_permission_prompt_for_tests();
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    {
        let mut state = chrome.borrow_mut();
        state.pending_grants.pop_front();
        state.pending_grants.push_back(PendingPrompt::with_reply(
            "so.tilo.terminal".into(),
            Capability::HostShell { scope: GRANT_ONCE },
            tx,
        ));
    }
    let prompt = host
        .permission_prompt_mut_for_tests()
        .expect("old prompt latched");
    prompt.set_rect(Rect::new(0.0, 0.0, 600.0, 80.0));
    let center = prompt.allow_rect().center();
    dispatch(prompt, &cursor_at(center));
    dispatch(prompt, &mouse_press());
    assert_eq!(prompt.pending_choice, Some(PromptChoice::Allow));
    host.tick_permission_prompt_for_tests();
    assert!(
        rx.try_recv().is_err(),
        "old click cannot release the new waiting request"
    );
    assert_eq!(chrome.borrow().pending_grants.len(), 1);
    assert!(
        host.permission_prompt_mut_for_tests().is_some(),
        "replacement is presented for a fresh decision"
    );
}

#[test]
fn permanent_permission_rechecks_owner_after_widget_latching() {
    let (chrome, _) = chrome_with_pending_grant(
        "so.tilo.terminal",
        Capability::HostShell { scope: GRANT_ONCE },
    );
    chrome.borrow_mut().test_only_owner_override = Some(true);
    let mut host = host_with_chrome(chrome.clone());
    host.tick_permission_prompt_for_tests();
    let prompt = host
        .permission_prompt_mut_for_tests()
        .expect("owner prompt");
    prompt.set_rect(Rect::new(0.0, 0.0, 600.0, 80.0));
    let center = prompt.always_rect().center();
    dispatch(prompt, &cursor_at(center));
    dispatch(prompt, &mouse_press());
    assert_eq!(prompt.pending_choice, Some(PromptChoice::Always));
    chrome.borrow_mut().test_only_owner_override = Some(false);
    host.tick_permission_prompt_for_tests();
    assert_eq!(
        chrome.borrow().pending_grants.len(),
        1,
        "owner loss must not consume permanent consent"
    );
    assert!(
        !host
            .permission_prompt_mut_for_tests()
            .expect("pending prompt")
            .is_owner
    );
}
