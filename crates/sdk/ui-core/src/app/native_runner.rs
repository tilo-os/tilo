//! The native event-loop runner: the winit window, render thread, AccessKit
//! adapter and surface host around [`App`]. Compiled with the `native`
//! feature; [`App::run`] is its entry point.
//!
//! It lives beside `App` rather than in [`crate::native`] because it drives
//! `App`'s private state directly; application semantics remain on `App` and
//! its responsibility-specific implementation units.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_lock::MutexRecover as _;

mod held_frame;
mod ime;
#[cfg(target_os = "linux")]
mod linux_header;
mod private_text_input;
mod process_windows;
mod slot_bar;
#[cfg(any(windows, target_os = "linux"))]
pub(super) mod startup;
mod wheel_batch;

use ime::{
    NativeImeWindowState, PrimaryPreeditUpdate, apply_native_ime, apply_native_ime_content,
    pending_floating_ime_state, preedit_after_event, primary_floating_ime_state,
    primary_ime_state_for_held_child, retained_return_ime_state, routes_to_floating_ime,
    secondary_ime_state,
};

#[cfg(not(target_os = "android"))]
type PrimaryA11yAdapter = accesskit_winit::Adapter;
#[cfg(target_os = "android")]
type PrimaryA11yAdapter = crate::accesskit_android_adapter::Adapter;

#[cfg(not(target_os = "android"))]
fn create_accessibility_adapter(
    event_loop: &ActiveEventLoop,
    window: &Window,
    proxy: EventLoopProxy<UserEvent>,
) -> Option<PrimaryA11yAdapter> {
    Some(PrimaryA11yAdapter::with_event_loop_proxy(
        event_loop, window, proxy,
    ))
}

#[cfg(target_os = "android")]
fn create_accessibility_adapter(
    event_loop: &ActiveEventLoop,
    window: &Window,
    proxy: EventLoopProxy<UserEvent>,
) -> Option<PrimaryA11yAdapter> {
    match PrimaryA11yAdapter::new(event_loop, window, proxy) {
        Ok(adapter) => Some(adapter),
        Err(error) => {
            crate::diag!("tilo: no TalkBack access: {error}");
            None
        }
    }
}

/// wgpu `RenderContext`, `RenderSurface`, and `vello::Renderer` live on a
/// dedicated worker thread (see [`crate::render_thread`]) — this struct
/// keeps only the channel ends to ship frames and recycle scene buffers.
struct AppHandler {
    app: App,
    clipboard_attachment: Option<crate::clipboard::ClipboardAttachment>,
    animation_schedule: animation_schedule::AnimationSchedule,
    /// When the next "just typed" highlight steps down its fade
    /// (`PresenceCtx::typing_next_step`).
    typing_step_at: Option<crate::time::Instant>,
    /// A frame held for a local read ([`crate::arriving`]): drawn when the
    /// read answers, or at this instant when its frame budget runs out.
    held_frame: Option<crate::time::Instant>,
    /// Debug check that animation frames move something.
    still_animation: animation_schedule::StillAnimation,
    native_scenario: Option<crate::native_scenario::Scenario>,
    native_input_ids: Vec<usize>,
    physical_input_trace: Option<crate::physical_input_trace::Trace>,
    native_input_origin: Option<Instant>,
    native_input_pointer_passthrough: bool,
    native_input_route_isolation: bool,
    /// `None` until the first `resumed` — every modern platform gates window
    /// creation behind an `ActiveEventLoop`.
    window: Option<Arc<Window>>,
    /// `None` again in `exiting`, after the worker is joined, so its surface
    /// drops BEFORE the `Window` arc goes away.
    render: Option<RenderThreadHandle>,
    /// Between `suspended` and the next `resumed` the window has no native
    /// surface to render into, so a redraw must not try to create one.
    surface_suspended: bool,
    /// What `suspended` took back from the render worker, for the next
    /// surface.
    suspended_renderer: Option<PreparedRenderer>,
    /// A real, separate fullscreen output for a root widget's audience view.
    /// It is deliberately not modelled as a floating surface: it has no
    /// desktop-space proxy or compositor handoff and is always destroyed
    /// before its renderer/window during stop and app teardown.
    audience_presentation: Option<AudiencePresentationWindow>,
    floating_windows: Vec<NativeFloatingWindow>,
    /// `None` until `resumed` creates the window (must be built while
    /// hidden); lazy, so the hot path pays nothing with no screen reader
    /// running.
    a11y_adapter: Option<PrimaryA11yAdapter>,
    /// Also carries host-chrome requests back into the loop.
    a11y_proxy: EventLoopProxy<UserEvent>,
    /// Last text-input-v3 state applied to winit. Edge-triggering this avoids
    /// emitting a protocol commit on every idle loop.
    ime_state: Option<NativeImeWindowState>,
    /// A real preedit belongs to the primary Wayland text-input resource.
    /// During an exact compositor-authorised Float handoff, keep that
    /// protocol resource active and route only its keyboard/IME events to the
    /// same retained editor now painted in the secondary surface.
    primary_preedit_active: bool,
    /// Winit emits an empty preedit before a commit from the same Wayland
    /// `done` batch. Retire at loop idle only if no commit followed it.
    primary_preedit_clear_pending: bool,
    floating_ime_continuation: Option<String>,
    /// Reverse counterpart: an exact suspended child keeps the real
    /// text-input protocol until its live composition commits, while its
    /// keyboard/IME events reach the reattached primary editor.
    floating_ime_return_continuation: Option<String>,
    private_input: private_text_input::Bridge,
    /// Wheel deltas of this wake, routed as one event ([`wheel_batch`]).
    wheel_batch: wheel_batch::WheelBatch,
    #[cfg(target_os = "android")]
    text_input: super::android_text_input::TextInputSession,
    /// The field with the keyboard was touched since the last IME sync.
    #[cfg(target_os = "android")]
    text_input_touched: bool,
    primary_frames_sent: u64,
    primary_frames_completed: u64,
    /// The last submitted frame painted something over the widgets that
    /// their damage does not describe; the next frame must erase it.
    top_layer_drawn: bool,
    /// The collaborators' marks painted over the widgets, as last submitted,
    /// so a frame damages only the marks that changed.
    presence_layer: crate::presence::PresenceLayer,
    /// Bounds what the presence layer draws outside its parts, so scrolls
    /// beneath peer marks still copy ([`peer_marks`]).
    mark_bounds: crate::scene_parts::DrawBounds,
    last_primary_frame_presented: u64,
    primary_retry: crate::render_thread::retry::RenderRetryState,
    /// `None` waits for the next primary submission; `Some(sequence)` waits
    /// until that exact-or-newer frame is reported Presented by the worker.
    floating_source_clear_frames: std::collections::HashMap<String, Option<u64>>,
    /// Reverse ownership gate. A suspended native surface remains covered by
    /// its compositor-frozen final frame until one primary app frame with the
    /// reattached widget is reported Presented.
    floating_destination_present_frames: std::collections::HashMap<String, Option<u64>>,
    /// Maps one-finger taps onto the existing pointer event contract so every
    /// standard and app-local control gets touch without bespoke branches.
    touch_pointer: crate::interaction::TouchPointerEmulator,
    /// Synthetic mouse events must retain touch modality rather than making
    /// the host immediately classify them as a fine pointer.
    dispatching_touch_pointer: bool,
    /// Per-window state handed to the next in-place reload. Keyed by the
    /// window id that stays for the life of the process.
    dev_window_state: std::collections::HashMap<String, Vec<u8>>,
    /// The reload library this process runs. The dev loop stages the first one
    /// from the build that produced this host, so it starts as the running code.
    dev_loaded_library: Option<std::path::PathBuf>,
    /// The last in-place reload, until its first frame is submitted.
    dev_reload_timing: Option<super::DevReloadTiming>,
    /// The menu bar and title-bar search field, when the host draws them.
    #[cfg(target_os = "macos")]
    host_chrome: Option<crate::host_chrome::macos::MacChrome>,
    /// The menu bar and Jump List, when the host draws them.
    #[cfg(windows)]
    host_chrome: Option<crate::host_chrome::windows::WindowsChrome>,
    /// What the host chrome last presented, to skip unchanged refreshes.
    #[cfg(any(target_os = "macos", windows))]
    host_commands_key: Option<super::host_commands::HostCommandsKey>,
    /// Who decorates the window, and the header bar when the app does.
    #[cfg(target_os = "linux")]
    decorations: linux_header::Decorations,
    /// The app's window-bar slots, drawn here while nothing hosts them.
    slot_bar: slot_bar::SlotBar,
    /// The host OS's appearance, for a standalone app outside Tilo OS.
    host_appearance: Option<crate::host_appearance::Source>,
    /// Settings.json changed while this process was idle. The next
    /// `about_to_wait` skips the 750ms poll debounce and re-reads Theme.
    settings_store_changed: Arc<AtomicBool>,
    /// Lives for the event loop so an idle app still hears Theme Studio
    /// and compositor appearance writes.
    _settings_watch: Option<settings_store::watch::SettingsWatch>,
    /// The workspace's identity was chosen while this window waited for it
    /// ([`crate::workspace_identity`]): the loop ends with the window kept
    /// for the launch to open the app in.
    handing_over: bool,
    /// The window is done: the last window of the process ends its loop, any
    /// other closes alone ([`process_windows`]).
    closing: std::cell::Cell<bool>,
}

#[cfg(test)]
mod floating_source_focus_tests {
    use super::*;

    type FocusEvents = std::rc::Rc<std::cell::RefCell<Vec<(Option<String>, bool)>>>;

    #[derive(Default)]
    struct FocusProbe {
        events: FocusEvents,
        drawable_ids: Vec<&'static str>,
        request: Option<crate::floating_surface::FloatingSurfaceRequest>,
    }

    impl crate::widget::AppWidget for FocusProbe {
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Editor
        }
        fn app_title(&self) -> &str {
            "Focus probe"
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Named("chat".into())
        }
    }

    impl crate::widget::FloatingSurfaceProvider for FocusProbe {
        fn take_floating_surface_request(
            &mut self,
        ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
            self.request.take()
        }

        fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>) {
            self.events
                .borrow_mut()
                .push((id.map(|id| format!("ime:{id}")), false));
        }

        fn draw_floating_surface(
            &mut self,
            id: &str,
            _scene: &mut Scene,
            _transform: Affine,
            _text: &mut TextRenderer,
            _theme: &Theme,
            _presence: &crate::PresenceCtx,
        ) -> bool {
            if !self.drawable_ids.contains(&id) {
                return false;
            }
            self.events
                .borrow_mut()
                .push((Some(format!("draw:{id}")), false));
            true
        }

        fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
            self.events
                .borrow_mut()
                .push((id.map(|id| format!("input:{id}")), false));
        }
        fn floating_surface_accessibility(&self, id: &str) -> Option<crate::a11y::AccessNode> {
            (id == "a").then(|| {
                crate::a11y::AccessNode::new(crate::a11y::Role::Button)
                    .with_hashed_key("source-a-send")
                    .with_action_target("source-a-send")
            })
        }
        fn perform_floating_surface_accessibility_action(
            &mut self,
            id: &str,
            target: &str,
            action: &crate::a11y::Action,
            _state: &mut crate::widget::Edits<'_>,
            _path: &automerge::ObjId,
        ) -> bool {
            let accepted = id == "a"
                && target == "source-a-send"
                && matches!(action, crate::a11y::Action::Activate);
            if accepted {
                self.events.borrow_mut().push((Some(target.into()), true));
            }
            accepted
        }
        fn floating_surface_ime(&self, id: &str) -> Option<(crate::ime::InputContent, KurboRect)> {
            (id == "a").then_some((
                crate::ime::InputContent::TEXT,
                KurboRect::new(-700.0, 500.0, -699.0, 520.0),
            ))
        }
        fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool) {
            self.events
                .borrow_mut()
                .push((id.map(str::to_owned), focused));
        }
    }

    impl Widget for FocusProbe {
        crate::provides!(dyn crate::widget::FloatingSurfaceProvider);

        fn sync(&mut self, _: &crate::widget::AppModel, _: &crate::widget::ModelPath) {}

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _: &Theme,
            _: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            use crate::widget::TaffyTreeExt as _;
            taffy.leaf_node(taffy::Style::default())
        }

        fn name(&self) -> &'static str {
            "FocusProbe"
        }
    }

    #[test]
    fn retained_inactive_tab_forwards_exact_draw_and_ime_source_through_the_host_chain() {
        use crate::widget::{GroupRow, TabDescriptor, TileContent, WindowGroup, WindowWidth};
        let probe = FocusProbe {
            drawable_ids: vec!["a", "b"],
            request: Some(crate::floating_surface::FloatingSurfaceRequest {
                id: "b".into(),
                title: "Conversation".into(),
                role: crate::floating_surface::FloatingRole::Conversation,
                source: crate::floating_surface::FloatingSource::AppRegion {
                    app_id: "chat".into(),
                    region: KurboRect::new(0.0, 0.0, 500.0, 500.0),
                },
                preferred_size: vello::kurbo::Size::new(500.0, 500.0),
                aspect_ratio: None,
                initial_rect: None,
                presentation_rect: None,
                tracking_source: None,
            }),
            ..Default::default()
        };
        let events = std::rc::Rc::clone(&probe.events);
        let group = WindowGroup::new(
            1,
            vec![
                (
                    TabDescriptor::new(1, "Primary"),
                    TileContent::App(Box::new(FocusProbe::default())),
                ),
                (
                    TabDescriptor::new(2, "Conversation"),
                    TileContent::App(Box::new(probe)),
                ),
            ],
            1,
            WindowWidth::Normal,
        );
        let mut root = GroupRow::new(vec![group]);
        assert_eq!(root.take_floating_surface_request().unwrap().id, "b");
        assert!(root.take_floating_surface_request().is_none());
        root.on_floating_surface_ime_source_changed(Some("b"));
        forward_native_surface_focus(&mut root, Some("b"), &WindowEvent::Focused(true));
        // The selected tab does not own either floating presentation. The
        // exact pass must reach the retained app through its AppWidget loan.
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        assert!(root.draw_floating_surface(
            "b",
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::default(),
            &crate::PresenceCtx::empty()
        ));
        assert!(!root.draw_floating_surface(
            "missing",
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::default(),
            &crate::PresenceCtx::empty()
        ));
        root.on_floating_surface_ime_source_changed(None);
        assert_eq!(
            *events.borrow(),
            vec![
                (Some("ime:b".into()), false),
                (Some("b".into()), true),
                (Some("draw:b".into()), false),
                (None, false),
            ]
        );
    }

    #[test]
    fn secondary_accessibility_dispatch_reaches_only_the_declared_source_control() {
        let mut root = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(
            FocusProbe::default(),
        )));
        let access = root.floating_surface_accessibility("a").unwrap();
        let tree = crate::accesskit_bridge::surface_tree_update(
            Some(&access),
            vello::kurbo::Point::ZERO,
            1.0,
        );
        let target_node = tree
            .nodes
            .iter()
            .find(|(_, node)| node.role() == accesskit::Role::Button)
            .map(|(id, _)| *id)
            .unwrap();
        let request = accesskit::ActionRequest {
            action: accesskit::Action::Click,
            target_node,
            target_tree: accesskit::TreeId::ROOT,
            data: None,
        };
        let mut state = automerge::AutoCommit::new();
        assert!(!dispatch_native_surface_accessibility(
            &mut root,
            "b",
            &request,
            &mut crate::widget::Edits::lend(&mut state),
            &automerge::ROOT,
        ));
        assert!(dispatch_native_surface_accessibility(
            &mut root,
            "a",
            &request,
            &mut crate::widget::Edits::lend(&mut state),
            &automerge::ROOT,
        ));
    }

    #[test]
    fn reused_native_surface_restores_focus_without_a_new_focus_event() {
        let probe = FocusProbe::default();
        let events = std::rc::Rc::clone(&probe.events);
        let mut root =
            crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(probe)));
        let mut lifecycle = NativeFloatingSurfaceLifecycle {
            adopted: true,
            suspended: false,
            focused: true,
        };
        forward_native_surface_focus(&mut root, Some("a"), &WindowEvent::Focused(true));

        for cycle in 1..=2 {
            lifecycle.suspend(&mut root, "a");
            assert!(lifecycle.suspended);
            assert!(!lifecycle.focused);
            assert!(!lifecycle.adopted);
            assert_eq!(events.borrow().last(), Some(&(Some("a".into()), false)));

            // The retained native window is still the seat's focus owner.
            // There is no second Focused(true) edge for the runner to receive.
            lifecycle.resume(&mut root, "a", true);
            assert!(
                lifecycle.focused,
                "Float cycle {cycle} must reconcile the retained window's actual focus"
            );
            assert!(!lifecycle.suspended);
            assert!(!lifecycle.adopted);
            assert_eq!(
                events.borrow().last(),
                Some(&(Some("a".into()), true)),
                "the retained editor must receive the same focus as its native window"
            );
            lifecycle.adopted = true;
        }
    }

    #[test]
    fn reused_native_surface_does_not_restore_focus_after_the_host_moves_it() {
        let probe = FocusProbe::default();
        let events = std::rc::Rc::clone(&probe.events);
        let mut root =
            crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(probe)));
        let mut lifecycle = NativeFloatingSurfaceLifecycle {
            focused: true,
            ..Default::default()
        };
        lifecycle.suspend(&mut root, "a");
        events.borrow_mut().clear();

        lifecycle.resume(&mut root, "a", false);

        assert!(!lifecycle.focused);
        assert!(!lifecycle.suspended);
        assert_eq!(
            *events.borrow(),
            vec![(Some("a".into()), false)],
            "resume must report the host's current focus, even if the editor was previously focused"
        );
    }

    #[test]
    fn secondary_window_ime_uses_source_focus_and_local_coordinates() {
        let probe = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(
            FocusProbe::default(),
        )));
        let source = KurboRect::new(-800.0, 200.0, -300.0, 700.0);
        let state = secondary_ime_state(&probe, "a", source, true).unwrap();
        assert_eq!(
            (state.x, state.y, state.width, state.height),
            (100, 300, 1, 20)
        );
        assert!(secondary_ime_state(&probe, "a", source, false).is_none());
        assert!(secondary_ime_state(&probe, "b", source, true).is_none());
    }

    #[test]
    fn secondary_accesskit_event_keeps_originating_native_window() {
        let window_id = winit::window::WindowId::dummy();
        let event: UserEvent = accesskit_winit::Event {
            window_id,
            window_event: accesskit_winit::WindowEvent::InitialTreeRequested,
        }
        .into();
        assert!(
            matches!(event, UserEvent::AccessKit { window_id: source, event: accesskit_winit::WindowEvent::InitialTreeRequested } if source == window_id)
        );
    }

    #[test]
    fn native_focus_forwarding_preserves_surface_identity() {
        let probe = FocusProbe::default();
        let events = std::rc::Rc::clone(&probe.events);
        let mut root =
            crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(probe)));
        forward_native_surface_focus(&mut root, Some("agent-a"), &WindowEvent::Focused(true));
        forward_native_surface_focus(&mut root, Some("agent-b"), &WindowEvent::Focused(true));
        forward_native_surface_focus(&mut root, Some("agent-a"), &WindowEvent::Focused(false));
        assert_eq!(
            *events.borrow(),
            vec![
                (Some("agent-a".into()), true),
                (Some("agent-b".into()), true),
                (Some("agent-a".into()), false),
            ]
        );
    }
}

#[cfg(target_os = "macos")]
impl AppHandler {
    /// Put History, Search and Undo into the menu bar and title bar.
    fn install_host_chrome(&mut self, window: &Window) {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if crate::host_chrome::HostChrome::current(self.app.input.platform)
            != crate::host_chrome::HostChrome::Native
        {
            return;
        }
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        let proxy = self.a11y_proxy.clone();
        let chrome = crate::host_chrome::macos::MacChrome::new(mtm, move |request| {
            let _ = proxy.send_event(match request {
                crate::host_chrome::macos::ChromeRequest::Command(invocation) => {
                    UserEvent::HostCommand(invocation)
                }
                crate::host_chrome::macos::ChromeRequest::Search(query) => {
                    UserEvent::HostSearch(query)
                }
            });
        });
        chrome.install(&objc2_app_kit::NSApplication::sharedApplication(mtm));
        if let Ok(handle) = window.window_handle()
            && let RawWindowHandle::AppKit(handle) = handle.as_raw()
        {
            // SAFETY: winit hands out the live NSView of a window it owns, on
            // the main thread, for at least the duration of this call.
            let view = unsafe { handle.ns_view.cast::<objc2_app_kit::NSView>().as_ref() };
            chrome.attach_toolbar(view);
        }
        self.host_chrome = Some(chrome);
        self.host_commands_key = None;
    }
}

#[cfg(any(target_os = "macos", windows))]
impl AppHandler {
    /// Hand the host chrome fresh commands when anything they show changed.
    fn refresh_host_chrome(&mut self) {
        if let Some(chrome) = &self.host_chrome {
            let key = self.app.host_commands_key();
            if self.host_commands_key != Some(key) {
                self.host_commands_key = Some(key);
                chrome.update(self.app.host_commands(true));
            }
        }
    }
}

#[cfg(windows)]
impl AppHandler {
    /// Put History, Search and Undo into the window's menu bar and the Jump
    /// List, and run the command a Jump List task launched this process with.
    fn install_host_chrome(&mut self, window: &Window) {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let proxy = self.a11y_proxy.clone();
        if let Some(invocation) = crate::host_chrome::windows::take_startup_command() {
            let _ = proxy.send_event(UserEvent::HostCommand(invocation));
        }
        if crate::host_chrome::HostChrome::current(self.app.input.platform)
            != crate::host_chrome::HostChrome::Native
        {
            return;
        }
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return;
        };
        let hwnd = windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut std::ffi::c_void);
        self.host_chrome = Some(crate::host_chrome::windows::WindowsChrome::install(
            hwnd,
            move |invocation| {
                let _ = proxy.send_event(UserEvent::HostCommand(invocation));
            },
        ));
        self.host_commands_key = None;
    }
}

/// The reload library the dev session currently points at.
fn dev_reload_library(session: &std::path::Path) -> Option<std::path::PathBuf> {
    std::fs::read_to_string(session.join("reload.path"))
        .ok()
        .map(|text| std::path::PathBuf::from(text.trim()))
        .filter(|path| path.is_file())
}

struct AudiencePresentationWindow {
    monitor: winit::monitor::MonitorHandle,
    window: Arc<Window>,
    render: Option<RenderThreadHandle>,
    render_init: Option<mpsc::Receiver<Result<RenderThreadHandle, String>>>,
}

