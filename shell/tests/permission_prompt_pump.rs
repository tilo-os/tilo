//! Integration tests for the permission-prompt pump (chrome <-> overlay-host
//! wiring, no render loop): queue a pending grant, tick to latch a
//! `PermissionPrompt`, click through the widget's own `handle_event`, tick
//! again to drain. Asserts queue state and the workspace `grants` map
//! (Once doesn't persist; Session/Always do).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use shell::shell::chrome_state::{ChromeState, OverlayFlags, PendingPrompt};
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    Capability, FsMode, GRANT_ALWAYS, GRANT_ONCE, GRANT_SESSION, Workspace, WorkspaceId,
    WorkspaceTree,
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

/// Drive an event through a [`PermissionPrompt`], mirroring the shell's own
/// `pump_permission_prompt`.
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

/// One workspace plus the requested app's pending grant. The workspace is
/// handed to the local user because an ownerless room has no administrators,
/// where "Always" is correctly refused.
fn chrome_with_pending_grant(
    app_id: &str,
    capability: Capability,
) -> (std::rc::Rc<std::cell::RefCell<ChromeState>>, WorkspaceId) {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::from(1);
    tree.workspaces.push(Workspace::new(id, "demo"));
    tree.set_active(id);
    let chrome = ChromeState::new(tree);
    {
        let mut st = chrome.borrow_mut();
        let owner = st.local_user_id.clone();
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
    fn sync(&mut self, _s: &mut AutoCommit, _p: &automerge::ObjId) {}
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> taffy::NodeId {
        let n = taffy.new_leaf(Default::default()).unwrap();
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
fn pump_drains_pending_grant_after_choice() {
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
    let tree = st.tree();
    let ws = tree.active_workspace().expect("active workspace");
    assert!(
        ws.app_grants_snapshot().get("org.example.app").is_none(),
        "Once doesn't persist on the workspace's grants map — \
         got: {:?}",
        ws.app_grants_snapshot().get("org.example.app"),
    );
}

#[test]
fn pump_persists_session_grant_to_workspace_grants_map() {
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
        dispatch(prompt, &left_press());
        assert_eq!(prompt.pending_choice, Some(PromptChoice::Session));
    }

    host.tick_permission_prompt_for_tests();

    let st = chrome.borrow();
    assert!(st.pending_grants.is_empty(), "queue must drain");
    let tree = st.tree();
    let ws = tree.active_workspace().expect("active workspace");
    let view = ws.app_grants_snapshot();
    let caps = view
        .get("org.example.app")
        .expect("Session click must persist a grant on the active workspace");
    assert_eq!(caps.len(), 1);
    // Scope must be `Session`, not the pre-prompt `Once` — the pump upgrades it.
    assert!(
        matches!(
            &caps[0],
            Capability::Camera {
                scope: GRANT_SESSION
            }
        ),
        "session grant scope must be Session, got {:?}",
        caps[0],
    );
}

#[test]
fn pump_persists_always_grant_with_filesystem_capability() {
    // Filesystem carries `path` + `mode` the pump must preserve across the flip.
    let granted_path = PathBuf::from("/home/u/work");
    let (chrome, _ws_id) = chrome_with_pending_grant(
        "org.gnome.gedit",
        Capability::Filesystem {
            path: granted_path.clone(),
            mode: FsMode::ReadWrite,
            scope: GRANT_ONCE,
        },
    );
    // "Always" means "still granted after a reboot", so read the state file back
    // — otherwise this only proves the grant reached memory.
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
        dispatch(prompt, &left_press());
        assert_eq!(prompt.pending_choice, Some(PromptChoice::Always));
    }
    host.tick_permission_prompt_for_tests();

    let expect_grant = |ws: &Workspace, where_: &str| {
        let view = ws.app_grants_snapshot();
        let caps = view
            .get("org.gnome.gedit")
            .unwrap_or_else(|| panic!("no grant {where_}"));
        assert_eq!(caps.len(), 1, "one grant {where_}, got {caps:?}");
        assert!(
            matches!(
                &caps[0],
                Capability::Filesystem {
                    path,
                    mode: FsMode::ReadWrite,
                    scope: GRANT_ALWAYS,
                } if path == &granted_path
            ),
            "filesystem always grant must preserve mode + path while \
             flipping scope ({where_}) — got {:?}",
            caps[0],
        );
    };

    {
        let st = chrome.borrow();
        let tree = st.tree();
        expect_grant(
            tree.active_workspace().expect("active workspace"),
            "on the active workspace",
        );
    }

    // The write is eager because the compositor can be killed rather than asked
    // to exit; reload the way the boot path does.
    let reloaded = shell::shell::persist::load_tree_from(&state_file)
        .expect("the pump must have written the state file");
    let ws = reloaded
        .workspaces
        .iter()
        .find(|w| w.id == WorkspaceId::from(1))
        .expect("the workspace survives the reload");
    expect_grant(ws, "after reloading the state file");
}

/// Per-test scratch dir under `target/`, off the user's real config dir.
fn scratch_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("target")
        .join("permission-prompt-pump")
        .join(format!("{}-{n}", std::process::id()));
    std::fs::create_dir_all(&base).expect("create scratch dir");
    base
}

#[test]
fn pump_deny_drops_pending_grant_without_persisting() {
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
        dispatch(prompt, &left_press());
        assert_eq!(prompt.pending_choice, Some(PromptChoice::Deny));
    }
    host.tick_permission_prompt_for_tests();

    let st = chrome.borrow();
    assert!(st.pending_grants.is_empty(), "queue must drain on Deny");
    let tree = st.tree();
    let ws = tree.active_workspace().expect("active workspace");
    assert!(
        ws.app_grants_snapshot().get("org.example.app").is_none(),
        "Deny must not persist any grant — got {:?}",
        ws.app_grants_snapshot().get("org.example.app"),
    );
}

#[test]
fn pump_does_not_latch_when_queue_is_empty() {
    let chrome = ChromeState::new(WorkspaceTree::default());
    {
        let st = chrome.borrow_mut();
        let mut tree = st.tree_mut();
        tree.workspaces
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

#[test]
fn pump_advances_to_next_grant_after_first_drain() {
    // Allow then Deny across two apps: both visible in sequence, nothing left.
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::from(1);
    tree.workspaces.push(Workspace::new(id, "demo"));
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
        dispatch(prompt, &left_press());
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
#[allow(dead_code)]
fn _suppress_unused() {
    let _ = OverlayFlags::default();
    let _ = Duration::from_secs(1);
    let _: VecDeque<u8> = VecDeque::new();
}
