//! Sandbox Phase 4 Task 4.2 — integration tests for the
//! permission-prompt pump.
//!
//! Exercises the chrome ↔ overlay-host wiring without spinning up a
//! full render loop:
//!   1. Push a `(AppId, Capability)` onto `chrome.pending_grants`.
//!   2. Run a `tick_permission_prompt_for_tests` so the host latches
//!      a `PermissionPrompt` widget.
//!   3. Synthesize a click on one of the four buttons through the
//!      widget's own `handle_event` (via the
//!      `permission_prompt_mut_for_tests` accessor).
//!   4. Tick again so the pump drains the choice + applies the grant.
//!
//! Each test asserts both the queue state (front popped) and the
//! workspace's `grants` map (Once doesn't persist; Session/Always do).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use shell::shell::chrome_state::{ChromeState, OverlayFlags};
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    Capability, FsMode, GrantScope, Workspace, WorkspaceId, WorkspaceTree,
};
use ui::widget::{PermissionPrompt, PromptChoice, Widget};

use automerge::AutoCommit;
use taffy::TaffyTree;
use vello::kurbo::{Point, Rect};
use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};

fn synth_device() -> DeviceId {
    // SAFETY: `DeviceId` is opaque; integration-test scaffolding
    // already uses this pattern in `quick-ui::widget::slider`'s
    // tests + several other shell test files.
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

/// Drive an event through a [`PermissionPrompt`] with throwaway
/// scaffolding. Mirrors the shell's own `pump_permission_prompt`
/// driving — the test path writes through the widget's `handle_event`
/// directly.
fn dispatch(p: &mut PermissionPrompt, event: &WindowEvent) {
    let mut tree = TaffyTree::<()>::new();
    let node = tree.new_leaf(Default::default()).unwrap();
    let mut handled = false;
    let mut focus_target = None;
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    p.handle_event(
        event,
        1.0,
        &winit::event::Modifiers::default(),
        &mut doc,
        &root,
        &mut handled,
        &mut focus_target,
        node,
        &tree,
    );
}

/// Build a minimal chrome holding a single workspace + the requested
/// app's pending grant. Returns the chrome handle and the workspace's
/// id so the test can assert on the post-pump grants map.
fn chrome_with_pending_grant(
    app_id: &str,
    capability: Capability,
) -> (
    std::rc::Rc<std::cell::RefCell<ChromeState>>,
    WorkspaceId,
) {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::from(1);
    tree.workspaces.push(Workspace::new(id, "demo"));
    tree.set_active(id);
    let chrome = ChromeState::new(tree);
    {
        let mut st = chrome.borrow_mut();
        st.pending_grants
            .push_back((app_id.to_string(), capability));
    }
    (chrome, id)
}

/// Stub content widget. The host doesn't need a real strip tree to
/// exercise the prompt pump.
struct StubContent {
    rect: Rect,
}
impl ui::widget::Widget for StubContent {
    fn sync(&mut self, _s: &mut AutoCommit, _p: &automerge::ObjId) {}
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> taffy::NodeId {
        let n = taffy.new_leaf(Default::default()).unwrap();
        node_map.insert(n, self as *mut dyn ui::widget::Widget);
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
    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &automerge::ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<taffy::NodeId>,
        _node_id: taffy::NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }
    fn name(&self) -> &str {
        "StubContent"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn host_with_chrome(
    chrome: std::rc::Rc<std::cell::RefCell<ChromeState>>,
) -> ShellOverlayHost {
    let stub = Box::new(StubContent { rect: Rect::ZERO });
    ShellOverlayHost::new(stub).with_chrome(chrome)
}

#[test]
fn pump_drains_pending_grant_after_choice() {
    let (chrome, _ws_id) = chrome_with_pending_grant(
        "org.example.app",
        Capability::Camera {
            scope: GrantScope::Once,
        },
    );
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
        // Drive the prompt through a known geometry so the button
        // rects are deterministic.
        prompt.set_rect(Rect::new(0.0, 0.0, 600.0, 80.0));
        let center = prompt.allow_rect().center();
        dispatch(prompt, &cursor_at(center));
        dispatch(prompt, &left_press());
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
    let ws = st.tree.active_workspace().expect("active workspace");
    assert!(
        ws.grants.get("org.example.app").is_none(),
        "Once doesn't persist on the workspace's grants map — \
         got: {:?}",
        ws.grants.get("org.example.app"),
    );
}

#[test]
fn pump_persists_session_grant_to_workspace_grants_map() {
    let (chrome, _ws_id) = chrome_with_pending_grant(
        "org.example.app",
        Capability::Camera {
            scope: GrantScope::Once,
        },
    );
    let mut host = host_with_chrome(chrome.clone());

    // Latch the prompt.
    host.tick_permission_prompt_for_tests();
    {
        let prompt = host
            .permission_prompt_mut_for_tests()
            .expect("prompt latched");
        prompt.set_rect(Rect::new(0.0, 0.0, 600.0, 80.0));
        let center = prompt.session_rect().center();
        dispatch(prompt, &cursor_at(center));
        dispatch(prompt, &left_press());
        assert_eq!(prompt.pending_choice, Some(PromptChoice::Session));
    }

    // Drain.
    host.tick_permission_prompt_for_tests();

    let st = chrome.borrow();
    assert!(st.pending_grants.is_empty(), "queue must drain");
    let ws = st.tree.active_workspace().expect("active workspace");
    let caps = ws
        .grants
        .get("org.example.app")
        .expect("Session click must persist a grant on the active workspace");
    assert_eq!(caps.len(), 1);
    // Scope must be `Session`, not the pre-prompt `Once` — the pump
    // upgrades the scope based on the user's choice.
    assert!(
        matches!(
            &caps[0],
            Capability::Camera {
                scope: GrantScope::Session
            }
        ),
        "session grant scope must be Session, got {:?}",
        caps[0],
    );
}

#[test]
fn pump_persists_always_grant_with_filesystem_capability() {
    // Sanity that the scope-replacement helper round-trips a non-
    // Camera variant — Filesystem carries `path` + `mode` fields the
    // pump must preserve while flipping the scope.
    let (chrome, _ws_id) = chrome_with_pending_grant(
        "org.gnome.gedit",
        Capability::Filesystem {
            path: PathBuf::from("/home/u/work"),
            mode: FsMode::ReadWrite,
            scope: GrantScope::Once,
        },
    );
    let mut host = host_with_chrome(chrome.clone());

    host.tick_permission_prompt_for_tests();
    {
        let prompt = host
            .permission_prompt_mut_for_tests()
            .expect("prompt latched");
        // is_owner defaults to true via `is_active_workspace_owner`.
        assert!(prompt.is_owner);
        prompt.set_rect(Rect::new(0.0, 0.0, 600.0, 80.0));
        let center = prompt.always_rect().center();
        dispatch(prompt, &cursor_at(center));
        dispatch(prompt, &left_press());
        assert_eq!(prompt.pending_choice, Some(PromptChoice::Always));
    }
    host.tick_permission_prompt_for_tests();

    let st = chrome.borrow();
    let ws = st.tree.active_workspace().expect("active workspace");
    let caps = ws.grants.get("org.gnome.gedit").expect("grant persisted");
    assert_eq!(caps.len(), 1);
    assert!(
        matches!(
            &caps[0],
            Capability::Filesystem {
                mode: FsMode::ReadWrite,
                scope: GrantScope::Always,
                ..
            }
        ),
        "filesystem always grant must preserve mode + path while \
         flipping scope — got {:?}",
        caps[0],
    );
}

#[test]
fn pump_deny_drops_pending_grant_without_persisting() {
    let (chrome, _ws_id) = chrome_with_pending_grant(
        "org.example.app",
        Capability::Microphone {
            scope: GrantScope::Once,
        },
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
        dispatch(prompt, &left_press());
        assert_eq!(prompt.pending_choice, Some(PromptChoice::Deny));
    }
    host.tick_permission_prompt_for_tests();

    let st = chrome.borrow();
    assert!(st.pending_grants.is_empty(), "queue must drain on Deny");
    let ws = st.tree.active_workspace().expect("active workspace");
    assert!(
        ws.grants.get("org.example.app").is_none(),
        "Deny must not persist any grant — got {:?}",
        ws.grants.get("org.example.app"),
    );
}

#[test]
fn pump_does_not_latch_when_queue_is_empty() {
    let chrome = ChromeState::new(WorkspaceTree::default());
    {
        let mut st = chrome.borrow_mut();
        st.tree.workspaces.push(Workspace::new(WorkspaceId::from(1), "demo"));
        let id = WorkspaceId::from(1);
        st.tree.set_active(id);
    }
    let mut host = host_with_chrome(chrome);
    host.tick_permission_prompt_for_tests();
    assert!(
        host.permission_prompt_mut_for_tests().is_none(),
        "no widget should latch when there's nothing to surface",
    );
}

#[test]
fn pump_advances_to_next_grant_after_first_drain() {
    // Two queued grants for the same app — first an Allow, then a
    // Deny. Both should be visible in sequence; after the second
    // drain the queue is empty and no widget remains.
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::from(1);
    tree.workspaces.push(Workspace::new(id, "demo"));
    tree.set_active(id);
    let chrome = ChromeState::new(tree);
    {
        let mut st = chrome.borrow_mut();
        st.pending_grants.push_back((
            "org.example.app".into(),
            Capability::Camera {
                scope: GrantScope::Once,
            },
        ));
        st.pending_grants.push_back((
            "org.other.app".into(),
            Capability::Microphone {
                scope: GrantScope::Once,
            },
        ));
    }
    let mut host = host_with_chrome(chrome.clone());

    // First grant — Allow.
    host.tick_permission_prompt_for_tests();
    {
        let prompt = host
            .permission_prompt_mut_for_tests()
            .expect("prompt latched");
        assert_eq!(prompt.app_id, "org.example.app");
        prompt.set_rect(Rect::new(0.0, 0.0, 600.0, 80.0));
        let center = prompt.allow_rect().center();
        dispatch(prompt, &cursor_at(center));
        dispatch(prompt, &left_press());
    }
    host.tick_permission_prompt_for_tests();

    // Second grant — Deny.
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
        dispatch(prompt, &left_press());
    }
    host.tick_permission_prompt_for_tests();

    let st = chrome.borrow();
    assert!(st.pending_grants.is_empty());
    assert!(
        host_has_no_prompt(&host),
        "no widget remains after both grants are drained",
    );
}

/// Tests-only helper to read the host's prompt slot through the
/// public `permission_prompt_mut_for_tests` accessor without
/// borrowing it for the rest of the test scope.
fn host_has_no_prompt(host: &ShellOverlayHost) -> bool {
    // The accessor takes `&mut`, so we cast through unsafe to read
    // the slot without exposing a parallel `&self` accessor on the
    // host. Cheap and contained — the prompt is `Option<...>` and
    // we only need the discriminant.
    //
    // Done via a one-liner cast because `&ShellOverlayHost` already
    // implies stable layout for `'a` and we never write through the
    // pointer.
    let host_ptr: *const ShellOverlayHost = host;
    let host_mut: *mut ShellOverlayHost = host_ptr as *mut _;
    let p = unsafe { (*host_mut).permission_prompt_mut_for_tests() };
    p.is_none()
}

// Reference unused field/import suppression: `OverlayFlags`, `Duration`,
// `VecDeque` are pulled in for symmetry with the wider chrome-state API
// the prompt sits inside, even when this file's tests don't poke them.
#[allow(dead_code)]
fn _suppress_unused() {
    let _ = OverlayFlags::default();
    let _ = Duration::from_secs(1);
    let _: VecDeque<u8> = VecDeque::new();
}