/// Keep the audience bound to the same monitor object across hotplug list
/// reordering; indices are only a one-tick selection aid.
fn audience_monitor_still_selected<T: PartialEq>(
    current: Option<&T>,
    selected: Option<&T>,
) -> bool {
    current
        .zip(selected)
        .is_some_and(|(current, selected)| current == selected)
}

#[cfg(test)]
mod audience_monitor_identity_tests {
    use super::audience_monitor_still_selected;

    #[test]
    fn stable_identity_survives_monitor_enumeration_reordering() {
        let audience = "connector-2";
        let reordered = ["connector-2", "connector-1"];
        assert!(audience_monitor_still_selected(
            reordered.first(),
            Some(&audience)
        ));
        assert!(!audience_monitor_still_selected(
            reordered.get(1),
            Some(&audience)
        ));
    }
}

struct NativeFloatingWindow {
    id: String,
    token: String,
    desktop_rect: KurboRect,
    coordinate_offset: Vec2,
    window: Arc<Window>,
    render: Option<RenderThreadHandle>,
    /// Device/surface creation is much slower than a frame on software and
    /// headless backends.  Keeping it off the winit thread lets the frozen
    /// compositor proxy continue following the pointer during handoff.
    render_init: Option<mpsc::Receiver<Result<RenderThreadHandle, String>>>,
    prepared_at: Instant,
    lifecycle: NativeFloatingSurfaceLifecycle,
    a11y_adapter: Option<PrimaryA11yAdapter>,
    ime_state: Option<NativeImeWindowState>,
    preedit_active: bool,
    preedit_clear_pending: bool,
}

#[derive(Default)]
struct NativeFloatingSurfaceLifecycle {
    adopted: bool,
    suspended: bool,
    focused: bool,
}

impl NativeFloatingSurfaceLifecycle {
    fn suspend(&mut self, root: &mut dyn Widget, id: &str) {
        self.focused = false;
        self.suspended = true;
        self.adopted = false;
        root.on_floating_surface_focus_changed(Some(id), false);
    }

    fn resume(&mut self, root: &mut dyn Widget, id: &str, native_focused: bool) {
        self.adopted = false;
        self.suspended = false;
        // Hiding the retained surface clears its editor's focus, but the
        // native seat may still own this exact window. Reconcile its current
        // focus: resuming does not necessarily produce another focus edge.
        self.focused = native_focused;
        root.on_floating_surface_focus_changed(Some(id), native_focused);
    }
}

/// Resolve the drawable presentation region inside a transparent native
/// floating allocation. The intersection is deliberate: a presentation
/// update and a platform resize can cross for one frame, but neither may paint
/// outside the buffer that is actually being submitted.
fn native_floating_window_clip(window: KurboRect, presentation: KurboRect) -> Option<KurboRect> {
    if !window.x0.is_finite()
        || !window.y0.is_finite()
        || !window.x1.is_finite()
        || !window.y1.is_finite()
        || !presentation.x0.is_finite()
        || !presentation.y0.is_finite()
        || !presentation.x1.is_finite()
        || !presentation.y1.is_finite()
        || window.width() <= 0.0
        || window.height() <= 0.0
        || presentation.width() <= 0.0
        || presentation.height() <= 0.0
    {
        return None;
    }
    let clipped = window.intersect(presentation);
    (clipped.width() > 0.0 && clipped.height() > 0.0).then_some(clipped)
}

#[cfg(test)]
mod native_floating_paint_tests {
    use super::native_floating_window_clip;
    use vello::kurbo::Rect;

    #[test]
    fn content_ground_is_clipped_to_the_submitted_native_window() {
        let window = Rect::new(-16.0, 120.0, 544.0, 480.0);
        assert_eq!(
            native_floating_window_clip(window, Rect::new(0.0, 120.0, 520.0, 480.0)),
            Some(Rect::new(0.0, 120.0, 520.0, 480.0))
        );
        assert_eq!(
            native_floating_window_clip(window, Rect::new(-32.0, 100.0, 560.0, 500.0)),
            Some(window),
            "a crossing presentation update cannot paint beyond this buffer"
        );
        assert_eq!(
            native_floating_window_clip(window, Rect::new(600.0, 120.0, 700.0, 220.0)),
            None
        );
    }

    #[test]
    fn malformed_content_never_becomes_an_opaque_desktop_spill() {
        let window = Rect::new(0.0, 0.0, 560.0, 360.0);
        assert_eq!(
            native_floating_window_clip(window, Rect::new(0.0, 0.0, f64::NAN, 360.0)),
            None
        );
        assert_eq!(
            native_floating_window_clip(window, Rect::new(0.0, 0.0, 0.0, 360.0)),
            None
        );
    }
}

fn forward_native_surface_focus(root: &mut dyn Widget, surface: Option<&str>, event: &WindowEvent) {
    if let WindowEvent::Focused(focused) = event {
        root.on_floating_surface_focus_changed(surface, *focused);
    }
}

fn dispatch_native_surface_accessibility(
    root: &mut dyn Widget,
    id: &str,
    request: &accesskit::ActionRequest,
    state: &mut crate::widget::Edits<'_>,
    path: &automerge::ObjId,
) -> bool {
    if request.target_tree != accesskit::TreeId::ROOT {
        return false;
    }
    let Some(action) =
        crate::accesskit_bridge::accesskit_action_to_ours(request.action, request.data.as_ref())
    else {
        return false;
    };
    let Some(access) = root.floating_surface_accessibility(id) else {
        return false;
    };
    let Some(target) = crate::accesskit_bridge::surface_action_target(&access, request.target_node)
    else {
        return false;
    };
    root.perform_floating_surface_accessibility_action(id, target, &action, state, path)
}

/// App-owned surfaces remain fixed to their live presentation rectangle.
/// Winit's non-resizable Wayland window captures min/max at creation; a
/// later request_inner_size does not advance those hints. Keep them aligned
/// so the next activation configure cannot restore the transfer-frame size.
fn resize_native_floating_window(window: &Window, rect: KurboRect) {
    let size = native_floating_logical_size(rect);
    window.set_min_inner_size(Some(size));
    window.set_max_inner_size(Some(size));
    let _ = window.request_inner_size(size);
}

fn draw_debug_outlines(app: &mut App, scene: &mut Scene, transform: Affine) {
    fn draw_debug_recursive(
        widget: &dyn Widget,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        depth: usize,
        cycle: &[Color; 3],
        label_ground: Color,
    ) {
        let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
            return;
        };
        let rect = widget.get_rect();
        let &[a, b, c] = cycle;
        let color = match depth.checked_rem(3).unwrap_or(0) {
            0 => a,
            1 => b,
            _ => c,
        };
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            transform,
            color,
            None,
            &rect,
        );
        let label = format!("{}: {}", widget.name(), depth);
        let text_y = (rect.y0 + 12.0).min(rect.y1);
        let bg_rect = KurboRect::new(rect.x0, rect.y0, rect.x0 + 100.0, rect.y0 + 14.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            label_ground,
            None,
            &bg_rect,
        );
        text_renderer.draw_text(
            scene,
            &label,
            10.0,
            color,
            {
                #[allow(clippy::arithmetic_side_effects, reason = "Affine composition/translation is matrix multiplication on f64 fields; it cannot panic or overflow")]
                let label_transform = transform * Affine::translate((rect.x0 + 2.0, text_y));
                label_transform
            },
            None,
        );
        widget.for_each_child(&mut |child| {
            draw_debug_recursive(
                child,
                scene,
                transform,
                text_renderer,
                depth.saturating_add(1),
                cycle,
                label_ground,
            );
        });
    }
    let debug_outline_cycle = [
        app.theme.tokens.colors.semantic.warning,
        app.theme.tokens.colors.semantic.info,
        app.theme.tokens.colors.semantic.success,
    ];
    let label_ground = app
        .theme
        .tokens
        .colors
        .surfaces
        .scrim
        .with_alpha(180.0 / 255.0);
    draw_debug_recursive(
        app.root_widget.as_ref(),
        scene,
        transform,
        &mut app.text_renderer,
        0,
        &debug_outline_cycle,
        label_ground,
    );
}

/// Whether this windowing system shows a window icon the app sets. winit's
/// Wayland backend drops it (`set_window_icon` is empty there; compositors
/// take the icon from the app id), so reading and decoding one before the
/// first frame would be wasted.
fn takes_window_icon(event_loop: &ActiveEventLoop) -> bool {
    use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    !event_loop
        .display_handle()
        .is_ok_and(|display| matches!(display.as_raw(), RawDisplayHandle::Wayland(_)))
}

/// The slow-event reporting threshold in milliseconds, when the developer
/// overrides it with `TILO_SLOW_EVENT_MS`; see `slow_event`.
pub(super) fn slow_event_threshold_ms() -> Option<u64> {
    std::env::var("TILO_SLOW_EVENT_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// The Wayland application id (X11 `WM_CLASS`) of the primary window: the
/// app's own (`explicit`, an input surface's), else its package's. See
/// `resolve_app_id`: a shared id collapses per-app compositor lookups.
fn primary_window_app_id(explicit: Option<&str>) -> String {
    let package_id = std::env::var("TILO_PACKAGE_ID").ok();
    resolve_app_id(
        explicit.or(package_id.as_deref()),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .as_deref(),
    )
}

/// How the primary window is made, whether by `resumed` or before the app
/// is constructed (`startup`).
fn primary_window_attributes(
    title: String,
    icon: Option<winit::window::Icon>,
    app_id: &str,
) -> winit::window::WindowAttributes {
    let window_attributes = Window::default_attributes()
        .with_title(title)
        .with_window_icon(icon)
        // Draggable title bar on compositors with no server-side
        // decorations (e.g. GNOME/mutter); explicit though it's the
        // winit default, so the intent is unmistakable.
        .with_decorations(true)
        .with_visible(false)
        .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
    // Optional profiling placement keeps concurrent benchmark windows from
    // completely occluding one another. Platform window managers may ignore it.
    let window_attributes = match std::env::var("TILO_PROFILE_WINDOW_POSITION")
        .ok()
        .and_then(|value| crate::profiling::profile_window_position(&value))
    {
        Some((x, y)) => window_attributes.with_position(winit::dpi::LogicalPosition::new(x, y)),
        None => window_attributes,
    };
    // Benchmarks can keep their own windows above unrelated desktop apps.
    // This does not change normal application window ordering.
    let window_attributes = if std::env::var("TILO_PROFILE_WINDOW_FRONT").as_deref() == Ok("1") {
        window_attributes.with_window_level(winit::window::WindowLevel::AlwaysOnTop)
    } else {
        window_attributes
    };
    #[cfg(target_os = "linux")]
    let window_attributes = {
        use winit::platform::wayland::WindowAttributesExtWayland;
        window_attributes.with_name(app_id, "")
    };
    // `resolve_app_id` falls back to a generic id rather than none; Linux
    // announces it, other platforms name windows by their process.
    debug_assert!(
        !app_id.is_empty(),
        "the primary window always has an app id"
    );
    window_attributes
}

fn create_window_render(
    window: &Arc<Window>,
    transparent: bool,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<RenderThreadHandle, String> {
    // Drawn by the room's render service: no device in this process.
    if crate::render_thread::remote::requested() {
        match crate::render_thread::remote::spawn(
            window.clone(),
            transparent,
            render_ready_wake(window.id(), proxy.clone()),
        ) {
            Ok(render) => return Ok(render),
            Err(reason) => {
                tracing::warn!(%reason, "no render service; drawing on this process's own device");
            }
        }
    }
    create_window_render_prepared(window, transparent, None, None, proxy)
}

pub(crate) use crate::render_thread::PreparedRenderer;

/// Something the launch made for the primary window before the app was
/// constructed (`startup`), judged against what the app wants.
#[derive(Debug, PartialEq, Eq)]
enum Early<T> {
    /// It was made for exactly this: use it.
    Kept(T),
    /// It was made for another window (an input surface announces its own
    /// app id): never use it, make the real one.
    Discarded,
    /// Nothing was made early.
    Absent,
}

/// `early` made under `made_for`, kept only when that is `wanted`.
fn early_for<K: PartialEq, T>(early: Option<(K, T)>, wanted: &K) -> Early<T> {
    match early {
        Some((made_for, thing)) if made_for == *wanted => Early::Kept(thing),
        Some(_) => Early::Discarded,
        None => Early::Absent,
    }
}

thread_local! {
    /// The event loop the launch built before constructing the app
    /// (`startup`), until `App::run_with_setup` takes it.
    static EARLY_EVENT_LOOP: std::cell::RefCell<Option<EventLoop<UserEvent>>> =
        const { std::cell::RefCell::new(None) };
    /// The primary window made with it and the app id it announces, until
    /// `resumed` takes it.
    static EARLY_WINDOW: std::cell::RefCell<Option<(String, Arc<Window>)>> =
        const { std::cell::RefCell::new(None) };
}

fn take_early_event_loop() -> Option<EventLoop<UserEvent>> {
    EARLY_EVENT_LOOP.with(|slot| slot.borrow_mut().take())
}

fn take_early_window() -> Option<(String, Arc<Window>)> {
    EARLY_WINDOW.with(|slot| slot.borrow_mut().take())
}

/// A configured surface for `window` on a device of `render_cx` that can
/// present to it, sized to the window now.
fn window_surface(
    render_cx: &mut RenderContext,
    window: Arc<Window>,
    transparent: bool,
) -> Result<vello::util::RenderSurface<'static>, String> {
    let size = window.inner_size();
    let mut surface = pollster::block_on(render_cx.create_surface_unconfigured(
        window,
        size.width.max(1),
        size.height.max(1),
        vello::wgpu::PresentMode::AutoVsync,
    ))
    .map_err(|e| format!("could not create a drawing surface: {e}"))?;
    if transparent {
        // `Auto` commonly resolves to Opaque on Wayland even when winit's
        // window was created transparent. Floating surfaces paint
        // premultiplied Vello output, so request the matching compositor mode
        // explicitly; otherwise the transparent gutter becomes a black box.
        surface.config.alpha_mode = vello::wgpu::CompositeAlphaMode::PreMultiplied;
    }
    let device_handle = render_cx
        .devices
        .get(surface.dev_id)
        .ok_or_else(|| "render device index out of range".to_string())?;
    // Flipping to the sRGB suffix here would panic on mismatch with the
    // format `create_surface` already chose. `configure` returns no
    // `Result`, so guard it like the worker's reconfigures do
    // (`render_thread::configure_surface_checked`) — an already-lost
    // surface would otherwise hit wgpu's panicking default handler.
    tilo_startup_metrics::mark("window-surface-validation-scope-start");
    let configure_scope = device_handle
        .device
        .push_error_scope(vello::wgpu::ErrorFilter::Validation);
    tilo_startup_metrics::mark("window-surface-checked-configure-start");
    surface
        .surface
        .configure(&device_handle.device, &surface.config);
    tilo_startup_metrics::mark("window-surface-checked-configure-done");
    if let Some(err) = pollster::block_on(configure_scope.pop()) {
        return Err(format!("could not configure the drawing surface: {err}"));
    }
    tilo_startup_metrics::mark("window-surface-validation-scope-done");
    Ok(surface)
}

fn create_window_render_prepared(
    window: &Arc<Window>,
    transparent: bool,
    prepared: Option<PreparedRenderer>,
    prepared_surface: Option<(WindowId, vello::util::RenderSurface<'static>)>,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<RenderThreadHandle, String> {
    let (render_cx, surface, renderer) =
        prepare_window_render(window, transparent, prepared, prepared_surface)?;
    // After this, every GPU call happens on its render worker.
    let render = crate::render_thread::spawn_render_thread_for_window(
        render_cx,
        surface,
        renderer,
        render_ready_wake(window.id(), proxy),
        Some(window.clone()),
    )?;
    Ok(render)
}

/// What wakes `window`'s handler when its render worker has news.
fn render_ready_wake(
    window_id: WindowId,
    proxy: EventLoopProxy<UserEvent>,
) -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(move || {
        // Avoid synchronous macOS main-thread dispatch while the UI waits on submission.
        let _ = proxy.send_event(UserEvent::RenderReady(window_id));
    })
}

/// The device, surface and renderer `window` renders with: what the launch
/// prepared for it where that fits, made now otherwise.
fn prepare_window_render(
    window: &Arc<Window>,
    transparent: bool,
    prepared: Option<PreparedRenderer>,
    prepared_surface: Option<(WindowId, vello::util::RenderSurface<'static>)>,
) -> Result<(RenderContext, vello::util::RenderSurface<'static>, Renderer), String> {
    tilo_startup_metrics::mark("window-render-context-start");
    let (mut render_cx, prepared_renderer) = match prepared {
        Some(prepared) => (
            prepared.context,
            Some((prepared.device_id, prepared.renderer)),
        ),
        None => (RenderContext::new(), None),
    };
    let surface = match early_for(prepared_surface, &window.id()) {
        Early::Kept(surface) => {
            tilo_startup_metrics::mark("window-surface-prepared");
            surface
        }
        early => {
            if matches!(early, Early::Discarded) {
                // Made for an early window this app did not keep; already
                // dropped, before a surface is made for this one.
                tilo_startup_metrics::mark("window-surface-prepared-discarded");
            }
            window_surface(&mut render_cx, window.clone(), transparent)?
        }
    };
    let device_handle = render_cx
        .devices
        .get(surface.dev_id)
        .ok_or_else(|| "render device index out of range".to_string())?;
    // create_surface checks presentation support and selects another device
    // if necessary. Never use pipelines built for a different device.
    let renderer = match prepared_renderer {
        Some((device_id, renderer)) if device_id == surface.dev_id => renderer,
        Some(_) => {
            // The preparation's work is wasted; the startup file says so.
            tilo_startup_metrics::mark("native-render-prepare-discarded");
            create_native_renderer(device_handle)?
        }
        None => create_native_renderer(device_handle)?,
    };
    Ok((render_cx, surface, renderer))
}

fn create_native_renderer(device_handle: &crate::gpu::DeviceHandle) -> Result<Renderer, String> {
    let plan = NativeRendererPlan::for_device(device_handle);
    let renderer = plan.build(&device_handle.device)?;
    Ok(plan.finish(device_handle, renderer))
}

/// What building vello's renderer for a device takes, decided on the thread
/// that owns the device's handle; [`Self::build`], the compile, needs only
/// the device itself, so it can run on another thread
/// (`startup::renderer_beside_surface`).
pub(super) struct NativeRendererPlan {
    options: RendererOptions,
    deferral: vello::PipelineDeferral,
    pipelines: Option<crate::pipeline_cache::AppPipelineCache>,
    software: bool,
}

impl NativeRendererPlan {
    pub(super) fn for_device(device_handle: &crate::gpu::DeviceHandle) -> Self {
        // Vello's default heuristic uses every available core minus two. On a
        // 16-thread laptop that means fourteen simultaneous Vulkan pipeline
        // compilations for each freshly opened app, competing with the compositor
        // and making the whole desktop appear frozen. Two workers keep startup
        // parallel without turning one window into a system-wide CPU burst.
        let shader_threads = std::env::var("TILO_VELLO_INIT_THREADS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .and_then(std::num::NonZeroUsize::new)
            .unwrap_or(std::num::NonZeroUsize::MIN.saturating_add(1));
        tilo_startup_metrics::mark("window-vello-init-start");
        // This app's own pipelines from its last launch, else the session's.
        let pipelines = crate::pipeline_cache::AppPipelineCache::open(device_handle);
        // The first frame waits for this. Build only what it dispatches; the
        // render worker starts the rest once that frame is presented. On a
        // software adapter the worker rasterizes frames on the CPU and vello draws
        // only backdrop blurs, so none of its pipelines are on the way there.
        let software = crate::render_thread::rasterizes_on_cpu(&device_handle.adapter.get_info());
        let deferral = if software {
            vello::PipelineDeferral::All
        } else {
            vello::PipelineDeferral::Unused
        };
        let options = RendererOptions {
            // Diagnostic/low-end fallback: CPU coarse stages with the same
            // GPU fine raster. Kept opt-in until the benchmark proves it is
            // faster on a device; the default remains the GPU pipeline.
            use_cpu: crate::env::var_os("TILO_UI_VELLO_CPU").is_some(),
            // Every frame below renders with `AaConfig::Area`. Advertising
            // 8x/16x MSAA as well only compiles and retains shader variants
            // this framework never selects, increasing startup and GPU
            // memory pressure on exactly the low-end devices we target.
            antialiasing_support: vello::AaSupport::area_only(),
            num_init_threads: Some(shader_threads),
            // Pipelines already compiled, so a launch does not compile vello
            // again for its own empty shader cache.
            pipeline_cache: pipelines
                .as_ref()
                .map(|pipelines| pipelines.cache().clone()),
        };
        Self {
            options,
            deferral,
            pipelines,
            software,
        }
    }

    /// Compile the renderer's pipelines on `device`: the slow part.
    pub(super) fn build(&self, device: &vello::wgpu::Device) -> Result<Renderer, String> {
        let options = RendererOptions {
            use_cpu: self.options.use_cpu,
            antialiasing_support: self.options.antialiasing_support,
            num_init_threads: self.options.num_init_threads,
            pipeline_cache: self.options.pipeline_cache.clone(),
        };
        let renderer = Renderer::new_deferring(device, options, self.deferral)
            .map_err(|e| format!("could not create the renderer: {e}"))?;
        tilo_startup_metrics::mark("window-vello-init-done");
        Ok(renderer)
    }

    /// Keep what the build used, on the thread that owns the device's handle.
    pub(super) fn finish(
        self,
        device_handle: &crate::gpu::DeviceHandle,
        mut renderer: Renderer,
    ) -> Renderer {
        // Kept once the deferred pipelines are built too.
        if let Some(pipelines) = self.pipelines {
            pipelines.keep_until_built();
        }
        // A frame that draws a backdrop blur renders with vello even on a
        // software adapter. When this app's first frame did last time, build at
        // once, on the init threads and default-path pipelines first, so that
        // frame waits on a build well under way instead of compiling each
        // pipeline it dispatches alone. A first frame the CPU rasterizes waits on
        // nothing, so its app constructs without LLVM compiling beside it; its
        // render worker starts the build with the first frame that draws a
        // backdrop, or after the first present.
        if self.software && crate::pipeline_cache::first_frame_backdrop::expected() {
            renderer.start_deferred_pipelines(
                &device_handle.device,
                crate::pipeline_cache::persist_unsaved,
            );
        }
        #[cfg(all(windows, feature = "dx12-shader-cache"))]
        device_handle.finish_shader_cache();
        renderer
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "native host runner: window render setup probes the GPU on its own thread"
)]
fn begin_window_render_init(
    window: Arc<Window>,
    transparent: bool,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<mpsc::Receiver<Result<RenderThreadHandle, String>>, String> {
    let (result_tx, result_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("floating-render-init".into())
        .spawn(move || {
            let result = create_window_render(&window, transparent, proxy);
            // The surface may have been permanently closed while a slow GPU
            // backend was still probing. In that case, stop the newly-created
            // render worker here so its surface is dropped before `window`.
            if let Err(mpsc::SendError(result)) = result_tx.send(result)
                && let Ok(mut render) = result
            {
                render.shutdown_and_join();
            }
        })
        .map_err(|error| format!("could not start floating renderer setup: {error}"))?;
    Ok(result_rx)
}

/// A frame goes to the render thread, which presents it while this thread
/// reads on: tell the compositor the frame is coming, so one that waits for
/// this window to answer input (an agent's screenshot) waits for the frame
/// rather than taking the window's quiet event loop for an answer.
fn announce_frame(window: &Window) {
    window.announce_frame();
}

impl AppHandler {
    fn handle_routed_window_event(&mut self, window_id: WindowId, event: WindowEvent) {
        let _entry = self.app.enter_wake_scope();
        // A pen's sample is no event of its own: widgets read it while they
        // handle the pointer event it precedes.
        let Some(_pen) = self.app.input.pen.route(&event) else {
            return;
        };
        if render_probe_event(&event) {
            let auxiliary_event = self
                .floating_windows
                .iter()
                .any(|surface| surface.window.id() == window_id)
                || self
                    .audience_presentation
                    .as_ref()
                    .is_some_and(|audience| audience.window.id() == window_id);
            if auxiliary_event
                && self.primary_retry.request_probe()
                && let Some(primary) = &self.window
            {
                primary.request_redraw();
            }

            if self
                .window
                .as_ref()
                .is_some_and(|window| window.id() == window_id)
                && self.primary_retry.request_probe()
                && let Some(window) = &self.window
            {
                window.request_redraw();
            }
            if let Some(surface) = self
                .floating_windows
                .iter_mut()
                .find(|surface| surface.window.id() == window_id)
                && surface
                    .render
                    .as_mut()
                    .is_some_and(|render| render.retry.request_probe())
            {
                surface.window.request_redraw();
            }
            if let Some(audience) = self
                .audience_presentation
                .as_mut()
                .filter(|audience| audience.window.id() == window_id)
                && audience
                    .render
                    .as_mut()
                    .is_some_and(|render| render.retry.request_probe())
            {
                audience.window.request_redraw();
            }
        }

        if self
            .audience_presentation
            .as_ref()
            .is_some_and(|audience| audience.window.id() == window_id)
        {
            self.handle_audience_window_event(event);
            return;
        }
        if let Some(index) = self
            .floating_windows
            .iter()
            .position(|surface| surface.window.id() == window_id)
        {
            self.handle_native_floating_window_event(index, event);
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        if window.id() == window_id {
            self.handle_primary_window_event(&window, event);
        }
    }
    fn visible_auxiliary_needs_shared_root(&self) -> bool {
        self.floating_windows.iter().any(|surface| {
            !surface.lifecycle.suspended
                && surface
                    .render
                    .as_ref()
                    .is_none_or(|render| !render.retry.known_occluded())
        }) || self.audience_presentation.as_ref().is_some_and(|audience| {
            audience
                .render
                .as_ref()
                .is_none_or(|render| !render.retry.known_occluded())
        })
    }
    fn route_window_event(
        &mut self,

        window_id: WindowId,
        event: WindowEvent,
        origin: native_input_isolation::EventOrigin,
    ) -> bool {
        crate::focus_visible::note(&event);
        native_input_isolation::dispatch(
            self.native_input_route_isolation,
            origin,
            event,
            |event| {
                self.handle_routed_window_event(window_id, event);
            },
        )
    }
    /// Called before showing the window on Windows, and from the first
    /// `RedrawRequested` elsewhere so GPU init doesn't starve Wayland's
    /// initial focus events. `Err` is returned, not panicked, since every step here can
    /// fail when the compositor exits mid-open.
    fn init_render(&mut self, window: &Arc<Window>) -> Result<(), String> {
        if let Some(retained) = self.suspended_renderer.take()
            && !retained.device_lost()
        {
            match create_window_render_prepared(
                window,
                false,
                Some(retained),
                None,
                self.a11y_proxy.clone(),
            ) {
                Ok(render) => {
                    self.render = Some(render);
                    return Ok(());
                }
                Err(error) => {
                    tracing::warn!(%error, "could not render with the renderer kept over suspend");
                }
            }
        }
        // On a Wayland display the preparation already made this window's
        // device, pipelines and surface off the main thread, so the render
        // worker starts at once and finishes waiting for them itself: the
        // first frame's sync, layout and draw run here meanwhile instead of
        // after the wait.
        #[cfg(any(windows, target_os = "linux"))]
        {
            // Drawn by the room's render service: the launch prepared its
            // connection, not a device (`startup::prepare_native_renderer`).
            if crate::render_thread::remote::requested() {
                self.app.startup_renderer = None;
                self.render = Some(create_window_render(
                    window,
                    false,
                    self.a11y_proxy.clone(),
                )?);
                return Ok(());
            }
            if let Some(preparation) = self
                .app
                .startup_renderer
                .take_if(|preparation| preparation.made_early_window())
            {
                let worker_window = window.clone();
                self.render = Some(crate::render_thread::spawn_render_thread_deferred(
                    move || {
                        let startup =
                            tilo_startup_metrics::span("native-render-prepare-wait", || {
                                preparation.wait()
                            });
                        match startup {
                            Ok(Ok(startup)) => prepare_window_render(
                                &worker_window,
                                false,
                                Some(startup.prepared),
                                startup.surface,
                            ),
                            Ok(Err(error)) => {
                                tracing::warn!(%error, "renderer preparation failed; selecting device for the actual surface");
                                prepare_window_render(&worker_window, false, None, None)
                            }
                            Err(error) => {
                                tracing::warn!(%error, "renderer preparation disconnected; selecting device for the actual surface");
                                prepare_window_render(&worker_window, false, None, None)
                            }
                        }
                    },
                    render_ready_wake(window.id(), self.a11y_proxy.clone()),
                    Some(window.clone()),
                )?);
                return Ok(());
            }
            if let Some(preparation) = self.app.startup_renderer.take() {
                // How long the first redraw still waits on the preparation.
                match tilo_startup_metrics::span("native-render-prepare-wait", || {
                    preparation.wait()
                }) {
                    Ok(Ok(startup)) => {
                        self.render = Some(create_window_render_prepared(
                            window,
                            false,
                            Some(startup.prepared),
                            startup.surface,
                            self.a11y_proxy.clone(),
                        )?);
                        return Ok(());
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "renderer preparation failed; selecting device for the actual surface")
                    }
                    Err(error) => {
                        tracing::warn!(%error, "renderer preparation disconnected; selecting device for the actual surface")
                    }
                }
            }
        }
        self.render = Some(create_window_render(
            window,
            false,
            self.a11y_proxy.clone(),
        )?);
        Ok(())
    }

    /// Reconcile the root's optional audience projection with a *separate*
    /// native toplevel. A missing monitor list, a one-output session, or an
    /// invalid target all leave this `None`; the root's normal fullscreen
    /// intent then supplies the single-output fallback. The chosen output
    /// may be the one the presenter window currently shows on: Tilo pairs
    /// the `.audience` surface and moves the presenter view to the other
    /// screen.
    fn sync_audience_presentation(
        &mut self,
        event_loop: &ActiveEventLoop,
        presenter: &Arc<Window>,
    ) {
        let _entry = self.app.enter_wake_scope();
        let request = self.app.root_widget.audience_presentation_request();
        let monitors: Vec<winit::monitor::MonitorHandle> = presenter.available_monitors().collect();
        let primary_monitor = presenter.primary_monitor();
        let local_monitor = presenter
            .current_monitor()
            .or_else(|| primary_monitor.clone());
        let presenter_index = local_monitor.as_ref().and_then(|current| {
            monitors
                .iter()
                .position(|monitor| monitor == current)
                .and_then(|index| u32::try_from(index).ok())
        });
        let infos: Vec<crate::fullscreen::MonitorInfo> = monitors
            .iter()
            .enumerate()
            .map(|(index, monitor)| {
                super::monitor_info(index, monitor, primary_monitor.as_ref() == Some(monitor))
            })
            .collect();
        let target = request.and_then(|request| {
            crate::fullscreen::select_audience_output(&infos, request.target, presenter_index)
        });

        if self.audience_presentation.as_ref().is_some_and(|audience| {
            audience_monitor_still_selected(
                target.and_then(|index| monitors.get(index as usize)),
                Some(&audience.monitor),
            )
        }) {
            return;
        }
        self.close_audience_presentation();
        let Some(index) = target else {
            return;
        };
        let Some(monitor) = monitors.get(index as usize).cloned() else {
            return;
        };

        let attributes = Window::default_attributes()
            .with_title("Audience presentation")
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(false);
        #[cfg(target_os = "linux")]
        let attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            let package_id = std::env::var("TILO_PACKAGE_ID").ok();
            let app_id = resolve_app_id(
                self.app.window.app_id.as_deref().or(package_id.as_deref()),
                std::env::current_exe()
                    .ok()
                    .and_then(|path| {
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                    })
                    .as_deref(),
            );
            // A role-like suffix keeps a compositor from mistaking the
            // audience output for a duplicate editor surface.
            attributes.with_name(format!("{app_id}.audience"), "")
        };
        let Ok(window) = event_loop.create_window(attributes) else {
            tracing::warn!(
                "could not create separate audience presentation window; using single-output fallback"
            );
            return;
        };
        let window = Arc::new(window);
        window.set_fullscreen(Some(Fullscreen::Borderless(Some(monitor.clone()))));
        window.set_visible(true);
        window.request_redraw();
        self.audience_presentation = Some(AudiencePresentationWindow {
            monitor,
            window,
            render: None,
            render_init: None,
        });
        self.app.root_widget.on_audience_presentation_changed(true);
        self.app.layout.layout_dirty = true;
        presenter.request_redraw();
    }

    /// Drop the render worker before the Window Arc, as required by wgpu's
    /// surface lifetime. This is deliberately idempotent: hotplug, explicit
    /// stop, and process teardown can race through the same state transition.
    fn close_audience_presentation(&mut self) {
        let _entry = self.app.enter_wake_scope();
        let Some(mut audience) = self.audience_presentation.take() else {
            return;
        };
        audience.window.set_visible(false);
        if let Some(mut render) = audience.render.take() {
            render.shutdown_and_join();
        }
        self.app.root_widget.on_audience_presentation_changed(false);
        self.app.layout.layout_dirty = true;
    }

    /// Paint one audience frame without running a second widget layout or
    /// synchronising a second document. The root is responsible for a pure
    /// audience projection of the same model used by the presenter surface.
    fn handle_audience_redraw(&mut self) -> bool {
        let _entry = self.app.enter_wake_scope();
        let Some(audience) = self.audience_presentation.as_mut() else {
            return false;
        };
        if audience.render.is_none() {
            if audience.render_init.is_none() {
                match begin_window_render_init(
                    audience.window.clone(),
                    false,
                    self.a11y_proxy.clone(),
                ) {
                    Ok(init) => audience.render_init = Some(init),
                    Err(error) => tracing::warn!(%error, "could not start audience renderer"),
                }
            }
            let Some(init) = audience.render_init.as_ref() else {
                return true;
            };
            match init.try_recv() {
                Ok(Ok(render)) => {
                    audience.render = Some(render);
                    audience.render_init = None;
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "could not initialise audience renderer");
                    audience.render_init = None;
                    return false;
                }
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => {
                    audience.render_init = None;
                    return false;
                }
            }
        }
        let Some(render) = audience.render.as_mut() else {
            return false;
        };
        while let Ok(outcome) = render.frame_complete.try_recv() {
            self.app
                .capture_auxiliary_completed("audience", outcome == FrameOutcome::Presented);
            match outcome {
                FrameOutcome::Presented
                | FrameOutcome::Reconfigured
                | FrameOutcome::Skipped
                | FrameOutcome::Occluded => {
                    render.retry.observe(outcome, crate::time::now());
                }
                FrameOutcome::SurfaceGone => return false,
            }
        }
        match crate::render_thread::prepare_frame(
            &mut render.retry,
            &mut render.buffers,
            crate::time::now(),
        ) {
            Ok(true) => {}
            Ok(false) => return true,
            Err(_) => return false,
        }
        let (mut scene, mut damage_regions) = match render.buffers.try_recv() {
            Ok(scene) => scene,
            Err(mpsc::TryRecvError::Empty) => {
                return true;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                tracing::warn!("audience render worker exited");
                return false;
            }
        };
        damage_regions.clear();
        let extent =
            render.frame_extent(audience.window.inner_size(), audience.window.scale_factor());
        let scale = extent.scale;
        let rect = vello::kurbo::Rect::new(
            0.0,
            0.0,
            f64::from(extent.width) / scale,
            f64::from(extent.height) / scale,
        );
        let secrets = crate::secret_paint::SecretPaint::begin();
        let drawn = self.app.root_widget.draw_audience_presentation(
            &mut scene,
            Affine::scale(scale),
            &mut self.app.text_renderer,
            &self.app.theme,
            rect,
        );
        self.app
            .capture_auxiliary_drawn("audience", secrets.finish());
        if !self.app.capture_frame_ready() {
            scene.reset();
            render.buffers.unprepare((scene, damage_regions));
            return true;
        }
        if !drawn {
            return false;
        }
        announce_frame(&audience.window);
        if render
            .render_tx
            .send(RenderRequest::Frame {
                cpu_frame_id: None,
                on_present: None,
                scene,
                width: extent.width,
                height: extent.height,
                base_color: self.app.theme.tokens.colors.surfaces.content,
                damage_regions,
                scroll_copies: Vec::new(),
                full_redraw: true,
                backdrop: crate::backdrop::BackdropFrame::default(),
            })
            .is_err()
        {
            tracing::warn!(
                "audience render worker stopped; returning to the single-output fallback"
            );
            return false;
        }
        self.app.capture_auxiliary_submitted("audience");
        true
    }

    /// A touch on the field that has the keyboard, which Android answers by
    /// settling the composition and bringing a dismissed keyboard back.
    #[cfg(target_os = "android")]
    fn touches_focused_field(
        &self,
        window_id: WindowId,
        location: winit::dpi::PhysicalPosition<f64>,
    ) -> bool {
        let Some(window) = self
            .window
            .as_ref()
            .filter(|window| window.id() == window_id)
        else {
            return false;
        };
        let Some(field) = self.app.focused_node else {
            return false;
        };
        let scale = window.scale_factor();
        let point = Point::new(location.x / scale, location.y / scale);
        self.app
            .inspect_node(field, |widget| widget.get_rect().contains(point))
            .unwrap_or(false)
    }

    fn set_floating_ime_continuation(&mut self, next: Option<String>) {
        if self.floating_ime_continuation == next {
            return;
        }
        self.floating_ime_continuation = next;
        let id = self.floating_ime_continuation.clone();
        self.app
            .root_widget
            .on_floating_surface_ime_source_changed(id.as_deref());
    }

    fn sync_ime(&mut self, window: &Window) {
        if self.primary_preedit_active {
            self.app.flush_floating_present_requests_before_ime();
        }
        let _entry = self.app.enter_wake_scope();
        self.private_input.service(
            &self.app,
            window,
            !self
                .floating_windows
                .iter()
                .any(|surface| surface.lifecycle.focused),
        );
        // Return keeps the real composer on its exact suspended child until
        // the input method finishes. The primary redraw has moved the editor's
        // caret, so update that still-active protocol without waiting for
        // another keyboard/IME event on the hidden child.
        if let Some(id) = self.floating_ime_return_continuation.as_deref()
            // The initial Suspended message precedes primary relayout. Its
            // gate becomes Some only after that returned frame is submitted.
            && self
                .floating_destination_present_frames
                .get(id)
                .is_none_or(Option::is_some)
            && let Some(surface) = self
                .floating_windows
                .iter_mut()
                .find(|surface| surface.id == id && surface.lifecycle.suspended && surface.preedit_active)
        {
            let next = primary_ime_state_for_held_child(
                self.app.ime_window_state(),
                surface.coordinate_offset,
                surface.desktop_rect.origin().to_vec2(),
            );
            let next = retained_return_ime_state(next, surface.ime_state, true);
            apply_native_ime(&surface.window, next, &mut surface.ime_state);
        }
        let continued = self.floating_ime_continuation.as_deref().and_then(|id| {
            let offset = *self.app.floating.coordinate_offsets.get(id)?;
            primary_floating_ime_state(self.app.root_widget.as_ref(), id, offset)
        });
        if self.floating_ime_continuation.is_some() && continued.is_none() {
            // The exact editor or host-provided coordinate binding retired.
            // Never route subsequent source events by a stale surface id.
            self.set_floating_ime_continuation(None);
        }
        let pending =
            pending_floating_ime_state(&self.app, self.primary_preedit_active, self.ime_state);
        let next = if continued.is_some() {
            continued
        } else if pending.is_some() {
            // Present was accepted on the live host connection, but Prepare
            // has not arrived yet. Keep the real source protocol's existing
            // caret while the exact retained editor still exports IME intent.
            // Destination routing starts only after the host authorises it.
            pending
        } else if self
            .floating_windows
            .iter()
            .any(|surface| surface.lifecycle.focused)
        {
            None
        } else {
            self.app
                .ime_window_state()
                .filter(|_| self.private_input.standard_input_open(&self.app))
                .map(NativeImeWindowState::from)
        };
        #[cfg(target_os = "android")]
        if super::android_text_input::bridge::installed() {
            let field = next.and(self.app.focused_node);
            let restart = self.text_input.refocus(field);
            let touched = std::mem::take(&mut self.text_input_touched);
            if next != self.ime_state || restart || touched {
                let surrounding = field.and_then(|node| {
                    self.app
                        .inspect_node(node, crate::ime::ImeSurroundingText::from_widget)
                        .ok()
                        .flatten()
                });
                super::android_text_input::bridge::update(
                    next.map(ImeWindowState::from).as_ref(),
                    surrounding.as_ref(),
                    window.scale_factor(),
                    restart,
                );
                self.ime_state = next;
            }
            // After the update, so the fresh connection starts from the
            // caret the touch just placed.
            if touched {
                super::android_text_input::bridge::touched();
            }
            return;
        }
        if next == self.ime_state {
            return;
        }
        if let Some(intent) = next {
            // Apply purpose before enabling: winit's Wayland backend
            // commits the enable immediately, so the first activation
            // must already carry the right content type. Cursor geometry
            // follows enable because winit intentionally ignores cursor
            // updates while IME is disabled.
            apply_native_ime_content(window, intent);
            window.set_ime_allowed(true);
            window.set_ime_cursor_area(
                winit::dpi::LogicalPosition::new(intent.x, intent.y),
                winit::dpi::LogicalSize::new(intent.width, intent.height),
            );
        } else {
            window.set_ime_allowed(false);
        }
        self.ime_state = next;
    }

    fn create_native_floating_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: String,
        token: String,
        rect: KurboRect,
        coordinate_offset: Vec2,
    ) -> Result<(), String> {
        let _entry = self.app.enter_wake_scope();
        if let Some(existing) = self
            .floating_windows
            .iter_mut()
            .find(|surface| surface.id == id)
        {
            if existing.token != token {
                return Err("shell changed the token of a live floating surface".into());
            }
            if existing.desktop_rect.size() != rect.size() {
                resize_native_floating_window(&existing.window, rect);
            }
            existing.desktop_rect = rect;
            existing.coordinate_offset = coordinate_offset;
            existing.prepared_at = crate::time::now();
            existing.lifecycle.resume(
                self.app.root_widget.as_mut(),
                &existing.id,
                existing.window.has_focus(),
            );
            existing.window.set_visible(true);
            existing.window.request_redraw();
            return Ok(());
        }
        let logical_size = native_floating_logical_size(rect);
        let attributes = Window::default_attributes()
            .with_title(format!("Floating · {id}"))
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_visible(false)
            .with_inner_size(logical_size);
        #[cfg(target_os = "linux")]
        let attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attributes.with_name(
                format!(
                    "{}{}",
                    crate::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX,
                    token
                ),
                "",
            )
        };
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|error| format!("create floating surface window: {error}"))?,
        );
        let a11y_adapter =
            create_accessibility_adapter(event_loop, &window, self.a11y_proxy.clone());
        window.set_visible(true);
        window.request_redraw();
        self.floating_windows.push(NativeFloatingWindow {
            id,
            token,
            desktop_rect: rect,
            coordinate_offset,
            window,
            render: None,
            render_init: None,
            prepared_at: crate::time::now(),
            lifecycle: NativeFloatingSurfaceLifecycle::default(),
            a11y_adapter,
            ime_state: None,
            preedit_active: false,
            preedit_clear_pending: false,
        });
        Ok(())
    }

    fn suspend_native_floating_window(&mut self, id: &str) -> bool {
        let _entry = self.app.enter_wake_scope();
        let preserve_ime = self.floating_ime_return_continuation.as_deref() == Some(id);
        let Some(surface) = self
            .floating_windows
            .iter_mut()
            .find(|surface| surface.id == id)
        else {
            return false;
        };
        surface.window.set_visible(false);
        if !preserve_ime {
            apply_native_ime(&surface.window, None, &mut surface.ime_state);
        }
        surface.lifecycle.suspend(self.app.root_widget.as_mut(), id);
        true
    }

    fn close_native_floating_window(&mut self, id: &str) -> bool {
        let _entry = self.app.enter_wake_scope();
        let Some(index) = self
            .floating_windows
            .iter()
            .position(|surface| surface.id == id)
        else {
            return false;
        };
        let mut surface = self.floating_windows.remove(index);
        surface.window.set_visible(false);
        apply_native_ime(&surface.window, None, &mut surface.ime_state);
        self.app
            .root_widget
            .on_floating_surface_focus_changed(Some(id), false);
        if let Some(mut render) = surface.render.take() {
            render.shutdown_and_join();
        }
        true
    }

    fn apply_floating_host_messages(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let _entry = self.app.enter_wake_scope();
        let mut changed = false;
        for message in self.app.take_floating_host_messages() {
            match message {
                crate::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
                    id,
                    token,
                    rect,
                    content_rect: _,
                    transfer_covered,
                    coordinate_offset,
                } => match crate::floating_surface::protocol_rect(rect) {
                    Ok(rect) => {
                        if transfer_covered {
                            self.floating_source_clear_frames.insert(id.clone(), None);
                            if let Some(primary) = self.window.as_ref() {
                                primary.request_redraw();
                            }
                        }
                        let continue_ime = self.primary_preedit_active
                            && primary_floating_ime_state(
                                self.app.root_widget.as_ref(),
                                &id,
                                Vec2::new(coordinate_offset[0], coordinate_offset[1]),
                            )
                            .is_some();
                        match self.create_native_floating_window(
                            event_loop,
                            id.clone(),
                            token,
                            rect,
                            Vec2::new(coordinate_offset[0], coordinate_offset[1]),
                        ) {
                            Ok(()) => {
                                if continue_ime {
                                    self.set_floating_ime_continuation(Some(id));
                                }
                                changed = true;
                            }
                            Err(error) => {
                                tracing::warn!(%error, "could not prepare native floating surface");
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "shell sent invalid floating surface rect");
                    }
                },
                crate::floating_surface::FloatingSurfaceHostMessage::Adopted { id, rect } => {
                    if crate::floating_surface::protocol_rect(rect).is_ok()
                        && let Some(surface) = self
                            .floating_windows
                            .iter_mut()
                            .find(|surface| surface.id == id)
                    {
                        surface.lifecycle.adopted = true;
                        surface.window.request_redraw();
                        changed = true;
                    }
                }
                crate::floating_surface::FloatingSurfaceHostMessage::Reattach { id }
                | crate::floating_surface::FloatingSurfaceHostMessage::Closed { id } => {
                    if self.floating_ime_continuation.as_deref() == Some(id.as_str()) {
                        self.set_floating_ime_continuation(None);
                    }
                    if self.floating_ime_return_continuation.as_deref() == Some(id.as_str()) {
                        self.floating_ime_return_continuation = None;
                    }
                    self.floating_source_clear_frames.remove(&id);
                    self.floating_destination_present_frames.remove(&id);
                    changed |= self.close_native_floating_window(&id);
                }
                crate::floating_surface::FloatingSurfaceHostMessage::Suspended { id } => {
                    if self.floating_ime_continuation.as_deref() == Some(id.as_str()) {
                        self.set_floating_ime_continuation(None);
                    }
                    if self
                        .floating_windows
                        .iter()
                        .any(|surface| surface.id == id && surface.preedit_active)
                    {
                        self.floating_ime_return_continuation = Some(id.clone());
                    }
                    self.floating_source_clear_frames.remove(&id);
                    self.floating_destination_present_frames
                        .insert(id.clone(), None);
                    changed |= self.suspend_native_floating_window(&id);
                    if let Some(primary) = self.window.as_ref() {
                        primary.request_redraw();
                    }
                }
                crate::floating_surface::FloatingSurfaceHostMessage::CaptureProtectionApplied {
                    ..
                } => {
                    for surface in &self.floating_windows {
                        surface.window.request_redraw();
                    }
                    if let Some(audience) = &self.audience_presentation {
                        audience.window.request_redraw();
                    }
                    changed = true;
                }
                crate::floating_surface::FloatingSurfaceHostMessage::DeviceShellOperation {
                    ..
                }
                | crate::floating_surface::FloatingSurfaceHostMessage::DesktopBounds { .. }
                | crate::floating_surface::FloatingSurfaceHostMessage::Appearance { .. }
                | crate::floating_surface::FloatingSurfaceHostMessage::PaintKeyframe
                | crate::floating_surface::FloatingSurfaceHostMessage::PaintStop
                | crate::floating_surface::FloatingSurfaceHostMessage::ShareSelection
                | crate::floating_surface::FloatingSurfaceHostMessage::PointAt { .. }
                | crate::floating_surface::FloatingSurfaceHostMessage::ToolbarEvent { .. }
                | crate::floating_surface::FloatingSurfaceHostMessage::HistoryDescriptions {
                    ..
                }
                | crate::floating_surface::FloatingSurfaceHostMessage::DeviceDisplayMoveResult {
                    ..
                }
                | crate::floating_surface::FloatingSurfaceHostMessage::HostShellAllowed {
                    ..
                }
                // Never delivered: the client's `pump` ends on it.
                | crate::floating_surface::FloatingSurfaceHostMessage::VersionMismatch {
                    ..
                } => {}
            }
        }
        changed
    }

    /// A prepared surface that never commits must not strand its app-owned
    /// content half outside the primary window forever. Keep polling only
    /// while the first buffer is pending, then ask the shell to close the
    /// abandoned capability; `Closed` drives the ordinary animated return.
    fn recover_stalled_floating_windows(&mut self) -> bool {
        // A frozen compositor-side transfer frame keeps the surface visually
        // continuous while a cold GPU backend prepares. This deadline is now
        // a genuine failure bound, not a UX timer: ordinary adoption replaces
        // the proxy on its first committed buffer.
        // A cold secondary WGPU surface can spend several seconds enumerating
        // fallbacks on software/headless systems. The compositor's transfer
        // proxy is already covering every pixel, so returning at four seconds
        // caused a needless visible snap just before a viable backend could
        // finish. Ten seconds remains a bounded failure path without turning
        // ordinary slow device discovery into UX churn.
        const FIRST_BUFFER_DEADLINE: Duration = Duration::from_secs(10);
        let _entry = self.app.enter_wake_scope();
        let stalled = self
            .floating_windows
            .iter()
            .filter(|surface| {
                !surface.lifecycle.suspended
                    && !surface.lifecycle.adopted
                    && surface.prepared_at.elapsed() >= FIRST_BUFFER_DEADLINE
            })
            .map(|surface| surface.id.clone())
            .collect::<Vec<_>>();
        if stalled.is_empty() {
            return false;
        }
        for id in stalled {
            tracing::warn!(%id, "floating surface missed its first-buffer deadline; returning it to the app");
            if let Some(client) = self.app.floating.client.as_mut() {
                let _ = client.send(
                    &crate::floating_surface::FloatingSurfaceClientMessage::Dismiss {
                        id: id.clone(),
                    },
                );
            }
            self.close_native_floating_window(&id);
        }
        true
    }

    fn sync_native_floating_geometry(&mut self) -> bool {
        let _entry = self.app.enter_wake_scope();
        let presentations = self.app.root_widget.floating_surface_presentations();
        let mut changed = false;
        for surface in &mut self.floating_windows {
            if let Some(presentation) = presentations
                .iter()
                .find(|presentation| presentation.id == surface.id)
                && presentation.window_rect != surface.desktop_rect
            {
                if presentation.window_rect.size() != surface.desktop_rect.size() {
                    // App-owned live activities can change silhouette while
                    // detached: a call gains participant rows, navigation
                    // reveals another instruction, or accessibility text
                    // grows. Resize the real transparent buffer with the
                    // shared presentation rect so its soft chrome is never
                    // clipped inside the old native allocation.
                    resize_native_floating_window(&surface.window, presentation.window_rect);
                }
                surface.desktop_rect = presentation.window_rect;
                surface.window.request_redraw();
                changed = true;
            }
        }
        changed
    }

    fn handle_floating_redraw(&mut self, index: usize) -> bool {
        let _entry = self.app.enter_wake_scope();
        let (app, surfaces) = (&mut self.app, &mut self.floating_windows);
        let Some(surface) = surfaces.get_mut(index) else {
            return false;
        };
        let capture_id = format!("floating:{}", surface.id);
        if surface.lifecycle.suspended {
            return true;
        }
        if surface.render.is_none() {
            if surface.render_init.is_none() {
                match begin_window_render_init(
                    surface.window.clone(),
                    true,
                    self.a11y_proxy.clone(),
                ) {
                    Ok(init) => surface.render_init = Some(init),
                    Err(error) => {
                        tracing::warn!(%error, id = %surface.id, "floating surface renderer failed");
                        return false;
                    }
                }
            }
            let Some(init) = surface.render_init.as_ref() else {
                return false;
            };
            match init.try_recv() {
                Ok(Ok(render)) => {
                    surface.render = Some(render);
                    surface.render_init = None;
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, id = %surface.id, "floating surface renderer failed");
                    surface.render_init = None;
                    return false;
                }
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => {
                    tracing::warn!(id = %surface.id, "floating surface renderer setup stopped");
                    surface.render_init = None;
                    return false;
                }
            }
        }
        let Some(render) = surface.render.as_mut() else {
            return false;
        };
        while let Ok(outcome) = render.frame_complete.try_recv() {
            app.capture_auxiliary_completed(&capture_id, outcome == FrameOutcome::Presented);
            match outcome {
                FrameOutcome::Presented
                | FrameOutcome::Reconfigured
                | FrameOutcome::Skipped
                | FrameOutcome::Occluded => {
                    render.retry.observe(outcome, crate::time::now());
                }
                FrameOutcome::SurfaceGone => return false,
            }
        }
        match crate::render_thread::prepare_frame(
            &mut render.retry,
            &mut render.buffers,
            crate::time::now(),
        ) {
            Ok(true) => {}
            Ok(false) => return true,
            Err(_) => return false,
        }
        let (mut scene, mut damage_regions) = match render.buffers.try_recv() {
            Ok(buffers) => buffers,
            Err(mpsc::TryRecvError::Empty) => return true,
            Err(mpsc::TryRecvError::Disconnected) => return false,
        };
        damage_regions.clear();
        // Every pooled scene is either newly seeded or reset by the worker
        // before recycling. Resetting it again here only rewrote all stream
        // lengths on every frame.
        let scale = surface.window.scale_factor();
        let extent = render.frame_extent(surface.window.inner_size(), scale);
        let transform = crate::scale::composite_transform(extent.scale)
            .pre_translate((-surface.desktop_rect.x0, -surface.desktop_rect.y0).into());
        let presentation = app
            .root_widget
            .floating_surface_presentations()
            .into_iter()
            .find(|presentation| presentation.id == surface.id);
        let Some(window_clip) = presentation.and_then(|presentation| {
            native_floating_window_clip(surface.desktop_rect, presentation.window_rect)
        }) else {
            tracing::warn!(id = %surface.id, "floating surface has no valid content region");
            return false;
        };
        let secrets = crate::secret_paint::SecretPaint::begin();
        use vello::peniko::{BlendMode, Fill};
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &window_clip,
        );
        let drawn = app.root_widget.draw_floating_surface(
            &surface.id,
            &mut scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
        scene.pop_layer();
        if !drawn {
            tracing::warn!(id = %surface.id, "floating surface content is no longer available");
            return false;
        }
        if let Some(adapter) = surface.a11y_adapter.as_mut() {
            adapter.update_if_active(|| {
                let access = app.root_widget.floating_surface_accessibility(&surface.id);
                crate::accesskit_bridge::native_tree_update(
                    crate::accesskit_bridge::surface_tree_update(
                        access.as_ref(),
                        surface.desktop_rect.origin(),
                        scale,
                    ),
                )
            });
        }
        let next = secondary_ime_state(
            app.root_widget.as_ref(),
            &surface.id,
            surface.desktop_rect,
            surface.lifecycle.focused,
        );
        apply_native_ime(&surface.window, next, &mut surface.ime_state);
        app.capture_auxiliary_drawn(&capture_id, secrets.finish());
        if !app.capture_frame_ready() {
            scene.reset();
            render.buffers.unprepare((scene, damage_regions));
            return true;
        }
        announce_frame(&surface.window);
        if render
            .render_tx
            .send(RenderRequest::Frame {
                cpu_frame_id: None,
                on_present: None,
                scene,
                width: extent.width,
                height: extent.height,
                base_color: Color::TRANSPARENT,
                damage_regions,
                scroll_copies: Vec::new(),
                full_redraw: true,
                // The compositor puts this window over the desktop; nothing
                // under its surfaces is in this scene to blur.
                backdrop: crate::backdrop::BackdropFrame::default(),
            })
            .is_err()
        {
            return false;
        }
        app.capture_auxiliary_submitted(&capture_id);
        true
    }

    fn dispatch_floating_event(
        &mut self,
        window: &Arc<Window>,
        desktop_rect: KurboRect,
        event: WindowEvent,
    ) {
        let _entry = self.app.enter_wake_scope();
        let scale = window.scale_factor();
        let event = match event {
            WindowEvent::CursorMoved {
                device_id,
                position,
            } => WindowEvent::CursorMoved {
                device_id,
                position: winit::dpi::PhysicalPosition::new(
                    position.x + desktop_rect.x0 * scale,
                    position.y + desktop_rect.y0 * scale,
                ),
            },
            WindowEvent::Touch(mut touch) => {
                touch.location = winit::dpi::PhysicalPosition::new(
                    touch.location.x + desktop_rect.x0 * scale,
                    touch.location.y + desktop_rect.y0 * scale,
                );
                WindowEvent::Touch(touch)
            }
            other => other,
        };
        let app = &mut self.app;
        let Some(root_node) = app
            .history_layout
            .map(|layout| layout.app)
            .or(app.layout.root_node)
        else {
            return;
        };
        let mut handled = false;
        let mut focus_target = None;
        app.mark_semantic_tree_stale();
        app.track_pointer_buttons(&event);
        app.refresh_controls_before_input(&event);
        let resources_before = app.resource_input_mark(&event);
        let capture = app.begin_local_input_event(&event);
        app.root_widget.handle_event(
            &event,
            &mut crate::widget::EventCtx {
                scale_factor: scale,
                modifiers: &app.input.modifiers,
                state: crate::widget::Edits::lend(&mut app.state.doc),
                path: &app.state.root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: root_node,
                taffy: &app.layout.taffy,
            },
        );
        drop(capture);
        app.drain_local_input_capture();
        // `handle_event` may change presentation-only resource declarations
        // without touching the CRDT (selected tab, open media, local mode);
        // only bare hover that changed nothing keeps the published tree.
        app.invalidate_resources_after_input(
            &event,
            handled || focus_target.is_some(),
            &resources_before,
            false,
        );
        if let Some(id) = focus_target {
            app.set_focused_node(Some(id));
        }
        if handled {
            app.commit_action();
            app.needs_sync = true;
            app.layout.layout_dirty |= app.root_widget.is_layout_dirty();
            if let Some(primary) = self.window.as_ref() {
                primary.request_redraw();
            }
            for surface in &self.floating_windows {
                surface.window.request_redraw();
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the frame handshake shares render state, timing locals, and early exits"
    )]
    /// Split out of `window_event`'s `RedrawRequested` arm (`clippy::too_many_lines`).
    /// Body unchanged from the original inline arm — see `window_event` for the
    /// event-loop plumbing this is called from. The layout pass, draw pass,
    /// legacy cursor drawing, and debug-outline drawing are further split
    /// into [`run_layout_pass`], [`draw_frame_content`], [`draw_legacy_cursors`]
    /// and [`draw_debug_outlines`] respectively; what remains is a single
    /// frame's render-thread handshake (acquire scene, draw, send, log) whose
    /// phases share `render`/`app`/`scene`/timing locals and several early
    /// `return`s — further fragmenting it risks changing frame-timing or
    /// error-path behaviour with no clippy/cargo loop available here to
    /// verify against.
    #[allow(clippy::too_many_lines, reason = "see doc comment above")]
    fn handle_redraw_requested(&mut self, window: &Arc<Window>) {
        let native_redraw_entry = self
            .native_input_origin
            .map(|origin| (origin, crate::time::now()));
        // Capture only the initial frame attempts, not steady-state redraws.
        // An early return leaves its final marker absent rather than inventing
        // a completed stage. A later successful submission ends capture.
        let first_primary_frame = self.primary_frames_sent == 0;
        let mark_first_frame = |name| {
            if first_primary_frame {
                tilo_startup_metrics::mark(name);
            }
        };
        mark_first_frame("first-frame-redraw-start");
        let _entry = self.app.enter_wake_scope();
        if self.surface_suspended {
            return;
        }
        if first_primary_frame {
            // The frame budget local reads get is one refresh of the display
            // the window opens on.
            self.animation_schedule.set_refresh_millihertz(
                window
                    .current_monitor()
                    .and_then(|monitor| monitor.refresh_rate_millihertz()),
            );
            crate::arriving::set_frame_budget(self.animation_schedule.interval());
        }
        if self.render.is_none() {
            tilo_startup_metrics::mark("first-redraw-init-render-start");
            if let Err(reason) = self.init_render(window) {
                // Retrying every redraw would only spin.
                crate::diag!("tilo: {reason}");
                stop_abnormally(
                    &self.closing,
                    format!("its renderer could not start: {reason}"),
                );
                return;
            }
            tilo_startup_metrics::mark("first-redraw-init-render-done");
        }
        let visible_sibling = self.visible_auxiliary_needs_shared_root();
        if visible_sibling {
            self.primary_retry.keep_shared_cadence(crate::time::now());
        }
        let Some(render) = self.render.as_mut() else {
            return;
        };
        // A `Reconfigured` or `Skipped` previous frame re-requests a
        // redraw so the user doesn't see a stale image.
        let mut needs_followup_redraw = false;
        let mut surface_gone = false;
        while let Ok(outcome) = render.frame_complete.try_recv() {
            self.primary_frames_completed = self.primary_frames_completed.saturating_add(1);
            if let Some(markers) = self.app.floating.complete_action_marker_frame(
                self.primary_frames_completed,
                outcome == FrameOutcome::Presented,
            ) && let Some(client) = self.app.floating.client.as_mut()
            {
                let _ = client.send(
                    &crate::floating_surface::FloatingSurfaceClientMessage::ActionMarkers {
                        markers,
                    },
                );
            }
            match outcome {
                FrameOutcome::Presented => {
                    self.last_primary_frame_presented = self.primary_frames_completed;
                    self.app
                        .capture_frame_presented(self.primary_frames_completed);
                    self.primary_retry.observe_for_shared_root(
                        outcome,
                        crate::time::now(),
                        visible_sibling,
                    );
                }
                FrameOutcome::Reconfigured | FrameOutcome::Skipped | FrameOutcome::Occluded => {
                    self.primary_retry.observe_for_shared_root(
                        outcome,
                        crate::time::now(),
                        visible_sibling,
                    );
                }
                FrameOutcome::SurfaceGone => {
                    surface_gone = true;
                }
            }
        }
        let cleared = completed_primary_frame_gates(
            &self.floating_source_clear_frames,
            self.last_primary_frame_presented,
        );
        for id in cleared {
            let sent = self.app.floating.client.as_mut().is_some_and(|client| {
                client
                    .send(
                        &crate::floating_surface::FloatingSurfaceClientMessage::SourceCleared {
                            id: id.clone(),
                        },
                    )
                    .is_ok()
            });
            if sent {
                self.floating_source_clear_frames.remove(&id);
            }
        }
        let destinations_presented = completed_primary_frame_gates(
            &self.floating_destination_present_frames,
            self.last_primary_frame_presented,
        );
        for id in destinations_presented {
            let sent = self
                .app
                .floating
                .client
                .as_mut()
                .is_some_and(|client| {
                    client
                        .send(
                            &crate::floating_surface::FloatingSurfaceClientMessage::DestinationPresented {
                                id: id.clone(),
                            },
                        )
                        .is_ok()
                });
            if sent {
                self.floating_destination_present_frames.remove(&id);
            }
        }
        if surface_gone {
            let reason = crate::render_thread::take_surface_loss()
                .unwrap_or_else(|| "its drawing surface was lost".to_owned());
            stop_abnormally(&self.closing, reason);
            return;
        }
        // Do not consume sync/layout/dirty state unless a finite render buffer
        // is available. Worker completion wakes this window if admission defers.
        match crate::render_thread::prepare_frame(
            &mut self.primary_retry,
            &mut render.buffers,
            crate::time::now(),
        ) {
            Ok(true) => {}
            Ok(false) => return,
            Err(_) => {
                stop_abnormally(&self.closing, "its render thread stopped".to_owned());
                return;
            }
        }
        sync_safe_area(&mut self.app, window);
        #[cfg(target_os = "linux")]
        self.decorations.refresh(&mut self.app, window);
        self.slot_bar.refresh(&mut self.app, window);
        let app = &mut self.app;

        // Continue one bounded provider page per frame. Re-querying resets the
        // palette generation/cursors, so stale work cannot leak into a newer
        // query. A follow-up redraw is requested only while more pages exist.
        let (search_pending, search_announcement) =
            app.actions
                .action_palette
                .as_mut()
                .map_or((false, None), |palette| {
                    let progress = palette.advance_search_page();
                    (progress.pending, palette.take_search_announcement())
                });
        needs_followup_redraw |= search_pending;
        if let (Some(reader), Some(announcement)) = (
            app.accessibility.screen_reader.as_mut(),
            search_announcement,
        ) {
            reader.announce(announcement, tts::Priority::Notification, false);
        }

        let inner = window.inner_size();
        // The frame is drawn at the window's size and scale unless that is
        // past the device's largest texture (a window over several
        // displays): then at the lower scale that fits, which the
        // compositor stretches over the window. Input and accessibility
        // keep the window's own scale; both meet in logical pixels.
        let extent = render.frame_extent(inner, window.scale_factor());
        let width = extent.width;
        let height = extent.height;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "scale factor is intentionally narrowed to f32 for the render pipeline; identical to today's output"
        )]
        let scale_factor = extent.scale as f32;
        let window_scale = window.scale_factor();
        app.layout.last_frame_size = Some(inner);
        let mut floating_content_changed =
            app.layout.layout_dirty || app.layout.structure_dirty || app.needs_sync;
        // Model and input changes reach the pixels through the widgets they
        // dirty, whose damage the frame collects below. Only what damage
        // cannot describe repaints the whole window: a rebuilt tree, moved
        // geometry, a resized surface, and layers drawn over the widgets.
        let mut full_redraw_required = app.layout.structure_dirty;
        // Armed for this frame: each rebuild it does clears it unless the
        // widgets behind it described the change (a long list mounting the
        // rows a scroll brought near).
        app.layout.frame_rebuilds_described = true;

        // Records first: if the root widget opened a different
        // record, sync against that record's document, not the
        // outgoing one's.
        mark_first_frame("first-frame-record-start");
        let record_changed = app.swap_requested_component() | app.record_tick();
        mark_first_frame("first-frame-record-done");
        floating_content_changed |= record_changed;
        // A different record is a different document under the same tree.
        full_redraw_required |= record_changed;

        let __prof_frame_start = app.profiler.as_ref().map(|_| crate::time::now());
        // Drain model notifications and advance update-driven widgets before
        // synchronization. A worker's `Wake` event reaches this pass without
        // requiring the app to own a proxy or poll inside its model.
        mark_first_frame("first-frame-update-start");
        let sync_phase = slow_event::phase("sync");
        if app.update_native_model() {
            floating_content_changed = true;
        }
        mark_first_frame("first-frame-update-done");
        mark_first_frame("first-frame-sync-start");
        if app.sync_model(false) {
            floating_content_changed = true;
        }
        // A control may have asked for larger text or high contrast; both
        // rebuild the tree, so this frame repaints everything in the new look.
        if app.apply_accessibility_request() {
            floating_content_changed = true;
        }
        if crate::component_lifecycle::take_window_close_intent(app.root_widget.as_mut())
            || app.take_close_window_request()
        {
            if app.admit_native_close_request() {
                self.closing.set(true);
                return;
            }
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
        drop(sync_phase);
        mark_first_frame("first-frame-sync-done");
        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::Sync, __prof_frame_start);
        }

        mark_first_frame("first-frame-live-regions-start");
        app.scan_live_regions();
        mark_first_frame("first-frame-live-regions-done");

        // E.g. a tab split inserting a new WindowGroup: rebuild the
        // taffy tree from scratch so new widgets get node ids the
        // layout pass below can see.
        mark_first_frame("first-frame-build-layout-start");
        app.layout.structure_dirty |= app.take_root_structure_dirty();
        // Captured BEFORE `rebuild_structure` clears it — a fresh
        // taffy tree always needs a full solve regardless of the
        // scroll fast path below.
        let structure_rebuilt = app.layout.structure_dirty;
        floating_content_changed |= structure_rebuilt;
        let __prof_build = app.profiler.as_ref().map(|_| crate::time::now());
        if app.layout.structure_dirty {
            app.rebuild_structure();
        }
        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::BuildLayout, __prof_build);
        }

        mark_first_frame("first-frame-build-layout-done");

        let perf = perf_enabled();
        let t_frame_start = if perf { Some(crate::time::now()) } else { None };
        let Some(root_node) = app.layout.root_node else {
            tracing::error!("root_node unset during a redraw; skipping frame");
            return;
        };
        mark_first_frame("first-frame-layout-start");
        // Every widget's box before a relayout that rebuilds nothing, and
        // what says whether the layout pass rebuilt or resized: a frame
        // whose only moves are scrolls this window copies stays sparse.
        let boxes_before = (!structure_rebuilt
            && app.layout.layout_dirty
            && app
                .history
                .as_ref()
                .is_none_or(|history| history.live_interactive())
            && app.history_layout.is_none()
            && app.close_admission.dialog.is_none())
        .then(|| crate::scroll_copy::placed(app.root_widget.as_ref()));
        let layout_before = (app.layout.rebuilds, app.layout.last_layout_logical);
        let layout_phase = slow_event::phase("layout");
        let Some((geometry_changed, t_compute_us, t_apply_us)) = run_layout_pass(
            app,
            root_node,
            width,
            height,
            scale_factor,
            structure_rebuilt,
            Some(&crate::widget::NativeViewHost::live(
                window,
                app.input.platform,
            )),
        ) else {
            return;
        };
        drop(layout_phase);
        mark_first_frame("first-frame-layout-done");
        // A local read this frame would show has not answered and is still
        // inside its frame budget ([`crate::arriving`]): the frame on screen
        // stays until the data lands (its wake redraws) or the budget runs
        // out (`about_to_wait` redraws then), so a window or a view opens
        // complete. Like an aborted layout, the held frame keeps its render
        // admission; the frame that is drawn rebuilds and repaints in full.
        let now = crate::time::now();
        if let Some(until) =
            held_frame::frame_held_until(app.root_widget.holds_frame_until(now), now)
        {
            app.layout.structure_dirty = true;
            self.held_frame = Some(until);
            return;
        }
        self.held_frame = None;
        // A relayout that moved widgets and rebuilt or resized nothing may be
        // scrolls this window copies; that is decided with the frame's damage.
        let moved_from = boxes_before.filter(|_| {
            geometry_changed
                && app.layout.last_layout_logical == layout_before.1
                && (app.layout.rebuilds == layout_before.0 || app.layout.frame_rebuilds_described)
        });
        full_redraw_required |= structure_rebuilt || (geometry_changed && moved_from.is_none());
        if geometry_changed {
            // A rebuild marks it in `rebuild_structure`; a moved control
            // under a still pointer has not seen it either.
            app.input.pointer_replay.mark_unsettled();
        }
        // Read before `draw_frame_content` drains widget dirty flags via
        // `mark_drawn`. Clean compositor/exposure redraws do not invalidate
        // independent native floating surfaces.
        floating_content_changed |= app.root_widget.needs_redraw() || app.history_needs_redraw();

        // Admission reserved this pair before state updates. Taking it cannot
        // wait on GPU work; aborted layouts retain it in the handle for retry.
        let __prof_pool_wait = app.profiler.as_ref().map(|_| crate::time::now());
        let Ok((mut scene, mut damage_regions)) = render.buffers.recv() else {
            stop_abnormally(&self.closing, "its render thread stopped".to_owned());
            return;
        };
        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::ScenePoolWait, __prof_pool_wait);
        }
        damage_regions.clear();
        // Presented pixels lost geometry to a GPU scratch overflow; damage
        // cannot describe where.
        full_redraw_required |= render.take_full_repaint();
        full_redraw_required |= app.debug_mode
            || app.actions.action_palette.is_some()
            || app.input.tooltip_host.needs_frame()
            || app.input.tooltip_host.visible().is_some()
            || app
                .profiler
                .as_ref()
                .is_some_and(crate::profiling::FrameProfiler::hud_enabled);
        // The worker resets scenes before returning them to this pool; the
        // pre-seeded scenes are empty too.

        let t_draw_start = if perf { Some(crate::time::now()) } else { None };
        crate::webview::frame_trace::begin_scene();
        // Floating chrome (menus, popovers, sheets, the palette, tooltips)
        // records what lies below it, and the render worker blurs that
        // backdrop under the material before the frame renders.
        let backdrop_recording = crate::backdrop::BackdropRecording::begin();
        crate::backdrop::layer_scene(&scene, 0);
        let draw_phase = slow_event::phase("draw");
        let drawn = draw_frame_content(app, &mut scene, width, height, scale_factor, t_draw_start);
        drop(draw_phase);
        // An empty state waiting for its room's first sync turns a spinner
        // no widget's damage covers (`crate::room_sync`): a new pose damages
        // its slot, in scene pixels.
        let spinner_damage = crate::room_sync::take_spinner_damage();
        let (transform, window_rect, t_draw_us) =
            (drawn.transform, drawn.window_rect, drawn.draw_us);
        // The tooltip's material blurs what is under it, which repaints this
        // window in full on the render worker whatever damage says; until it
        // takes damage there too, a tooltip is a layer like the others here.
        let mut top_layer = drawn.top_layer.is_some() || drawn.tooltip.is_some();
        // What paints over the widgets without moving with a scroll and is no
        // widget the copy planner sees: the header bar another desktop gets.
        let before_decorations = scene.encoding().draw_tags.len();
        #[cfg(target_os = "linux")]
        {
            self.decorations.draw(app, &mut scene, transform);
            full_redraw_required |= self.decorations.take_repaint();
        }
        self.slot_bar.draw(app, &mut scene, transform);
        full_redraw_required |= self.slot_bar.take_repaint();
        let decorated = scene.encoding().draw_tags.len() != before_decorations;
        let backdrop =
            crate::backdrop::BackdropFrame::from_single_scene(backdrop_recording.finish());
        let __prof_presence = app.profiler.as_ref().map(|_| crate::time::now());
        // Peer pointers, names and activity paint over the widgets and damage
        // only the marks that changed. Carets, selections and marks widgets
        // paint from presence carry no widget damage: a change to them (or
        // unprovable layer damage) repaints the window.
        let presence_damage = draw_presence_layer(
            app,
            &mut self.presence_layer,
            transform,
            window_rect,
            crate::time::now(),
        );
        scene.append(self.presence_layer.scene(), None);
        full_redraw_required |= presence_damage.is_none();
        let content_draw_tags = scene.encoding().draw_tags.len();

        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::PresenceOverlay, __prof_presence);
        }

        let mut native_main_stages =
            crate::render_thread::timing::enabled().then(MainStages::default);
        record_main_stage(&mut native_main_stages, MainStage::BeforeProfilerCommit);

        // Folds in any `handle_event` time banked since last frame.
        if let (Some(p), Some(start)) = (app.profiler.as_mut(), __prof_frame_start) {
            p.commit_frame(u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX));
            record_main_stage(&mut native_main_stages, MainStage::AfterProfilerCommit);
            if !self.native_input_ids.is_empty() {
                let cpu_frame_id = p.history().back().map(|sample| sample.frame);
                crate::native_scenario::log::emit_json(
                    "tilo-native-frame",
                    serde_json::json!({
                        "cpu_frame_id": cpu_frame_id,
                        "input_ids": std::mem::take(&mut self.native_input_ids),
                        "frame_commit_us": self.native_input_origin.map(|origin| origin.elapsed().as_micros()),
                        // Capture after drawing: layout may clamp scroll state.
                        // Input-time effects alone cannot prove the frame used it.
                        "state": self.slot_bar.with_search_evidence(app.native_input_snapshot()),
                    }),
                );
            }
        } else {
            record_main_stage(&mut native_main_stages, MainStage::AfterProfilerCommit);
        }
        crate::webview::frame_trace::finish_scene(
            app.profiler
                .as_ref()
                .and_then(|p| p.history().back())
                .map(|sample| sample.frame),
        );
        record_main_stage(&mut native_main_stages, MainStage::AfterNativeFrameLog);

        if app.debug_mode {
            draw_debug_outlines(app, &mut scene, transform);
        }
        record_main_stage(&mut native_main_stages, MainStage::AfterDebugOutlines);

        // Debug outlines paint over the widgets too. A layer drawn this
        // frame, or one erased since the last, covers pixels no widget's
        // damage reports. (The presence layer reports its own.)
        top_layer |= scene.encoding().draw_tags.len() != content_draw_tags;
        full_redraw_required |=
            top_layer || std::mem::replace(&mut self.top_layer_drawn, top_layer);

        // Animated widgets update their visual bounds while drawing (for
        // example, an indeterminate progress segment records its old and new
        // positions there). Capture damage only after that work, but before
        // `mark_drawn` clears regular dirty flags. This makes the submitted
        // scene and its damage describe the same frame rather than trailing
        // animation by one frame.
        let mut scroll_copies = Vec::new();
        if !full_redraw_required {
            let root = app.root_widget.as_ref();
            // The retained target holds the last presented frame, so a scroll
            // copies what stays on screen (`crate::scroll_copy`).
            let mut offers = crate::scroll_copy::accepting(|| {
                collect_submitted_widget_damage(root, &mut damage_regions);
            });
            // Widget damage is logical, like the spinner's slot once the
            // scale is taken out of it; both are scaled back together.
            damage_regions.extend(
                spinner_damage.map(|slot| slot.scale_from_origin(1.0 / f64::from(scale_factor))),
            );
            // Moved geometry repaints the window unless every move is a
            // scroll's content moving with it; rows a long list mounted or
            // let go of in it are damage beside the copy.
            if let Some(before) = &moved_from {
                let moves = (!offers.is_empty()).then(|| {
                    crate::scroll_copy::moves_besides(
                        before,
                        &crate::scroll_copy::placed(root),
                        &offers,
                    )
                });
                match moves {
                    Some(moves) if moves.besides.is_empty() => {
                        damage_regions.extend(moves.carried);
                    }
                    _ => full_redraw_required = true,
                }
            }
            // Peer marks and a header bar paint over the content without
            // moving with it, where the planner cannot see them. The marks
            // are damage around the copies where they can be bounded; the
            // header bar, or marks that cannot, repaint the scrollers.
            let window = KurboRect::new(0.0, 0.0, f64::from(width), f64::from(height));
            let marks = if offers.is_empty() {
                None
            } else {
                peer_marks(&self.presence_layer, &mut self.mark_bounds, window)
            };
            if !offers.is_empty() && (decorated || marks.is_none()) {
                damage_regions.extend(offers.iter().map(crate::scroll_copy::ScrollOffer::viewport));
                offers.clear();
            }
            if full_redraw_required {
                offers.clear();
            }
            let history = app.history.as_deref().map(|history| history as &dyn Widget);
            let planned = crate::scroll_copy::plan(
                &offers,
                &std::iter::once(root).chain(history).collect::<Vec<_>>(),
                f64::from(scale_factor),
                (width, height),
            );
            if full_redraw_required
                || !submit_presence_damage(
                    &mut damage_regions,
                    presence_damage.as_deref().unwrap_or_default(),
                    self.presence_layer.widgets_place_content_marks(),
                    f64::from(scale_factor),
                    window,
                )
            {
                full_redraw_required = true;
                damage_regions.clear();
            } else {
                // Already in device pixels, and exact: no effect margin. What
                // changed in place where a copy reads it lands one delta
                // away, as do the peer marks the copies took; what the
                // copies exposed is repainted as it is.
                damage_regions.extend(planned.damage);
                crate::scroll_copy::carry(&planned.copies, &mut damage_regions);
                if let Some(marks) = &marks {
                    crate::scroll_copy::marks_over_copies(
                        &planned.copies,
                        &marks.shown,
                        &marks.painted,
                        &mut damage_regions,
                    );
                }
                damage_regions.extend(planned.exposed);
                scroll_copies = planned.copies;
            }
        }
        if !app.capture_frame_ready() {
            scene.reset();
            render.buffers.unprepare((scene, damage_regions));
            // The service socket wakes the event loop when its acknowledgement
            // arrives. Keep dirty flags until that frame can actually be sent.
            return;
        }
        // This must follow damage collection: widget `draw` may update
        // damage geometry and `mark_drawn` consumes persistent dirty flags.
        complete_native_frame_content(app, |app| {
            record_main_stage(&mut native_main_stages, MainStage::AfterDamageMarkDrawn);

            // `update_if_active` only builds + sends when an AT is
            // attached, so this is free on the common no-reader path.
            if let Some(adapter) = self.a11y_adapter.as_mut() {
                let _a11y = slow_event::phase("a11y");
                let app_ref: &mut App = app;
                let scale = window_scale;
                adapter.update_if_active(|| {
                    crate::accesskit_bridge::native_tree_update(
                        app_ref.build_a11y_tree_update(scale),
                    )
                });
            }
            record_main_stage(&mut native_main_stages, MainStage::AfterAccessibility);
        });

        // Painted last, after the frame timing was committed above,
        // so the HUD's own cost never enters its numbers.
        if app
            .profiler
            .as_ref()
            .is_some_and(crate::profiling::FrameProfiler::hud_enabled)
        {
            let (hud_w, hud_h) =
                crate::scale::logical_size((width, height), f64::from(scale_factor));
            if let Some(p) = app.profiler.as_ref() {
                p.draw_hud(
                    &mut scene,
                    transform,
                    &mut app.text_renderer,
                    &app.theme,
                    (hud_w, hud_h),
                );
            }
        }
        let presents = crate::render_thread::retry::submit_redraw_frame(
            full_redraw_required,
            !damage_regions.is_empty() || !scroll_copies.is_empty(),
        );
        // `needs_animation_frame` is for motion: frames that change nothing
        // for a second mean a widget polls through them. Debug builds name it.
        if cfg!(debug_assertions)
            && self
                .still_animation
                .frame(app.root_widget.needs_animation_frame(), presents)
        {
            crate::diag!(
                "tilo-ui: a second of animation frames painted nothing; asked for by {:?}. \
                 Wait on a wake deadline or a waking channel (ui::wake::waking_channel) instead.",
                animation_schedule::animating_leaves(app.root_widget.as_ref())
            );
        }
        if presents {
            let cpu_frame_id = app
                .profiler
                .as_ref()
                .and_then(|profiler| profiler.history().back())
                .map(|sample| sample.frame)
                .or_else(|| {
                    self.physical_input_trace
                        .as_ref()
                        .map(|_| self.primary_frames_sent.saturating_add(1))
                });

            if let Some(trace) = self.physical_input_trace.as_mut()
                && trace.has_pending()
                && let Some(row) = trace.drawn(
                    cpu_frame_id,
                    self.slot_bar
                        .with_search_evidence(app.native_input_snapshot()),
                )
            {
                crate::native_scenario::log::emit_json("tilo-physical-frame", row);
            }

            let req = RenderRequest::Frame {
                cpu_frame_id,
                scene,
                width,
                height,
                base_color: app.theme.tokens.colors.surfaces.content,
                damage_regions,
                scroll_copies,
                full_redraw: full_redraw_required,
                backdrop,
                on_present: self.dev_reload_timing.take().map(|timing| {
                    let submitted = crate::time::wall();
                    Box::new(move |presented| timing.record(submitted, presented))
                        as crate::render_thread::PresentHook
                }),
            };
            record_main_stage(&mut native_main_stages, MainStage::AfterHudRequest);
            if let Some((origin, entered)) = native_redraw_entry {
                // All three values use the input timeline's monotonic origin.
                // This is a CPU submit attempt, not GPU/display completion.
                crate::native_scenario::log::emit_json(
                    "tilo-native-redraw",
                    serde_json::json!({
                        "cpu_frame_id": app.profiler.as_ref().and_then(|p| p.history().back()).map(|s| s.frame),
                        "redraw_enter_us": entered.duration_since(origin).as_micros(),
                        "timed_build_start_us": __prof_frame_start.map(|start| start.duration_since(origin).as_micros()),
                        "submit_attempt_us": origin.elapsed().as_micros(),
                    }),
                );
            }
            record_main_stage(&mut native_main_stages, MainStage::AfterNativeRedrawLog);
            let send_cpu_frame_id = match &req {
                RenderRequest::Frame { cpu_frame_id, .. } => *cpu_frame_id,
                RenderRequest::Shutdown { .. } => None,
            };
            announce_frame(window);
            let (send_result, send_bounds) = crate::render_thread::timing::around(
                crate::render_thread::timing::enabled(),
                || render.render_tx.send(req),
            );
            if let Some(bounds) = send_bounds {
                crate::native_scenario::log::emit_timing_json(
                    "tilo-native-send",
                    serde_json::json!({
                        "cpu_frame_id": send_cpu_frame_id,
                        "host_clock": crate::native_scenario::host_clock::CLOCK_NAME,
                        "before_host_lo_s": bounds.before.map(|b| b.lo_s),
                        "before_host_hi_s": bounds.before.map(|b| b.hi_s),
                        "after_host_lo_s": bounds.after.map(|b| b.lo_s),
                        "after_host_hi_s": bounds.after.map(|b| b.hi_s),
                        "send_ok": send_result.is_ok(),
                    }),
                );
            }
            if let Some(stages) = native_main_stages {
                crate::native_scenario::log::emit_timing_json(
                    "tilo-native-main-stages",
                    stages.row(send_cpu_frame_id),
                );
            }
            if send_result.is_err() {
                let reason = crate::render_thread::take_surface_loss()
                    .unwrap_or_else(|| "its render thread stopped".to_owned());
                stop_abnormally(&self.closing, reason);
                return;
            }
            mark_first_frame("first-frame-submitted");
            self.presence_layer.presented();
            self.primary_frames_sent = self.primary_frames_sent.saturating_add(1);
            let submitted = self.primary_frames_sent;
            app.capture_frame_submitted(submitted);
            app.floating
                .submitted_action_markers
                .push_back((submitted, app.floating.drawn_action_markers.clone()));
            for required in self.floating_source_clear_frames.values_mut() {
                if required.is_none() {
                    *required = Some(submitted);
                }
            }
            for required in self.floating_destination_present_frames.values_mut() {
                if required.is_none() {
                    *required = Some(submitted);
                }
            }

            let t_send_us = t_frame_start.map_or(0, |t| t.elapsed().as_micros());
            if let Some(_start) = t_frame_start {
                // On a static frame this reads `shape=0us hit=N miss=0`.
                let (shape_hits, shape_misses, shape_ns) = app.text_renderer.take_shape_stats();
                crate::diag!(
                    "perf frame: main_total={:>5}us layout={}({}+{}us) draw={:>5}us (shape={:>4}us hit={} miss={}) send_to_worker={:>5}us",
                    t_send_us,
                    if geometry_changed { "Y" } else { "n" },
                    t_compute_us,
                    t_apply_us,
                    t_draw_us,
                    shape_ns.checked_div(1000).unwrap_or(0),
                    shape_hits,
                    shape_misses,
                    t_send_us,
                );
            }
        } else {
            // The callback that woke this redraw changed no pixels. Presenting
            // would commit a new buffer and a new frame callback, and the
            // shell would paint that echo while the desktop sat still.
            scene.reset();
            render.buffers.unprepare((scene, damage_regions));
        }
        // The surface a handled input left for this frame, now that its sync
        // ran and the frame's availability refresh built the tree.
        app.publish_owed_action_surface();

        if needs_followup_redraw {
            window.request_redraw();
        }
        // The secondary surfaces paint the same app-owned state. A primary
        // frame therefore invalidates each of them exactly once; their own
        // presentation never recursively requests another primary frame.
        if floating_content_changed {
            for surface in &self.floating_windows {
                surface.window.request_redraw();
            }
        }
        if let Some(audience) = self.audience_presentation.as_ref() {
            audience.window.request_redraw();
        }
    }

    /// Whether something consumed the event.
    fn handle_other_window_event(&mut self, window: &Arc<Window>, other: WindowEvent) -> bool {
        let synthesized_touch = self.dispatching_touch_pointer;
        let _dispatch = slow_event::phase("dispatch");
        self.app
            .surface_input(window.as_ref(), other, synthesized_touch)
    }
}

impl AppHandler {
    /// `UserEvent` injection keeps the ordinary window dispatch after admission.
    fn injected_window_event(&mut self, window_id: WindowId, event: WindowEvent) {
        self.route_window_event(
            window_id,
            event,
            native_input_isolation::EventOrigin::Injected,
        );
    }
}

impl AppHandler {
    /// Rebuild into the windows this process already owns.
    ///
    /// The primary window, every live floating window, and an audience window
    /// stay. Reload does not call `create_window`.
    fn reload_open_windows_in_place(&mut self) {
        let mut windows = Vec::new();
        if let Some(window) = &self.window {
            windows.push(("primary".to_owned(), window.clone()));
        }
        for surface in &self.floating_windows {
            if !surface.lifecycle.suspended {
                windows.push((surface.id.clone(), surface.window.clone()));
            }
        }
        if self.audience_presentation.is_some() {
            if let Some(audience) = &self.audience_presentation {
                windows.push(("audience".to_owned(), audience.window.clone()));
            }
        }
        let Some(session) = crate::host::environment().dev_session.clone() else {
            return;
        };
        let library = dev_reload_library(&session)
            .filter(|library| self.dev_loaded_library.as_ref() != Some(library));
        let Some(library) = library else {
            // No new library: a build that did not publish one, or a signal for
            // catalogs alone, keeps the code already running. Catalogs on disk
            // still replace the strings baked into this process.
            super::reload_dev_locale_catalogs_from(std::path::Path::new("locales"));
            self.app.needs_sync = true;
            for (_, window) in &windows {
                window.request_redraw();
            }
            return;
        };
        let received = crate::time::wall();
        let started = crate::time::now();
        let _ = super::write_dev_checkpoint(&self.app, &session);
        let checkpoint = std::fs::read(session.join("checkpoint.bin")).unwrap_or_default();
        let checkpointed = started.elapsed();
        let slots = windows
            .iter()
            .map(|(id, _)| super::DevWindowSlot {
                id: id.clone(),
                state: self
                    .dev_window_state
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| checkpoint.clone()),
            })
            .collect::<Vec<_>>();
        let loading = crate::time::now();
        let reloaded = match super::reload_from_library(&library, &slots) {
            Ok(reloaded) => reloaded,
            Err(error) => {
                eprintln!("tilo: reload rejected; keeping the last good app\n{error}");
                return;
            }
        };
        let loaded = loading.elapsed();
        let installing = crate::time::now();
        if let Some(root) = reloaded.root {
            if !self.app.install_reloaded_view(root) {
                eprintln!(
                    "tilo: the open windows could not take the new view; keeping the running one"
                );
                return;
            }
            self.app.init_layout();
        }
        self.dev_reload_timing = Some(super::DevReloadTiming {
            library: library.clone(),
            received,
            checkpoint: checkpointed,
            load: reloaded.load,
            handoff: loaded.saturating_sub(reloaded.load),
            install: installing.elapsed(),
        });
        self.dev_loaded_library = Some(library);
        self.dev_window_state = reloaded
            .windows
            .into_iter()
            .map(|window| (window.id, window.state))
            .collect();
        super::reload_dev_locale_catalogs_from(std::path::Path::new("locales"));
        self.app.needs_sync = true;
        let count = windows.len();
        for (_, window) in &windows {
            window.request_redraw();
        }
        eprintln!("tilo: reloaded {count} open window(s) in place");
    }
}

impl AppHandler {
    /// Route one event the platform reported for `window_id` (wheel deltas
    /// already summed, [`wheel_batch`]).
    fn platform_window_event(&mut self, window_id: WindowId, event: WindowEvent) {
        let _slow = slow_event::Span::begin("window_event", slow_event::window_event_name(&event));
        // Ctrl+C copies whichever source is typing: name the chord by its
        // Latin key before any handler matches it.
        let event =
            crate::latin_chord::renamed(&event, self.app.input.modifiers.state()).unwrap_or(event);
        // What this event opens or closes follows it (a key lands it at
        // once, a click moves it): `motion::input_source`.
        crate::motion::note_input(&event);
        if let Some(window) = self
            .window
            .clone()
            .filter(|window| window.id() == window_id)
            && self
                .private_input
                .event(&mut self.app, window.as_ref(), &event)
        {
            self.sync_ime(&window);
            return;
        }
        #[cfg(target_os = "android")]
        if let WindowEvent::Touch(touch) = &event
            && touch.phase == winit::event::TouchPhase::Started
            && self.ime_state.is_some()
            && self.touches_focused_field(window_id, touch.location)
        {
            for settled in self.text_input.settle() {
                self.route_window_event(
                    window_id,
                    settled,
                    native_input_isolation::EventOrigin::Platform,
                );
            }
            self.text_input_touched = true;
        }
        let physical_input = if crate::physical_input_trace::Trace::observes(&event)
            && self
                .window
                .as_ref()
                .is_some_and(|window| window.id() == window_id)
        {
            let state = self.physical_input_trace.as_ref().and_then(|_| {
                self.slot_bar
                    .with_search_evidence(self.app.native_input_snapshot())
            });
            self.physical_input_trace
                .as_mut()
                .and_then(|trace| trace.begin(&event, state))
        } else {
            None
        };
        self.note_host_appearance_event(&event);
        let admitted = self.route_window_event(
            window_id,
            event,
            native_input_isolation::EventOrigin::Platform,
        );
        if let Some(input) = physical_input {
            let mut row = crate::physical_input_trace::Trace::effect(
                input,
                self.slot_bar
                    .with_search_evidence(self.app.native_input_snapshot()),
            );
            let modifiers = self.app.input.modifiers.state();
            row["input_route_admitted"] = serde_json::json!(admitted);
            row["modifiers"] = serde_json::json!({
                "shift": modifiers.shift_key(), "control": modifiers.control_key(),
                "alt": modifiers.alt_key(), "super": modifiers.super_key(),
            });
            crate::native_scenario::log::emit_json("tilo-physical-input", row);
        }
    }
}

impl ApplicationHandler<UserEvent> for AppHandler {
    #[allow(
        clippy::too_many_lines,
        reason = "winit requires the complete synthetic event dispatch at this callback boundary"
    )]
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        let _slow = slow_event::Span::begin(
            "user_event",
            match &event {
                UserEvent::AccessKit { .. } => "AccessKit",
                UserEvent::RenderReady(_) => "RenderReady",
                _ => "other",
            },
        );
        let _entry = self.app.enter_wake_scope();
        let event = match event {
            UserEvent::AccessKit { window_id, event }
                if self
                    .floating_windows
                    .iter()
                    .any(|surface| surface.window.id() == window_id) =>
            {
                self.handle_secondary_accessibility_event(window_id, event);
                return;
            }
            UserEvent::AccessKit { window_id, .. }
                if self
                    .window
                    .as_ref()
                    .is_none_or(|window| window.id() != window_id) =>
            {
                // A queued request from a surface that has already closed
                // must not be reinterpreted against the primary tree.
                return;
            }
            UserEvent::RenderReady(id) => {
                if let Some(window) = self.window.as_ref().filter(|window| window.id() == id) {
                    window.request_redraw();
                } else if let Some(surface) = self
                    .floating_windows
                    .iter()
                    .find(|surface| surface.window.id() == id)
                {
                    surface.window.request_redraw();
                } else if let Some(audience) = self
                    .audience_presentation
                    .as_ref()
                    .filter(|audience| audience.window.id() == id)
                {
                    audience.window.request_redraw();
                }
                return;
            }
            UserEvent::Wake(wake) => {
                if self.app.accept_wake(&wake)
                    && wake.requires_sync()
                    && let Some(window) = self.window.as_ref()
                {
                    window.request_redraw();
                }
                return;
            }
            UserEvent::DevSessionStop => {
                self.app.autosave_flush();
                super::write_dev_checkpoint_now(&self.app);
                self.closing.set(true);
                return;
            }
            UserEvent::DevSessionReload => {
                self.reload_open_windows_in_place();
                return;
            }
            // The process opens the window ([`process_windows`]), not one of
            // its windows.
            UserEvent::OpenWindow(_) => return,
            other => other,
        };
        let Some(window) = self.window.clone() else {
            return;
        };
        let window_id = window.id();
        let event = match event {
            UserEvent::NativeScenarioInput(packet) => {
                if self.native_input_origin.is_none() {
                    let size = window.inner_size();
                    crate::native_scenario::log::emit_json(
                        "tilo-native-window",
                        serde_json::json!({
                            "scale_factor": window.scale_factor(),
                            "physical_width": size.width,
                            "physical_height": size.height,
                            "pointer_passthrough": self.native_input_pointer_passthrough,
                            "input_route_isolation": self.native_input_route_isolation,
                        }),
                    );
                }
                self.native_input_origin.get_or_insert(packet.origin);
                let (now, dispatch_host) = crate::native_scenario::host_clock::sample();
                crate::native_scenario::log::emit_json(
                    "tilo-native-input",
                    serde_json::json!({
                        "id": packet.id,
                        "warmup": packet.warmup,
                        "input": packet.input,
                        "scheduled_us": packet.scheduled_at.duration_since(packet.origin).as_micros(),
                        "sent_us": packet.sent_at.duration_since(packet.origin).as_micros(),
                        "dispatch_start_us": now.duration_since(packet.origin).as_micros(),
                        "host_clock": crate::native_scenario::host_clock::CLOCK_NAME,
                        "sent_host_lo_s": packet.sent_host.map(|bounds| bounds.lo_s),
                        "sent_host_hi_s": packet.sent_host.map(|bounds| bounds.hi_s),
                        "dispatch_host_lo_s": dispatch_host.map(|bounds| bounds.lo_s),
                        "dispatch_host_hi_s": dispatch_host.map(|bounds| bounds.hi_s),
                        "lateness_us": now.saturating_duration_since(packet.scheduled_at).as_micros(),
                    }),
                );
                let before = self
                    .slot_bar
                    .with_search_evidence(self.app.native_input_snapshot());
                self.native_input_ids.push(packet.id);
                // Pen steps carry their samples ahead of each pointer event,
                // so a paced stroke reaches widgets as a tablet's would.
                for event in packet.input.window_events() {
                    self.injected_window_event(window_id, event);
                }
                if let Some(before) = before {
                    crate::native_scenario::log::emit_json(
                        "tilo-native-effect",
                        serde_json::json!({
                            "id": packet.id, "before": before, "after": self.slot_bar.with_search_evidence(self.app.native_input_snapshot()),
                        }),
                    );
                }
                return;
            }
            other => other,
        };
        match event {
            UserEvent::Hover { x, y } => {
                let ev = crate::synthetic_input::cursor_moved(x, y);
                self.injected_window_event(window_id, ev);
            }
            UserEvent::Click { x, y } => {
                let moved = crate::synthetic_input::cursor_moved(x, y);
                self.injected_window_event(window_id, moved);
                let press = crate::synthetic_input::mouse_press();
                self.injected_window_event(window_id, press);
                let release = crate::synthetic_input::mouse_release();
                self.injected_window_event(window_id, release);
            }
            UserEvent::Key(key) => {
                for state in [ElementState::Pressed, ElementState::Released] {
                    self.injected_window_event(
                        window_id,
                        crate::synthetic_input::key_input(&key, state),
                    );
                }
            }
            UserEvent::Wheel { dx, dy } => {
                let ev = crate::synthetic_input::mouse_wheel(dx, dy);
                self.injected_window_event(window_id, ev);
            }
            UserEvent::External(payload) => {
                let app = &mut self.app;
                app.root_widget.on_external_event(
                    &payload,
                    &mut crate::widget::Edits::lend(&mut app.state.doc),
                    &app.state.root,
                );
                app.needs_sync = true;
                window.request_redraw();
            }
            UserEvent::HostCommand(invocation) => {
                self.app.invoke_host_command(&invocation);
                self.app.needs_sync = true;
                self.app.layout.layout_dirty = true;
                window.request_redraw();
            }
            UserEvent::HostSearch(query) => {
                if self
                    .app
                    .invoke_host_shortcut(crate::platform::HostShortcut::Search)
                    && let Some(palette) = self.app.actions.action_palette.as_mut()
                {
                    palette.set_query(&query);
                }
                self.app.layout.layout_dirty = true;
                window.request_redraw();
            }
            #[cfg(target_os = "android")]
            UserEvent::PlatformTextInput => {
                for op in super::android_text_input::bridge::drain() {
                    for event in self.text_input.events(op) {
                        self.route_window_event(
                            window_id,
                            event,
                            native_input_isolation::EventOrigin::Platform,
                        );
                    }
                }
                window.request_redraw();
            }
            UserEvent::Wake(_)
            | UserEvent::NativeScenarioInput(_)
            | UserEvent::RenderReady(_)
            | UserEvent::DevSessionStop
            | UserEvent::DevSessionReload => {}
            UserEvent::OpenWindow(_) => {}
            UserEvent::AccessKit {
                event: ak_event, ..
            } => {
                use accesskit_winit::WindowEvent as AkWindowEvent;
                match ak_event {
                    AkWindowEvent::InitialTreeRequested => {
                        if let Some(adapter) = self.a11y_adapter.as_mut() {
                            let scale = window.scale_factor();
                            let app_ref: &mut App = &mut self.app;
                            adapter.update_if_active(|| {
                                crate::accesskit_bridge::native_tree_update(
                                    app_ref.build_a11y_tree_update(scale),
                                )
                            });
                        }
                        window.request_redraw();
                    }
                    AkWindowEvent::ActionRequested(req) => {
                        // Assistive technology acts like a shortcut: what it
                        // opens lands at once.
                        crate::motion::note_source(crate::tokens::MotionSource::Action);
                        // `Some((x, y))` means "synthesise a click here".
                        if let Some(action) = crate::accesskit_bridge::accesskit_action_to_ours(
                            req.action,
                            req.data.as_ref(),
                        ) {
                            let click = self.app.dispatch_a11y_action(req.target_node, &action);
                            if let Some((x, y)) = click {
                                let scale = window.scale_factor();
                                let (px, py) = (x * scale, y * scale);
                                let moved = crate::synthetic_input::cursor_moved(px, py);
                                self.injected_window_event(window_id, moved);
                                let press = crate::synthetic_input::mouse_press();
                                self.injected_window_event(window_id, press);
                                let release = crate::synthetic_input::mouse_release();
                                self.injected_window_event(window_id, release);
                            }
                            window.request_redraw();
                        }
                    }
                    AkWindowEvent::AccessibilityDeactivated => {}
                }
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let _entry = self.app.enter_wake_scope();
        // Fires again after `suspended` on mobile, where the window survives
        // but its native surface was destroyed: the next redraw rebuilds the
        // renderer against the new surface.
        self.surface_suspended = false;
        if let Some(window) = &self.window {
            self.app.layout.layout_dirty = true;
            window.request_redraw();
            return;
        }
        // Required, not incidental: the compositor only accepts
        // `set_selection` from the connection that owns the focused surface.
        if self.clipboard_attachment.is_none() {
            self.clipboard_attachment =
                crate::host::native::attach_display(event_loop.owned_display_handle());
        }
        // AccessKit's adapter asserts it's created while the window is
        // hidden, so build invisible, create the adapter, show.
        let title = resolve_window_title(self.app.window.title.as_deref());
        let icon = if takes_window_icon(event_loop) {
            crate::window_icon::resolve()
        } else {
            None
        };
        let app_id = primary_window_app_id(self.app.window.app_id.as_deref());
        // The window opens at the content zoom this app had here.
        self.app.restore_content_zoom(&app_id);
        // Its shells take up the sidebar widths the person left them at.
        crate::widget::remember_sidebar_widths_for_app(&app_id);
        // The window the launch made before constructing the app, when it
        // announces the id this app's window must have. Its surface may
        // already be configured on the renderer-preparation thread.
        let early = match early_for(take_early_window(), &app_id) {
            Early::Kept(window) => {
                tilo_startup_metrics::mark("startup-window-adopted");
                window.set_title(&title);
                Some(window)
            }
            Early::Discarded => {
                // An input surface announces its own id: its window is made
                // below, and `init_render` discards the early surface.
                tilo_startup_metrics::mark("startup-window-discarded");
                None
            }
            Early::Absent => None,
        };
        let window = early.unwrap_or_else(|| {
            let window_attributes = primary_window_attributes(title, icon.clone(), &app_id);
            // `resumed` is a winit `ApplicationHandler` callback returning `()`,
            // so a `create_window` failure (compositor gone) has nowhere to
            // propagate to; there is no window to run without it either way.
            #[allow(
                clippy::unwrap_used,
                reason = "fixed () trait signature; window-creation failure at startup is unrecoverable"
            )]
            Arc::new(event_loop.create_window(window_attributes).unwrap())
        });
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::ActiveEventLoopExtMacOS;
            if let Err(error) = event_loop.set_quit_request_window(Some(window.id())) {
                crate::diag!("tilo: cannot protect application Quit: {error}");
                self.closing.set(true);
                return;
            }
        }
        tilo_startup_metrics::mark("primary-window-created");
        self.native_input_pointer_passthrough = false;
        self.native_input_route_isolation = false;
        if std::env::var("TILO_PROFILE_POINTER_PASSTHROUGH").as_deref() == Ok("1")
            && std::env::var("TILO_PROFILE_WINDOW_NO_ACTIVATE").as_deref() == Ok("1")
            && (self.native_scenario.is_some() || self.native_input_origin.is_some())
        {
            // Physical pointer movement must not replace the scripted hover.
            // Scripted inputs still use the normal UserEvent dispatch route.
            if let Err(error) = window.set_cursor_hittest(false) {
                eprintln!("Native benchmark pointer isolation failed: {error}");
                self.closing.set(true);
                return;
            }
            self.native_input_pointer_passthrough = true;
            self.native_input_route_isolation = true;
        }
        // Some backends only honour the icon post-creation; a no-op on
        // Wayland compositors that source it from the app_id instead.
        window.set_window_icon(icon);

        // Must be created before the window is shown.
        // One provider per view: the previous one must be withdrawn first.
        self.a11y_adapter = None;
        self.a11y_adapter =
            create_accessibility_adapter(event_loop, &window, self.a11y_proxy.clone());
        crate::widget::mount_native_views(
            self.app.root_widget.as_mut(),
            &crate::widget::NativeViewHost::live(&window, self.app.input.platform),
        );
        if let Some(host) = self.host_appearance.as_mut() {
            host.note_window_theme(window.theme());
        }
        self.apply_host_appearance(false);
        #[cfg(windows)]
        if let Err(reason) = self.init_render(&window) {
            crate::diag!("tilo: {reason}");
            self.closing.set(true);
            return;
        }
        #[cfg(any(target_os = "macos", windows))]
        self.install_host_chrome(&window);
        // Honour system/presentation roots before the first visible configure.
        // The normal reconcile path still owns output selection and later changes.
        self.app.reconcile_fullscreen(&window);
        tilo_startup_metrics::mark("primary-window-show-start");
        window.set_visible(true);
        tilo_startup_metrics::mark("primary-window-show-done");

        self.app.layout.layout_dirty = true;
        // GPU init deferred to first `RedrawRequested` (`init_render`): the
        // ~50ms surface+device init here would block `resumed`'s seat
        // wiring — on mutter that intermittently left the toplevel
        // unfocused with dead input on first launch.
        window.request_redraw();
        tilo_startup_metrics::mark("primary-window-redraw-requested");
        crate::dnd::set_drag_window(&window);
        self.window = Some(window);
        tilo_startup_metrics::mark("primary-window-resumed-done");
    }

    /// Android destroys the window's native surface when the activity leaves
    /// the screen and may kill the process any time after. Save the app's
    /// state and drop the surface while it is still valid; the renderer,
    /// window, widget tree and document stay for the next `resumed`.
    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        let _entry = self.app.enter_wake_scope();
        self.surface_suspended = true;
        for error in self.app.checkpoint_before_close() {
            tracing::error!(%error, "could not save state on suspend");
        }
        if let Some(mut render) = self.render.take() {
            self.suspended_renderer = render.shutdown_keeping_renderer();
        }
    }

    /// BEFORE the `Window` arc is dropped — the worker's
    /// `RenderSurface<'static>` references it, and wgpu may panic if the
    /// surface outlives the window.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Close clipboard admission before the runner dismantles its native
        // surfaces. Worker teardown retains its own display, off this thread.
        self.clipboard_attachment = None;
        self.app.close_wake_scope();
        let _entry = self.app.enter_wake_scope();
        // The Unix adapter owns an asynchronous AT-SPI provider.  Drop it
        // before the native views and window disappear, so external clients
        // receive the provider's normal unregister/teardown rather than being
        // left with object paths whose backing Wayland surface is already
        // gone.  This also makes the ordering explicit instead of relying on
        // AppHandler field-drop order.
        self.a11y_adapter = None;
        if !self.app.close_admission.admitted {
            self.app.autosave_flush();
        }
        self.app.close_native_content();
        self.close_audience_presentation();
        for surface in &mut self.floating_windows {
            if let Some(mut render) = surface.render.take() {
                render.shutdown_and_join();
            }
        }
        self.floating_windows.clear();
        if let Some(mut render) = self.render.take() {
            render.shutdown_and_join();
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // Wheel deltas reported in one wake route as one event; anything
        // else routes the waiting sum first.
        for (window_id, event) in self.wheel_batch.push(window_id, event) {
            self.platform_window_event(window_id, event);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let _slow = slow_event::Span::begin("about_to_wait", "");
        // The wake's last wheel deltas go before the loop waits.
        if let Some((window_id, event)) = self.wheel_batch.flush() {
            self.platform_window_event(window_id, event);
        }
        self.app.service_close_admission();
        if self.app.finish_close_if_ready() {
            self.closing.set(true);
            return;
        }
        // Someone chose who works in this workspace while the window waited
        // for it: save what it holds and leave the loop, keeping the window,
        // so the launch opens the app joined to its workspace in it.
        if !self.handing_over && crate::workspace_identity::chosen() {
            self.handing_over = true;
            for error in self.app.checkpoint_before_close() {
                tracing::error!(%error, "could not save before opening the workspace");
            }
            self.closing.set(true);
            return;
        }
        let _entry = self.app.enter_wake_scope();
        let Some(window) = self.window.clone() else {
            return;
        };
        if std::mem::take(&mut self.primary_preedit_clear_pending) {
            self.primary_preedit_active = false;
            self.app.floating.pending_ime_ids.clear();
            self.set_floating_ime_continuation(None);
        }
        let mut cleared_return = false;
        for surface in &mut self.floating_windows {
            if std::mem::take(&mut surface.preedit_clear_pending) {
                surface.preedit_active = false;
                if self.floating_ime_return_continuation.as_deref() == Some(surface.id.as_str()) {
                    apply_native_ime(&surface.window, None, &mut surface.ime_state);
                    cleared_return = true;
                }
            }
        }
        if cleared_return {
            self.floating_ime_return_continuation = None;
        }
        #[cfg(any(target_os = "macos", windows))]
        self.refresh_host_chrome();
        sync_safe_area(&mut self.app, &window);
        self.poll_touch_hold(&window);
        // Conservative shared-root fallback: hidden primary work may stay at
        // 60Hz while a visible auxiliary depends on its update/sync projection.
        // Only all-known-occluded surfaces receive full idle backoff.
        if self.visible_auxiliary_needs_shared_root() {
            self.primary_retry.keep_shared_cadence(crate::time::now());
        }
        // Once admission defers, periodic animation/startup demand must not
        // spin redraw events against the busy worker. Keep updating dirty state;
        // the worker's armed completion callback requests the next paint.
        let mut auxiliary_retry_at: Option<Instant> = None;
        for (aux_window, render) in self
            .floating_windows
            .iter()
            .filter(|surface| !surface.lifecycle.suspended)
            .map(|surface| (&surface.window, surface.render.as_ref()))
            .chain(
                self.audience_presentation
                    .iter()
                    .map(|audience| (&audience.window, audience.render.as_ref())),
            )
        {
            if let Some(deadline) = render.and_then(|render| render.retry.deadline()) {
                auxiliary_retry_at =
                    Some(auxiliary_retry_at.map_or(deadline, |prior| prior.min(deadline)));
                if deadline <= crate::time::now() && auxiliary_redraw_allowed(render) {
                    aux_window.request_redraw();
                }
            }
        }
        let auxiliary_retry_due =
            auxiliary_retry_at.is_some_and(|deadline| deadline <= crate::time::now());
        let retry_now = crate::time::now();
        let primary_blocked_until = self
            .primary_retry
            .occlusion_blocked(retry_now)
            .then(|| self.primary_retry.deadline())
            .flatten();
        let any_render_available = primary_blocked_until.is_none()
            || self.floating_windows.iter().any(|surface| {
                !surface.lifecycle.suspended
                    && surface
                        .render
                        .as_ref()
                        .is_none_or(|render| !render.retry.occlusion_blocked(retry_now))
            })
            || self.audience_presentation.as_ref().is_some_and(|audience| {
                audience
                    .render
                    .as_ref()
                    .is_none_or(|render| !render.retry.occlusion_blocked(retry_now))
            });
        let retry_at = self.primary_retry.deadline();
        let retry_due = retry_at.is_some_and(|deadline| deadline <= crate::time::now());
        let redraw_deferred = self
            .render
            .as_ref()
            .is_some_and(|render| render.buffers.redraw_deferred())
            || retry_at.is_some_and(|deadline| crate::time::now() < deadline);
        let request_primary_redraw = || {
            if !redraw_deferred {
                window.request_redraw();
            }
        };
        if retry_due {
            request_primary_redraw();
        }
        if self.last_primary_frame_presented > 0
            && let Some(scenario) = self.native_scenario.take()
        {
            crate::native_scenario::log::emit_json(
                "tilo-native-scenario",
                serde_json::json!({
                    "name": scenario.name, "steps": scenario.steps.len(),
                    "warmup_steps": scenario.warmup_steps, "interval_us": scenario.interval_us,
                    "event": "started_after_first_worker_presentation",
                }),
            );
            let proxy = self.a11y_proxy.clone();
            if let Err(error) = crate::native_scenario::spawn(scenario, move |packet| {
                proxy
                    .send_event(UserEvent::NativeScenarioInput(packet))
                    .is_ok()
            }) {
                crate::native_scenario::log::emit_json(
                    "tilo-native-scenario",
                    serde_json::json!({"event":"failed", "error":error}),
                );
                self.closing.set(true);
            }
        }

        if self.app.open_requested_palette() {
            request_primary_redraw();
        }
        if self.app.pump_floating_surface_service() {
            self.app.invalidate_widget_resource_cache();
            self.app.layout.layout_dirty = true;
            request_primary_redraw();
        }
        if self.apply_floating_host_messages(event_loop) {
            self.app.invalidate_widget_resource_cache();
            request_primary_redraw();
        }
        // PrepareSurface moves the same editor's IME intent into its exact
        // source continuation. Apply that host lifecycle before comparing the
        // primary window's intent, so an already-received handoff cannot emit
        // a disable for the intermediate attached-widget projection.
        self.sync_ime(&window);
        if self.private_input.pending() {
            request_primary_redraw();
        }
        self.sync_native_floating_geometry();
        if self.recover_stalled_floating_windows() {
            request_primary_redraw();
        }
        // `RedrawRequested` polls the asynchronous renderer setup. The shell
        // keeps showing its live transfer proxy in the meantime, so these
        // frames are cheap but essential: the first real buffer should take
        // ownership as soon as it is ready, without waiting for fresh input.
        for surface in &self.floating_windows {
            if !surface.lifecycle.suspended
                && !surface.lifecycle.adopted
                && auxiliary_redraw_allowed(surface.render.as_ref())
            {
                surface.window.request_redraw();
            }
        }
        // No redraw is pumped while the first frame is in flight: drawing
        // it again every loop turn spun the main thread through ~50 full
        // draws per launch while the worker rasterized. A first submission
        // that comes back `Reconfigured` or `Skipped` (racing the
        // compositor's first configure) wakes this window by itself
        // (`render_thread::notify_render_completion`).
        // This runs before the ordinary fullscreen reconciliation below. If a
        // real second output appears, the root is notified first and keeps
        // the local window as the presenter dashboard; if it disappears the
        // existing fullscreen path immediately becomes the safe fallback.
        self.sync_audience_presentation(event_loop, &window);
        if let Some(audience) = self.audience_presentation.as_ref()
            && audience.render_init.is_some()
        {
            // Poll the worker result without ever blocking the presenter
            // event loop; this is short-lived only while GPU setup runs.
            audience.window.request_redraw();
        }
        let force_theme = self.settings_store_changed.swap(false, Ordering::AcqRel);
        let app = &mut self.app;
        if force_theme {
            if let Some(due) = crate::time::now().checked_sub(std::time::Duration::from_millis(750))
            {
                app.last_theme_store_check = due;
            }
        }

        // Catches a compositor reconfigure that changes `inner_size()`
        // without delivering `Resized`.
        if let Some(prev) = app.layout.last_frame_size
            && window.inner_size() != prev
        {
            app.layout.layout_dirty = true;
            request_primary_redraw();
        }
        if app.pump_collab_presence() {
            request_primary_redraw();
        }
        if app.presence_ctx.prune_interactions(crate::time::now()) {
            request_primary_redraw();
        }
        if app.poll_model_invocations() {
            request_primary_redraw();
        }
        // Reveal requests must not wait on rendering an occluded/minimized
        // window: consume them as soon as its admitted actions are polled.
        if crate::component_lifecycle::take_window_activation_intent(app.root_widget.as_ref()) {
            window.set_minimized(false);
            window.focus_window();
            window.request_user_attention(Some(winit::window::UserAttentionType::Informational));
            tracing::debug!(target: "tilo.app.activation", "existing native window activation requested");
        }
        app.publish_playback_handoff();

        // A "just typed" highlight fades in steps, each a wake deadline: a
        // step that came due paints once, and the loop sleeps between them
        // instead of repainting the window at every refresh while a peer
        // types.
        let typing_now = crate::time::now();
        app.presence_ctx.prune_typing(typing_now);
        let typing_step_due = self.typing_step_at.is_some_and(|due| due <= typing_now);
        if typing_step_due {
            request_primary_redraw();
        }
        self.typing_step_at = app.presence_ctx.typing_next_step(typing_now);
        let typing_active = typing_step_due;

        let idle_model = app.service_native_idle_model();
        if idle_model.changed {
            request_primary_redraw();
        }

        if app.reconcile_fullscreen(&window) {
            request_primary_redraw();
        }

        // Run both: Settings' open Appearance pane may intentionally override
        // the file-level base with its richer live accessibility intent.
        let configured_changed = app.reconcile_configured_theme();
        // A settings change re-themes from the file; outside Tilo OS the host
        // appearance takes over again.
        let host_changed = self
            .host_appearance
            .as_mut()
            .and_then(|host| host.take_change(configured_changed))
            .is_some_and(|appearance| app.apply_host_appearance(appearance));
        let appearance_changed = app.reconcile_appearance();
        if configured_changed || host_changed || appearance_changed {
            app.needs_sync = true;
            request_primary_redraw();
        }

        // The active page/view can change during `sync` (e.g. the initial
        // auto-select), which the post-event broadcast above wouldn't catch.
        app.broadcast_location_presence();
        app.broadcast_view_presence();

        // Run here too, not only `RedrawRequested`, since painting isn't
        // guaranteed — a background tab, or a first frame behind a shader
        // compile.
        if app.record_tick() {
            request_primary_redraw();
        }

        // Without this a first peer with no input never re-syncs to seed its
        // doc after the grace window, so a late joiner diverges.
        if app.collaboration.manager.is_connected() && app.root_widget.needs_idle_sync() {
            app.needs_sync = true;
            request_primary_redraw();
        }

        app.broadcast_doc_delta();

        // Polling a view's topology can itself replace its widgets (Switch
        // reconciles here). Retire the old node pointers before another
        // input event can query a cursor or tooltip, even before drawing.
        if app.forget_replaced_widgets() {
            request_primary_redraw();
        }

        app.drain_local_input_capture();
        let scheduled_deadline = crate::close_admission::wake_deadline(
            app.root_widget
                .next_wake_deadline()
                .into_iter()
                .chain(self.touch_pointer.long_press_deadline())
                .chain(app.local_input_capture_deadline())
                .chain(
                    app.presence_ctx
                        .interaction_next_wake_deadline(crate::time::now()),
                )
                .chain(app.accessibility.zoom.deadline())
                .min(),
            app.close_admission.save_waiting,
            crate::time::now(),
        );
        let scheduled_due =
            scheduled_deadline.is_some_and(|deadline| deadline <= crate::time::now());
        if scheduled_due {
            // A due deadline means "run sync" (poll a directory, settle
            // spellcheck), not "commit a buffer". Files' 750ms metadata
            // poll found nothing and still presented at ~2 Hz because
            // `needs_sync` forced a full redraw.
            let updated = app.update_native_model();
            // A deadline of the host's own (a long press, an input capture,
            // presence) syncs; one the widgets asked for syncs only when
            // serving it changed something ([`Widget::serve_deadline`]): a
            // directory poll that found nothing reads nothing again and
            // keeps the published tree.
            let now = crate::time::now();
            let host_due = self
                .touch_pointer
                .long_press_deadline()
                .into_iter()
                .chain(app.local_input_capture_deadline())
                .chain(app.presence_ctx.interaction_next_wake_deadline(now))
                .chain(app.accessibility.zoom.deadline())
                .any(|due| due <= now);
            app.accessibility.zoom.publish();
            let widget_due = app
                .root_widget
                .next_wake_deadline()
                .is_some_and(|due| due <= now);
            if updated || host_due || !widget_due || app.root_widget.serve_deadline(now) {
                let _ = app.sync_model(true);
            }
            let structure = app.forget_replaced_widgets();
            if present_after_deadline_sync(
                updated || structure || app.layout.layout_dirty,
                app.root_widget.needs_animation_frame(),
            ) {
                request_primary_redraw();
            }
        }

        // The poll above already ran. Re-read what it published so a deadline
        // that is still in the past does not schedule a frame-rate follow-up.
        let scheduled_deadline = crate::close_admission::wake_deadline(
            app.root_widget
                .next_wake_deadline()
                .into_iter()
                .chain(self.touch_pointer.long_press_deadline())
                .chain(app.local_input_capture_deadline())
                .chain(
                    app.presence_ctx
                        .interaction_next_wake_deadline(crate::time::now()),
                )
                .chain(app.accessibility.zoom.deadline())
                .min(),
            app.close_admission.save_waiting,
            crate::time::now(),
        );
        let widget_still_due = !scheduled_due
            && scheduled_deadline.is_some_and(|deadline| deadline <= crate::time::now());

        // A hover tooltip is not motion, so it reveals even under
        // reduce-motion. Its dwell is a wait: the loop sleeps until the
        // reveal is due (`scheduled_deadline`) and paints once then.
        let tooltip_reveal = app.input.tooltip_host.reveal_at();
        let tooltip_pending = tooltip_reveal.is_some_and(|at| at <= crate::motion::frame_now());
        if tooltip_pending {
            request_primary_redraw();
        }
        // A frame held for a local read is drawn once its budget runs out.
        let held_due = self
            .held_frame
            .is_some_and(|until| until <= crate::time::now());
        if held_due {
            self.held_frame = None;
            request_primary_redraw();
        }
        let scheduled_deadline = scheduled_deadline
            .into_iter()
            .chain(tooltip_reveal.filter(|_| !tooltip_pending))
            .chain(self.typing_step_at)
            .chain(self.held_frame)
            .min();

        // A widget asking for a frame is served whatever the motion setting.
        // Reduced motion is applied where motion is decided — each widget
        // reads `Theme::motion()` and settles instead of asking — never by
        // starving the pump: a fade that replaces a slide still needs its
        // frames, and gating here froze every reduced-motion app that waited
        // on one.
        // A motion started outside the polled tree (a menu its owner draws in
        // its own overlay pass) asks through the motion clock.
        let widget_animation = app.root_widget.needs_animation_frame()
            || self.slot_bar.needs_frame()
            || crate::motion::frames_wanted(crate::time::now());
        let needs_frame = widget_animation
            || app
                .presence_ctx
                .interaction_animation_demand(crate::time::now(), &app.theme);
        let animate = any_render_available && needs_frame;
        if animate && self.animation_schedule.idle() {
            // An animation starts: beat at the refresh of the output the
            // window is on now.
            self.animation_schedule.set_refresh_millihertz(
                window
                    .current_monitor()
                    .and_then(|monitor| monitor.refresh_rate_millihertz()),
            );
        }
        let (animation_due, animation_deadline) =
            self.animation_schedule.poll(crate::time::now(), animate);
        if animation_due {
            // An animation frame advances what moves in the frame's update
            // (`update_native_model`), which syncs, and drops the published
            // resources, only when a widget reports that its model changed.
            // A glide, a fade or a slide changes nothing the app publishes:
            // forcing a sync here rebuilt and republished the whole resource
            // tree on every frame of every animation.
            // Forces `apply_layout` since an animation tick (e.g. momentum
            // fling) can reposition content without an input event.
            app.layout.layout_dirty |= widget_animation;
            request_primary_redraw();
            for surface in &self.floating_windows {
                if auxiliary_redraw_allowed(surface.render.as_ref()) {
                    surface.window.request_redraw();
                }
            }
            if let Some(audience) = self.audience_presentation.as_ref()
                && auxiliary_redraw_allowed(audience.render.as_ref())
            {
                audience.window.request_redraw();
            }
        }

        // A background pump can run while its visible projection is stable.
        // Redrawing only on a changed snapshot avoids full-scene GPU work on
        // every otherwise-idle collaboration wake.
        if let Some(tick) = app.presence.frame_tick.as_mut()
            && tick(&mut app.presence_ctx)
        {
            request_primary_redraw();
        }

        // `Wait` sleeps until real input, zero CPU; escalate to a bounded
        // ~60 Hz tick only while a root still wants another frame. A live
        // collaboration connection is not such a reason: its transport wakes
        // this loop for each inbound event, directory request and freed
        // outbound slot.
        let awaiting_floating_pixels = self.floating_windows.iter().any(|surface| {
            !surface.lifecycle.suspended
                && !surface.lifecycle.adopted
                && surface
                    .render
                    .as_ref()
                    .is_none_or(|render| !render.retry.occlusion_blocked(crate::time::now()))
        });
        let retry_at = retry_at.into_iter().chain(auxiliary_retry_at).min();
        // Widget deadlines are serviced by sync/redraw. While occluded they can
        // be delayed by at most the 250ms probe cap; independent history, record,
        // autosave and collaboration work above continues on its existing doors.
        let demand = crate::render_thread::retry::WakeDemand {
            typing: typing_active,
            tooltip: tooltip_pending,
            widget_due: widget_still_due,
            animation: needs_frame,
            auxiliary_startup: awaiting_floating_pixels,
            retry_due: retry_due || auxiliary_retry_due,
            frame_interval: self.animation_schedule.interval(),
        };
        let deadline = crate::render_thread::retry::next_wake(
            crate::time::now(),
            primary_blocked_until,
            any_render_available,
            demand,
            scheduled_deadline,
            retry_at,
            animation_deadline,
        );
        // A native field lease can retire and become ready during an idle sync.
        // Republish its admission even when no model/layout change needs paint;
        // otherwise a canceled recording's successor waits on a stale stamp.
        if scheduled_due {
            self.sync_ime(&window);
        }
        let deadline = deadline
            .into_iter()
            .chain(
                self.private_input
                    .busy()
                    .then(|| crate::time::now() + Duration::from_millis(50)),
            )
            .min();
        match deadline {
            Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

impl AppHandler {
    fn handle_audience_window_event(&mut self, event: WindowEvent) {
        let Some(audience_window) = self
            .audience_presentation
            .as_ref()
            .map(|audience| audience.window.clone())
        else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => {
                self.app.root_widget.on_audience_presentation_closed();
                self.close_audience_presentation();
                if let Some(primary) = self.window.as_ref() {
                    primary.request_redraw();
                }
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                audience_window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if !self.handle_audience_redraw() {
                    self.close_audience_presentation();
                    if let Some(primary) = self.window.as_ref() {
                        primary.request_redraw();
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.app.input.modifiers = modifiers,
            other => {
                let handled = self.app.root_widget.handle_audience_presentation_event(
                    &other,
                    &mut crate::widget::Edits::lend(&mut self.app.state.doc),
                    &self.app.state.root,
                );
                if handled {
                    self.app.commit_action();
                    self.app.needs_sync = true;
                    self.app.layout.layout_dirty = true;
                    if let Some(primary) = self.window.as_ref() {
                        primary.request_redraw();
                    }
                    audience_window.request_redraw();
                }
            }
        }
    }

    fn handle_secondary_accessibility_event(
        &mut self,
        window_id: winit::window::WindowId,
        event: accesskit_winit::WindowEvent,
    ) {
        let Some(index) = self
            .floating_windows
            .iter()
            .position(|surface| surface.window.id() == window_id)
        else {
            return;
        };
        let Some(surface) = self.floating_windows.get(index) else {
            return;
        };
        if surface.lifecycle.suspended {
            return;
        }
        let id = surface.id.clone();
        let origin = surface.desktop_rect.origin();
        let scale = surface.window.scale_factor();
        let access = self.app.root_widget.floating_surface_accessibility(&id);
        match event {
            accesskit_winit::WindowEvent::InitialTreeRequested => {
                if let Some(surface) = self.floating_windows.get_mut(index) {
                    if let Some(adapter) = surface.a11y_adapter.as_mut() {
                        adapter.update_if_active(|| {
                            crate::accesskit_bridge::native_tree_update(
                                crate::accesskit_bridge::surface_tree_update(
                                    access.as_ref(),
                                    origin,
                                    scale,
                                ),
                            )
                        });
                    }
                    surface.window.request_redraw();
                }
            }
            accesskit_winit::WindowEvent::ActionRequested(request) => {
                crate::motion::note_source(crate::tokens::MotionSource::Action);
                if dispatch_native_surface_accessibility(
                    self.app.root_widget.as_mut(),
                    &id,
                    &request,
                    &mut crate::widget::Edits::lend(&mut self.app.state.doc),
                    &self.app.state.root,
                ) {
                    self.app.invalidate_widget_resource_cache();
                    self.app.commit_action();
                    self.app.needs_sync = true;
                    if let Some(primary) = self.window.as_ref() {
                        primary.request_redraw();
                    }
                    if let Some(surface) = self.floating_windows.get(index) {
                        surface.window.request_redraw();
                    }
                }
            }
            accesskit_winit::WindowEvent::AccessibilityDeactivated => {}
        }
    }

    fn update_floating_preedit(&mut self, index: usize, update: PrimaryPreeditUpdate) {
        let Some(surface) = self.floating_windows.get_mut(index) else {
            return;
        };
        match update {
            PrimaryPreeditUpdate::Active => {
                surface.preedit_active = true;
                surface.preedit_clear_pending = false;
            }
            PrimaryPreeditUpdate::PendingClear => surface.preedit_clear_pending = true,
            PrimaryPreeditUpdate::Finished => {
                surface.preedit_active = false;
                surface.preedit_clear_pending = false;
            }
        }
    }

    fn handle_native_floating_window_event(&mut self, index: usize, event: WindowEvent) {
        self.app.root_widget.on_floating_surface_event_source(None);
        let Some(surface) = self.floating_windows.get(index) else {
            return;
        };
        let surface_id = surface.id.clone();
        let reverse = surface.lifecycle.suspended
            && self.floating_ime_return_continuation.as_deref() == Some(surface_id.as_str());
        if surface.lifecycle.suspended && !reverse {
            return;
        }
        if reverse {
            if matches!(event, WindowEvent::Focused(false)) {
                self.floating_ime_return_continuation = None;
                if let Some(surface) = self.floating_windows.get_mut(index) {
                    surface.preedit_active = false;
                    surface.preedit_clear_pending = false;
                    apply_native_ime(&surface.window, None, &mut surface.ime_state);
                }
                return;
            }
            if !matches!(
                event,
                WindowEvent::KeyboardInput { .. }
                    | WindowEvent::Ime(_)
                    | WindowEvent::ModifiersChanged(_)
            ) {
                return;
            }
            let preedit = preedit_after_event(&event);
            if let Some(primary) = self.window.clone() {
                self.handle_primary_window_event_routed(&primary, event);
            }
            if let Some(update) = preedit {
                self.update_floating_preedit(index, update);
                if matches!(update, PrimaryPreeditUpdate::Finished) {
                    self.floating_ime_return_continuation = None;
                }
            }
            let exact_source_active =
                self.floating_ime_return_continuation
                    .as_deref()
                    .is_some_and(|id| id == surface_id.as_str())
                    && self.floating_windows.get(index).is_some_and(|surface| {
                        surface.lifecycle.suspended && surface.preedit_active
                    });
            let projected = if exact_source_active {
                self.floating_windows.get(index).and_then(|surface| {
                    primary_ime_state_for_held_child(
                        self.app.ime_window_state(),
                        surface.coordinate_offset,
                        surface.desktop_rect.origin().to_vec2(),
                    )
                })
            } else {
                None
            };
            if let Some(surface) = self.floating_windows.get_mut(index) {
                let next =
                    retained_return_ime_state(projected, surface.ime_state, exact_source_active);
                apply_native_ime(&surface.window, next, &mut surface.ime_state);
            }
            return;
        }
        let window = surface.window.clone();
        let desktop_rect = surface.desktop_rect;
        self.app
            .root_widget
            .on_floating_surface_event_source(Some(&surface_id));
        if let Some(surface) = self.floating_windows.get_mut(index) {
            if let WindowEvent::Focused(focused) = &event {
                surface.lifecycle.focused = *focused;
            }
            if let Some(adapter) = surface.a11y_adapter.as_mut() {
                adapter.process_event(&window, &event);
            }
        }
        forward_native_surface_focus(self.app.root_widget.as_mut(), Some(&surface_id), &event);
        let preedit = preedit_after_event(&event);
        match event {
            WindowEvent::CloseRequested => {
                if let Some(client) = self.app.floating.client.as_mut() {
                    let _ = client.send(
                        &crate::floating_surface::FloatingSurfaceClientMessage::Dismiss {
                            id: surface_id.clone(),
                        },
                    );
                }
                self.close_native_floating_window(&surface_id);
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if !self.handle_floating_redraw(index)
                    && let Some(id) = self
                        .floating_windows
                        .get(index)
                        .map(|surface| surface.id.clone())
                {
                    self.close_native_floating_window(&id);
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.app.input.modifiers = modifiers,
            other => self.dispatch_floating_event(&window, desktop_rect, other),
        }
        if let Some(update) = preedit {
            self.update_floating_preedit(index, update);
        }
        self.app.root_widget.on_floating_surface_event_source(None);
        if let Some(surface) = self
            .floating_windows
            .iter_mut()
            .find(|surface| surface.id == surface_id)
        {
            let next = secondary_ime_state(
                self.app.root_widget.as_ref(),
                &surface.id,
                surface.desktop_rect,
                surface.lifecycle.focused,
            );
            apply_native_ime(&surface.window, next, &mut surface.ime_state);
        }
    }

    fn handle_primary_window_event(&mut self, window: &Arc<Window>, event: WindowEvent) {
        if matches!(event, WindowEvent::Focused(false)) {
            self.app.floating.pending_ime_ids.clear();
            // The compositor deliberately keeps the source protocol focus
            // only across the exact Float handoff. A real unrelated focus
            // loss retires the local routing proxy immediately; a later IME
            // event must not reach the detached editor by stale surface id.
            self.set_floating_ime_continuation(None);
            self.primary_preedit_active = false;
            self.primary_preedit_clear_pending = false;
        }
        let routed_surface = self
            .floating_ime_continuation
            .as_deref()
            .filter(|_| routes_to_floating_ime(&event))
            .map(str::to_owned);
        self.app
            .root_widget
            .on_floating_surface_event_source(routed_surface.as_deref());
        let preedit = preedit_after_event(&event);
        self.handle_primary_window_event_routed(window, event);
        self.app.root_widget.on_floating_surface_event_source(None);
        if let Some(update) = preedit {
            match update {
                PrimaryPreeditUpdate::Active => {
                    self.primary_preedit_active = true;
                    self.primary_preedit_clear_pending = false;
                }
                PrimaryPreeditUpdate::PendingClear => {
                    self.primary_preedit_clear_pending = true;
                }
                PrimaryPreeditUpdate::Finished => {
                    self.primary_preedit_active = false;
                    self.primary_preedit_clear_pending = false;
                }
            }
            if matches!(update, PrimaryPreeditUpdate::Finished) {
                self.app.floating.pending_ime_ids.clear();
                self.set_floating_ime_continuation(None);
            }
        }
        if let Some(id) = routed_surface
            && let Some(surface) = self
                .floating_windows
                .iter()
                .find(|surface| surface.id == id)
        {
            surface.window.request_redraw();
        }
    }

    fn handle_primary_window_event_routed(&mut self, window: &Arc<Window>, event: WindowEvent) {
        forward_native_surface_focus(self.app.root_widget.as_mut(), None, &event);
        if let Some(adapter) = self.a11y_adapter.as_mut() {
            adapter.process_event(window, &event);
        }
        #[cfg(target_os = "linux")]
        {
            let routed = self.decorations.route(&mut self.app, window, &event);
            if routed.close {
                self.close_primary_window(window);
            }
            if routed.consumed {
                return;
            }
        }
        if self.slot_bar.route(&mut self.app, window, &event) {
            return;
        }
        self.dispatch_touch_pointer_events(window, &event);
        match event {
            WindowEvent::CloseRequested => self.close_primary_window(window),
            WindowEvent::Resized(_) => {
                self.app.layout.layout_dirty = true;
                window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                crate::widget::mount_native_views(
                    self.app.root_widget.as_mut(),
                    &crate::widget::NativeViewHost::live(window, self.app.input.platform),
                );
                self.app.layout.layout_dirty = true;
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => self.handle_redraw_requested(window),
            WindowEvent::ModifiersChanged(modifiers) => self.app.input.modifiers = modifiers,
            other if self.app.input.platform.platform() == crate::platform::Platform::Android => {
                if let Some(escape) = crate::interaction::back_key_as_escape(&other) {
                    self.handle_back_key(window, escape);
                } else {
                    self.handle_other_window_event(window, other);
                }
            }
            other => {
                self.handle_other_window_event(window, other);
            }
        }
    }

    fn close_primary_window(&mut self, window: &Arc<Window>) {
        match crate::component_lifecycle::on_window_close_requested(self.app.root_widget.as_mut()) {
            crate::widget::WindowCloseAdmission::Allow => {
                if self.app.admit_native_close_request() {
                    self.closing.set(true);
                } else {
                    window.request_redraw();
                }
            }
            crate::widget::WindowCloseAdmission::Defer => {
                self.app.needs_sync = true;
                self.app.layout.layout_dirty = true;
                window.request_redraw();
            }
        }
    }

    /// Back dismisses what Escape would, then navigates back, and at the root
    /// closes the app; exiting the event loop lets Android finish the activity.
    fn handle_back_key(&mut self, window: &Arc<Window>, escape: WindowEvent) {
        let pressed = matches!(
            &escape,
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed
        );
        if self.handle_other_window_event(window, escape) || !pressed {
            return;
        }
        let app = &mut self.app;
        if app.root_widget.on_nav_gesture(
            crate::gesture::NavDirection::Back,
            &app.state.doc,
            &app.state.root,
        ) {
            app.needs_sync = true;
            window.request_redraw();
            return;
        }
        self.close_primary_window(window);
    }

    fn dispatch_touch_pointer_events(&mut self, window: &Arc<Window>, event: &WindowEvent) {
        let WindowEvent::Touch(touch) = event else {
            return;
        };
        self.app
            .set_interaction_environment(self.app.input.interaction.with_touch_modality());
        let scale = window.scale_factor();
        let app = &self.app;
        let pointer_events = self.touch_pointer.events_claimed(touch, |position| {
            app.touch_claim_at(vello::kurbo::Point::new(
                position.x / scale,
                position.y / scale,
            ))
        });
        self.dispatch_touch_pointer(window, touch.device_id, pointer_events);
    }

    fn dispatch_touch_pointer(
        &mut self,
        window: &Arc<Window>,
        device_id: winit::event::DeviceId,
        events: Vec<crate::interaction::TouchPointerEvent>,
    ) {
        self.dispatching_touch_pointer = true;
        for pointer_event in events {
            self.handle_other_window_event(window, pointer_event.window_event(device_id));
        }
        self.dispatching_touch_pointer = false;
    }

    /// A still finger that reached the long-press delay becomes a secondary
    /// click even though no touch event marks the moment.
    fn poll_touch_hold(&mut self, window: &Arc<Window>) {
        if let Some((device_id, events)) = self.touch_pointer.poll(crate::time::now()) {
            self.dispatch_touch_pointer(window, device_id, events);
            window.request_redraw();
        }
    }
}

impl App {
    /// Run this application on the platform event loop.
    pub fn run(self) {
        self.run_with_setup(|_| {});
    }

    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn with_startup_renderer(
        mut self,
        renderer: Option<startup::RendererPreparation>,
    ) -> Self {
        self.startup_renderer = renderer;
        self
    }

    /// Like [`App::run`] but exposes the winit [`EventLoopProxy`] before the
    /// loop starts, so callers can spawn threads that inject [`UserEvent`]s.
    ///
    /// # Panics
    ///
    /// Panics if the platform windowing system can't create the event loop.
    /// Once running, a Wayland compositor disconnect is a normal application
    /// shutdown boundary (session logout, compositor restart), so an
    /// `ExitFailure` is logged and returned rather than converted into a panic.
    #[allow(
        clippy::expect_used,
        clippy::unwrap_used,
        reason = "event-loop creation/run failure is unrecoverable at process startup; the public run()/run_with_setup() signature is called from every app's main() and returning Result here would ripple across the whole app surface for a failure with no fallback"
    )]
    pub fn run_with_setup<F>(self, setup: F)
    where
        F: FnOnce(EventLoopProxy<UserEvent>),
    {
        self.run_windows(setup, None);
    }

    /// [`Self::run_with_setup`], serving the windows the shell opens in this
    /// process later with `open` ([`crate::window_door`]): each becomes one
    /// more window of the loop, and the loop ends with the last.
    #[allow(
        clippy::expect_used,
        clippy::unwrap_used,
        reason = "event-loop creation/run failure is unrecoverable at process startup; the public run()/run_with_setup() signature is called from every app's main() and returning Result here would ripple across the whole app surface for a failure with no fallback"
    )]
    pub(crate) fn run_windows<F>(
        mut self,
        setup: F,
        open: Option<&mut process_windows::OpenWindow<'_>>,
    ) where
        F: FnOnce(EventLoopProxy<UserEvent>),
    {
        assert!(
            self.wake_scope.is_some(),
            "live App requires from_factory, try_from_factory, or from_scoped_parts"
        );
        // Everything from here to the window's creation is marked: Notes spent
        // 259 ms between `app-construct-end` and `primary-window-created`
        // with nothing in the startup file to say where.
        tilo_startup_metrics::mark("run-start");
        self.prepare_native_presentation(crate::host_chrome::HostChrome::current(
            self.input.platform,
        ));
        tilo_startup_metrics::mark("run-presentation-prepared");
        crate::host::native::install_default_if_unset(self.window.app_id.as_deref());
        // The event loop never waits on async work: anything that must is
        // spawned onto the app runtime and handed back through the wake scope.
        tilo_rt::mark_ui_thread();
        let mut event_loop_builder = EventLoop::<UserEvent>::with_user_event();
        #[cfg(target_os = "macos")]
        if crate::host::environment().diagnostics.window_no_activate {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            // Native scripted input does not need to take the user's keyboard.
            // Keep normal launches unchanged; this is an opt-in profiling mode.
            event_loop_builder.with_activate_ignoring_other_apps(false);
        }
        #[cfg(target_os = "android")]
        {
            use winit::platform::android::EventLoopBuilderExtAndroid;
            event_loop_builder.with_android_app(
                crate::launch::android::app()
                    .expect("an Android app starts through ui::android_main!")
                    .clone(),
            );
        }
        tilo_startup_metrics::mark("run-event-loop-build");
        // The launch may have built it already, to make the window early.
        let event_loop =
            take_early_event_loop().unwrap_or_else(|| event_loop_builder.build().unwrap());
        tilo_startup_metrics::mark("run-event-loop-built");
        tilo_host_search::forward_to_runtime();
        let proxy = event_loop.create_proxy();
        super::install_dev_session_stop(proxy.clone());
        #[cfg(target_os = "android")]
        super::android_text_input::bridge::install(proxy.clone());
        #[allow(
            clippy::panic,
            reason = "invalid explicitly requested benchmark input must stop startup rather than silently run an unmeasured scenario"
        )]
        let native_scenario = crate::native_scenario::load_from_env()
            .unwrap_or_else(|error| panic!("invalid native performance scenario: {error}"));
        assert!(
            native_scenario.is_none() || self.profiler.is_some(),
            "native performance scenarios require TILO_PROFILE_OUT or TILO_PROFILE"
        );
        let handler = self.into_handler(&proxy, native_scenario, setup);
        let door = open.and_then(|open| process_windows::serve_door(&proxy).map(|()| open));
        let mut handler = process_windows::ProcessWindows::new(handler, proxy, door);
        // Run on demand, so a window that gives its place to the next one
        // (`hand_over`) keeps the loop for that window to run on. On every
        // target winit's `run_app` is this same loop, consuming it.
        let (ended, event_loop) = {
            use winit::platform::run_on_demand::EventLoopExtRunOnDemand as _;
            let mut event_loop = event_loop;
            (event_loop.run_app_on_demand(&mut handler), event_loop)
        };
        match ended {
            Ok(()) => {}
            Err(winit::error::EventLoopError::ExitFailure(status)) => {
                tracing::debug!(status, "window system disconnected; application exiting");
            }
            Err(error) => {
                tracing::error!(%error, "application event loop failed; application exiting");
            }
        }
        let mut handler = handler.into_first();
        let handing_over = handler.as_ref().is_some_and(|handler| handler.handing_over);
        if handing_over && let Some(handler) = handler.as_mut() {
            hand_over(handler, event_loop);
        }
        // The app and its state are dropped (and saved) before the process
        // reports how its window ended, and so are the settings it changed.
        drop(handler);
        crate::storage::flush_settings_writes();
        if !handing_over && let Some(reason) = take_abnormal_stop() {
            report_window_lost_and_exit(&reason);
        }
    }

    /// The window this app is shown in, on the loop `proxy` wakes: its wake
    /// scope bound, its settings watched, `setup` run and its layout
    /// initialised. Its window itself is made when the loop resumes it.
    #[allow(
        clippy::expect_used,
        clippy::unwrap_used,
        reason = "a live App's wake scope is open and unbound until its window takes it"
    )]
    fn into_handler<F>(
        mut self,
        proxy: &EventLoopProxy<UserEvent>,
        native_scenario: Option<crate::native_scenario::Scenario>,
        setup: F,
    ) -> AppHandler
    where
        F: FnOnce(EventLoopProxy<UserEvent>),
    {
        let wake_proxy = proxy.clone();
        self.wake_scope
            .as_mut()
            .unwrap()
            .bind(move |event| {
                let _ = wake_proxy.send_event(UserEvent::Wake(event));
            })
            .expect("App wake scope must be open and unbound at native startup");
        // The AccessKit adapter routes events back through this as
        // `UserEvent::AccessKit`.
        let a11y_proxy = proxy.clone();
        let settings_store_changed = Arc::new(AtomicBool::new(false));
        let settings_watch = {
            let _entry = self.enter_wake_scope();
            self.watch_system_settings
                .then(|| {
                    let pending = Arc::clone(&settings_store_changed);
                    let wake = crate::wake::WakeHandle::new();
                    settings_store::watch::watch_settings(move || {
                        pending.store(true, Ordering::Release);
                        wake.wake();
                    })
                    .ok()
                })
                .flatten()
        };
        tilo_startup_metrics::mark("run-settings-watched");
        let host_appearance = {
            let _entry = self.enter_wake_scope();
            setup(proxy.clone());
            tilo_startup_metrics::mark("run-setup-done");
            self.init_layout();
            tilo_startup_metrics::mark("run-init-layout-done");
            self.watch_system_settings
                .then(crate::host_appearance::Source::start)
                .flatten()
        };
        tilo_startup_metrics::mark("run-host-appearance-started");

        AppHandler {
            app: self,
            clipboard_attachment: None,
            window: None,
            render: None,
            surface_suspended: false,
            suspended_renderer: None,
            audience_presentation: None,
            floating_windows: Vec::new(),
            a11y_adapter: None,
            a11y_proxy,
            ime_state: None,
            primary_preedit_active: false,
            primary_preedit_clear_pending: false,
            floating_ime_continuation: None,
            floating_ime_return_continuation: None,
            private_input: private_text_input::Bridge::default(),
            wheel_batch: wheel_batch::WheelBatch::default(),
            #[cfg(target_os = "android")]
            text_input: super::android_text_input::TextInputSession::default(),
            #[cfg(target_os = "android")]
            text_input_touched: false,
            primary_frames_sent: 0,
            primary_frames_completed: 0,
            top_layer_drawn: false,
            presence_layer: crate::presence::PresenceLayer::new(),
            mark_bounds: crate::scene_parts::DrawBounds::default(),
            last_primary_frame_presented: 0,
            primary_retry: crate::render_thread::retry::RenderRetryState::default(),
            floating_source_clear_frames: std::collections::HashMap::new(),
            floating_destination_present_frames: std::collections::HashMap::new(),
            touch_pointer: crate::interaction::TouchPointerEmulator::default(),
            dispatching_touch_pointer: false,
            native_scenario,
            native_input_ids: Vec::new(),
            physical_input_trace: crate::physical_input_trace::Trace::when_enabled(
                std::env::var("TILO_PROFILE_PRESENTATION").ok().as_deref(),
            ),
            native_input_origin: None,
            native_input_pointer_passthrough: false,
            native_input_route_isolation: false,
            animation_schedule: animation_schedule::AnimationSchedule::default(),
            typing_step_at: None,
            held_frame: None,
            still_animation: animation_schedule::StillAnimation::default(),
            dev_window_state: std::collections::HashMap::new(),
            dev_loaded_library: crate::host::environment()
                .dev_session
                .as_deref()
                .and_then(dev_reload_library),
            dev_reload_timing: None,
            #[cfg(any(target_os = "macos", windows))]
            host_chrome: None,
            #[cfg(any(target_os = "macos", windows))]
            host_commands_key: None,
            #[cfg(target_os = "linux")]
            decorations: linux_header::Decorations::Unsettled,
            slot_bar: slot_bar::SlotBar::new(),
            host_appearance,
            settings_store_changed,
            _settings_watch: settings_watch,
            handing_over: false,
            closing: std::cell::Cell::new(false),
        }
    }
}

/// Keep what the next window of this process takes up, as the launch made
/// them before the first: the loop, the window (its `resumed` adopts it, so
/// it never leaves the screen) and the shell-ui connection, which the
/// launcher hands each process once. What the window drew with was shut
/// down when the loop exited; the next window draws anew.
fn hand_over(handler: &mut AppHandler, event_loop: EventLoop<UserEvent>) {
    // A window the platform says is shown (X11) cannot take a new
    // accessibility adapter: the next window is made afresh there. A Wayland
    // window never says, and is kept.
    if let Some(window) = handler
        .window
        .take()
        .filter(|window| window.is_visible() != Some(true))
    {
        let app_id = primary_window_app_id(handler.app.window.app_id.as_deref());
        EARLY_WINDOW.with(|slot| *slot.borrow_mut() = Some((app_id, window)));
    }
    if let Some(client) = handler.app.floating.client.take() {
        client.hand_over();
    }
    EARLY_EVENT_LOOP.with(|slot| *slot.borrow_mut() = Some(event_loop));
}

/// Why the window is stopping, when no one asked it to close.
static ABNORMAL_STOP: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Stop the event loop because the window cannot go on, keeping why. The
/// process then exits non-zero instead of looking like a deliberate quit.
fn stop_abnormally(closing: &std::cell::Cell<bool>, reason: String) {
    tracing::error!(%reason, "{}", super::WINDOW_LOST);
    ABNORMAL_STOP.lock_recover().get_or_insert(reason);
    closing.set(true);
}

fn take_abnormal_stop() -> Option<String> {
    ABNORMAL_STOP.lock_recover().take()
}

#[allow(
    clippy::exit,
    reason = "the native app's lifecycle ends here; a window that stopped on \
              its own must not report the clean exit a deliberate quit does"
)]
fn report_window_lost_and_exit(reason: &str) -> ! {
    eprintln!("error: {}: {reason}", super::WINDOW_LOST);
    std::process::exit(super::WINDOW_LOST_EXIT_CODE);
}

impl AppHandler {
    fn note_host_appearance_event(&mut self, event: &WindowEvent) {
        let Some(host) = self.host_appearance.as_mut() else {
            return;
        };
        match event {
            WindowEvent::ThemeChanged(theme) => {
                host.note_window_theme(Some(*theme));
                host.refresh();
            }
            WindowEvent::Focused(true) => host.refresh(),
            _ => {}
        }
    }

    fn apply_host_appearance(&mut self, force: bool) -> bool {
        self.host_appearance
            .as_mut()
            .and_then(|host| host.take_change(force))
            .is_some_and(|appearance| self.app.apply_host_appearance(appearance))
    }
}

/// Follow the system chrome drawn over the window. Android reports it
/// from `TiloActivity`'s insets listener; elsewhere it stays zero.
fn sync_safe_area(app: &mut App, window: &Arc<Window>) {
    #[cfg(target_os = "android")]
    {
        let area = crate::scale::SafeArea::from_physical(
            crate::launch::android::safe_area_insets(),
            window.scale_factor(),
        );
        if area != app.layout.safe_area {
            app.layout.safe_area = area;
            app.layout.layout_dirty = true;
            window.request_redraw();
        }
    }
    #[cfg(not(target_os = "android"))]
    let _ = (app, window);
}

/// A due `next_wake_deadline` runs sync. Present only when that sync
/// changed the model, the tree, or an in-flight animation. `needs_redraw`
/// is not a reason: Files stays `needs_redraw` after `mark_drawn` (closed
/// menus and other default leaves), so treating it as dirty recommits
/// every 750ms metadata poll.
fn present_after_deadline_sync(model_or_layout_dirty: bool, needs_animation: bool) -> bool {
    model_or_layout_dirty || needs_animation
}

#[cfg(test)]
mod deadline_present_tests {
    use super::present_after_deadline_sync;

    #[test]
    fn idle_scheduled_sync_does_not_request_frames() {
        assert!(
            !present_after_deadline_sync(false, false),
            "an idle scheduled poll must not keep requesting frames"
        );
        // Sticky `needs_redraw` after mark_drawn is not an argument: the
        // helper no longer takes that bit. Model/structure/layout OR into
        // the first flag at the call site (keeps clippy's bool limit).
        assert!(present_after_deadline_sync(false, true));
        assert!(present_after_deadline_sync(true, false));
    }
}

#[cfg(test)]
mod early_window_tests {
    use super::{Early, early_for};

    #[test]
    fn an_early_window_is_kept_only_for_the_app_id_it_announces() {
        let made = Some(("so.tilo.notes".to_string(), "window"));
        assert_eq!(
            early_for(made.clone(), &"so.tilo.notes".to_string()),
            Early::Kept("window")
        );
        // An input surface announces its own id, never the package's: the
        // early window must not become its window.
        assert_eq!(
            early_for(made, &"so.tilo.keyboard".to_string()),
            Early::Discarded
        );
        assert_eq!(
            early_for(None::<(String, &str)>, &"so.tilo.notes".to_string()),
            Early::Absent
        );
    }

    #[test]
    fn a_prepared_surface_is_used_only_by_the_window_it_was_made_for() {
        assert_eq!(
            early_for(Some((7_u64, "surface")), &7),
            Early::Kept("surface")
        );
        assert_eq!(early_for(Some((7_u64, "surface")), &8), Early::Discarded);
    }
}
