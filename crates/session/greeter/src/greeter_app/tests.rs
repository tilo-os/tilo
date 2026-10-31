#[test]
fn diagnostic_session_environment_forwards_only_explicit_fixed_booleans() {
    let mut guard = tilo_test_env::ScopedEnv::new();
    guard
        .set("TILO_NATIVE_SCENARIO", "secret-scenario")
        .set("TILO_TEST_CONTROL_FIRST_USE", "mint-identity")
        .set("TILO_PROFILE_PRESENTATION", "0")
        .set("TILO_PROFILE_GPU_STAGES", "true")
        .set("VELLO_GPU_TIMESTAMPS", "1;unexpected")
        .set("VELLO_GPU_DISPATCH_TIMESTAMPS", "0");
    let baseline = GreeterApp::session_env(None);
    assert_eq!(
        baseline,
        vec![
            "XDG_SESSION_TYPE=wayland",
            "XDG_SESSION_DESKTOP=tilo",
            "XDG_CURRENT_DESKTOP=tilo",
        ]
    );
    guard
        .set("TILO_PROFILE_PRESENTATION", "1")
        .set("TILO_PROFILE_GPU_STAGES", "1")
        .set("VELLO_GPU_TIMESTAMPS", "1")
        .set("VELLO_GPU_DISPATCH_TIMESTAMPS", "1");
    let enabled = GreeterApp::session_env(None);
    assert_eq!(&enabled[..3], baseline.as_slice());
    assert_eq!(
        &enabled[3..],
        [
            "TILO_PROFILE_PRESENTATION=1",
            "TILO_PROFILE_GPU_STAGES=1",
            "VELLO_GPU_TIMESTAMPS=1",
            "VELLO_GPU_DISPATCH_TIMESTAMPS=1",
        ]
    );
}

use super::*;
use automerge::{AutoCommit, ReadDoc};
use greetd_ipc::codec::SyncCodec as _;
use greetd_ipc::{AuthMessageType, ErrorType, Request, Response};
use std::collections::HashMap;
use std::os::unix::net::UnixListener;
use taffy::AvailableSpace;
use ui::a11y::Politeness;
use ui::widget::find_test_id;

/// The shipped Tilo session plus a second, separately installed one, so
/// there is a real choice and the picker is in the footer. Human names,
/// not the `DesktopNames=` id they launch under.
fn test_sessions() -> Vec<Session> {
    vec![
        Session {
            name: "Tilo".to_string(),
            argv: vec![
                "/usr/libexec/tilo-session".into(),
                "--backend".into(),
                "drm".into(),
            ],
            default: true,
        },
        Session {
            name: "Weston".to_string(),
            argv: vec!["/usr/bin/weston".into()],
            default: false,
        },
    ]
}

pub(super) fn one_session() -> Vec<Session> {
    vec![test_sessions().remove(0)]
}

/// Tests read a still clock, so the minute never turns during one.
fn pinned(mut app: GreeterApp) -> GreeterApp {
    app.clock = ClockFace {
        time: "09:41".into(),
        date: "Saturday, October 4".into(),
    };
    app.clock_pinned = true;
    app
}

fn quick_user() -> LoginUser {
    LoginUser {
        username: "tester".into(),
        display_name: "Ada Quick".into(),
    }
}

fn card_of(app: &GreeterApp) -> Box<dyn Widget> {
    app.build_composition(&Theme::dark())
}

fn set_focus_by_id(widget: &mut dyn Widget, id: &str) {
    widget.set_focus(widget.test_id() == Some(id));
    for child in widget.children_mut() {
        set_focus_by_id(child, id);
    }
}

fn key_event(key: Key, text: Option<&str>) -> WindowEvent {
    // winit intentionally keeps the platform-specific key field private;
    // these are the same inert placeholders used by ui::App's key tests.
    let event = ui::synthetic_input::key_event(key, text.map(Into::into), ElementState::Pressed);
    ui::synthetic_input::keyboard_input(event)
}

fn named_key_event(key: NamedKey) -> WindowEvent {
    key_event(Key::Named(key), None)
}

fn char_event(c: &str) -> WindowEvent {
    key_event(Key::Character(c.into()), Some(c))
}

/// Type `text` into the username field, one keystroke at a time.
fn type_username(app: &mut GreeterApp, state: &mut AutoCommit, text: &str) {
    for c in text.chars() {
        dispatch(app, state, "greeter.username", &char_event(&c.to_string()));
    }
    app.sync(state, &automerge::ROOT);
}

fn typed_username(app: &GreeterApp) -> String {
    app.username.with(Clone::clone)
}

/// Whether the greeter left `doc` exactly as empty as it was lent.
fn untouched(doc: &AutoCommit) -> bool {
    doc.keys(automerge::ROOT).next().is_none()
}

fn find_mut<'a>(w: &'a mut dyn Widget, id: &str) -> Option<&'a mut dyn Widget> {
    if w.test_id() == Some(id) {
        return Some(w);
    }
    w.children_mut()
        .into_iter()
        .find_map(|child| find_mut(child, id))
}

/// Activate the control `id` as Enter, Space or assistive technology
/// does, then let the greeter take what it reported.
///
/// A press publishes the control's own action; the answer a window's host
/// gives it is what runs the greeter's handler, so the test answers it here.
fn press(app: &mut GreeterApp, doc: &mut AutoCommit, id: &str) {
    app.sync(doc, &automerge::ROOT);
    app.composed.replace(app.build_composition(&Theme::dark()));
    let composed = app.composed.element_mut().expect("the greeter composed");
    let control = find_mut(composed, id).unwrap_or_else(|| panic!("{id} is on the screen"));
    assert!(
        control.perform_action(
            &ui::a11y::Action::Activate,
            &[],
            &mut ui::widget::Edits::lend(doc),
            &automerge::ROOT
        ),
        "{id} accepts activation"
    );
    ui::widget::controls::start_pending_control_invocations(
        composed,
        &mut ui::widget::Edits::lend(doc),
        &automerge::ROOT,
    );
    app.sync(doc, &automerge::ROOT);
}

fn dispatch_enter(app: &mut GreeterApp, state: &mut AutoCommit, focused: &str) -> bool {
    dispatch(app, state, focused, &named_key_event(NamedKey::Enter))
}

fn dispatch(
    app: &mut GreeterApp,
    state: &mut AutoCommit,
    focused: &str,
    event: &WindowEvent,
) -> bool {
    app.sync(state, &automerge::ROOT);
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = app.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout(
            node,
            Size {
                width: AvailableSpace::Definite(1280.0),
                height: AvailableSpace::Definite(800.0),
            },
        )
        .unwrap();
    app.apply_layout(&taffy, node, Point::ZERO);
    set_focus_by_id(app, focused);

    ui::testing::Dispatch::new()
        .doc(state)
        .layout(&taffy, node)
        .send(app, event)
        .handled
}

/// How a widget identifies itself in a tree dump: its `test_id` when it
/// has one, otherwise its type name. Visible labels are localized, so
/// they are never the automation address.
fn ident(w: &dyn Widget) -> String {
    w.test_id()
        .map_or_else(|| w.name().to_string(), str::to_string)
}

/// Every focusable widget under `w`, in the depth-first order the App
/// builds `focusable_order` in.
fn collect_focusables(w: &dyn Widget, out: &mut Vec<String>) {
    for child in w.children() {
        if child.is_focusable() && child.is_enabled() {
            out.push(ident(child));
        }
        collect_focusables(child, out);
    }
}

fn accessible_name_for(w: &dyn Widget, id: &str) -> Option<String> {
    find_test_id(w, id)
        .and_then(Widget::accessibility)
        .and_then(|n| n.name)
}

fn rect_for(w: &dyn Widget, id: &str) -> Option<Rect> {
    find_test_id(w, id).map(Widget::get_rect)
}

/// The panel's focusable widgets, identified by stable id, in tab order.
fn focusables(app: &GreeterApp) -> Vec<String> {
    let card = card_of(app);
    let mut v = Vec::new();
    collect_focusables(card.as_ref(), &mut v);
    v
}

/// The *shape* of the focus order — one widget type per focusable. The
/// App restores focus by ordinal after a rebuild, so this is what must
/// not change across incidental rebuilds.
fn focus_shape(app: &GreeterApp) -> Vec<String> {
    fn walk(w: &dyn Widget, out: &mut Vec<String>) {
        for child in w.children() {
            if child.is_focusable() {
                out.push(child.name().to_string());
            }
            walk(child, out);
        }
    }
    let card = card_of(app);
    let mut v = Vec::new();
    walk(card.as_ref(), &mut v);
    v
}

/// Every widget under `w` whose `name()` matches, at any depth.
fn count_named(w: &dyn Widget, name: &str) -> usize {
    w.children()
        .iter()
        .map(|c| usize::from(c.name() == name).saturating_add(count_named(*c, name)))
        .sum()
}

/// Everything the panel announces, in tree order: the accessible name of
/// every node that has one — what a screen reader reads out.
fn spoken(w: &dyn Widget) -> Vec<String> {
    fn said(node: ui::a11y::AccessNode, out: &mut Vec<String>) {
        if let Some(name) = node.name {
            out.push(name);
        }
        for child in node.children {
            said(child, out);
        }
    }
    fn walk(w: &dyn Widget, out: &mut Vec<String>) {
        for c in w.children() {
            if let Some(node) = Widget::accessibility(c) {
                said(node, out);
            }
            walk(c, out);
        }
    }
    let mut out = Vec::new();
    walk(w, &mut out);
    out
}

fn live_politeness_for(w: &dyn Widget, name: &str) -> Option<Politeness> {
    /// The node named `name` in `node`'s subtree.
    fn named<'a>(node: &'a ui::a11y::AccessNode, name: &str) -> Option<&'a ui::a11y::AccessNode> {
        if node.name.as_deref() == Some(name) {
            return Some(node);
        }
        node.children.iter().find_map(|child| named(child, name))
    }
    if let Some(node) = w.accessibility()
        && let Some(found) = named(&node, name)
    {
        return found.live;
    }
    w.children()
        .into_iter()
        .find_map(|child| live_politeness_for(child, name))
}

/// The passcode prompt's accessible node.
fn passcode_node(w: &dyn Widget) -> ui::a11y::AccessNode {
    find_test_id(w, "greeter.password")
        .and_then(Widget::accessibility)
        .expect("the passcode prompt is accessible")
}

/// Where the passcode's field itself sits, inside its prompt.
fn passcode_field_rect(w: &dyn Widget) -> Option<Rect> {
    let node = find_test_id(w, "greeter.password").and_then(Widget::accessibility)?;
    let field = node
        .children
        .iter()
        .find(|child| child.role == ui::a11y::Role::TextInput)?;
    let b = field.bounds?;
    Some(Rect::new(b.x0, b.y0, b.x1, b.y1))
}

/// A settled greeter laid out the way App does it, kept alive so it can
/// be relaid out in place after input — exactly the per-keystroke path.
struct Laid {
    taffy: TaffyTree<()>,
    node: NodeId,
    node_map: HashMap<NodeId, *mut dyn Widget>,
    bindings: ui::bound_dispatch::BoundNodeMap,
    text: TextRenderer,
    viewport: (f32, f32),
}

impl Laid {
    fn new(app: &mut GreeterApp, doc: &AutoCommit, viewport: (f32, f32), theme: &Theme) -> Self {
        app.sync(doc, &automerge::ROOT);
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        for _ in 0..8 {
            let mut laid = Self {
                taffy: TaffyTree::new(),
                node: NodeId::from(0usize),
                node_map: HashMap::new(),
                bindings: ui::bound_dispatch::BoundNodeMap::new(),
                text,
                viewport,
            };
            laid.node = app.build_layout(&mut laid.taffy, theme, &mut laid.node_map);
            app.sync(doc, &automerge::ROOT);
            // SAFETY: this helper exclusively owns the matching tree and
            // the app stays in place through admission and the solve.
            unsafe {
                laid.bindings
                    .rebuild(app, &laid.taffy, laid.node, &laid.node_map);
            }
            laid.relayout(app);
            if !app.take_structure_dirty() {
                return laid;
            }
            text = laid.text;
        }
        panic!("greeter composition did not settle before paint");
    }

    fn relayout(&mut self, app: &mut GreeterApp) {
        // SAFETY: as above; the tree was built by this helper for `app`.
        unsafe {
            ui::widget::compute_widget_layout(
                &mut self.taffy,
                self.node,
                Size {
                    width: AvailableSpace::Definite(self.viewport.0),
                    height: AvailableSpace::Definite(self.viewport.1),
                },
                &self.node_map,
                &self.bindings,
                &mut self.text,
            )
        }
        .unwrap();
        app.apply_layout(&self.taffy, self.node, Point::ZERO);
    }

    fn deliver(&self, app: &mut GreeterApp, doc: &mut AutoCommit, event: &WindowEvent) {
        ui::testing::Dispatch::new()
            .doc(doc)
            .layout(&self.taffy, self.node)
            .send(app, event);
    }
}

fn layout_at_with_theme(app: &mut GreeterApp, viewport: (f32, f32), theme: &Theme) {
    let doc = AutoCommit::new();
    let _ = Laid::new(app, &doc, viewport, theme);
}

fn inside(outer: Rect, inner: Rect) -> bool {
    inner.x0 >= outer.x0 && inner.y0 >= outer.y0 && inner.x1 <= outer.x1 && inner.y1 <= outer.y1
}

fn geometry(w: &dyn Widget, out: &mut Vec<Rect>) {
    out.push(w.get_rect());
    for child in w.children() {
        geometry(child, out);
    }
}

fn paint(app: &mut GreeterApp, theme: &Theme, text: &mut TextRenderer) {
    let mut scene = Scene::new();
    text.set_text_scale(theme.text_scale);
    app.draw(
        &mut scene,
        Affine::IDENTITY,
        text,
        theme,
        &PresenceCtx::default(),
    );
}

fn scene_bytes(app: &mut GreeterApp) -> (Vec<u32>, Vec<u32>) {
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    app.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &Theme::dark(),
        &PresenceCtx::default(),
    );
    let encoding = scene.encoding();
    (
        encoding
            .draw_tags
            .iter()
            .map(|tag| tag.0)
            .collect::<Vec<_>>(),
        encoding.path_data.clone(),
    )
}

fn rect_of(w: &dyn Widget, id: &str) -> Option<Rect> {
    if w.test_id() == Some(id) {
        return Some(w.get_rect());
    }
    w.children()
        .into_iter()
        .find_map(|child| rect_of(child, id))
}

/// Whether the widget `id` is out of the accessibility tree: it or an
/// ancestor reports itself hidden.
fn hidden_from_access(w: &dyn Widget, id: &str, hidden: bool) -> Option<bool> {
    let hidden = hidden || w.accessibility_hidden();
    if w.test_id() == Some(id) {
        return Some(hidden);
    }
    w.children()
        .into_iter()
        .find_map(|child| hidden_from_access(child, id, hidden))
}

/// "The login screen must appear with a 2944x1840 built-in panel plus a
/// 1920x1080 monitor attached."
///
/// The kiosk stretches the one sign-in window over both displays. With
/// each display at its own scale that is 3392x1080; when the kiosk keeps
/// both at 1x it is 4864x1840, which at the panel's scale 2 was a
/// 9728-wide texture the device refused, and the greeter exited until
/// greetd gave up. Either way the card is on the panel, the monitor shows
/// the time, and the window's frame fits the device.
#[test]
fn the_login_screen_appears_with_a_built_in_panel_and_a_1080p_monitor_attached() {
    use crate::output_scale::{Arrangement, Display, Displays};
    let arrangements = [
        (
            Rect::new(0.0, 0.0, 1472.0, 920.0),
            Rect::new(1472.0, 0.0, 3392.0, 1080.0),
        ),
        (
            Rect::new(0.0, 0.0, 2944.0, 1840.0),
            Rect::new(2944.0, 0.0, 4864.0, 1080.0),
        ),
    ];
    for (panel, monitor) in arrangements {
        let displays = Displays::fixed(Arrangement {
            displays: vec![
                Display {
                    rect: panel,
                    internal: true,
                },
                Display {
                    rect: monitor,
                    internal: false,
                },
            ],
        });
        let window = panel.union(monitor);
        for theme in [Theme::dark(), Theme::light()] {
            let mut app =
                GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()))
                    .with_displays(displays.clone());
            #[allow(
                clippy::cast_possible_truncation,
                reason = "display sizes in logical px fit f32"
            )]
            let viewport = (window.width() as f32, window.height() as f32);
            layout_at_with_theme(&mut app, viewport, &theme);
            assert_eq!(app.content, panel, "the card fills the built-in panel");
            for id in ["greeter.password", "greeter.clock"] {
                let rect = rect_of(&app, id).unwrap_or_else(|| panic!("{id} is laid out"));
                assert!(
                    rect.area() > 0.0 && inside(panel, rect),
                    "{id} at {rect:?} is not on the built-in panel {panel:?}"
                );
            }
            assert!(
                rect_of(&app, "greeter.password").is_some_and(|field| !inside(monitor, field)),
                "the monitor carries no password field"
            );
            for id in ["greeter.display-clock", "greeter.display-date"] {
                let rect = rect_of(&app, id).unwrap_or_else(|| panic!("{id} is laid out"));
                assert!(
                    rect.area() > 0.0 && inside(monitor, rect),
                    "{id} at {rect:?} is not on the monitor {monitor:?}"
                );
                assert_eq!(
                    hidden_from_access(&app, id, false),
                    Some(true),
                    "the monitor's {id} is read once, on the panel"
                );
            }
            assert_eq!(
                hidden_from_access(&app, "greeter.password", false),
                Some(false)
            );
        }

        // The frame the window draws at the panel's scale 2 fits every
        // device the toolkit opens, and keeps the window's logical size.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "display sizes in physical px fit u32"
        )]
        let physical = (
            (window.width() * 2.0) as u32,
            (window.height() * 2.0) as u32,
        );
        let limit = ui::gpu::requested_texture_limit();
        let extent = ui::scale::fit_to_texture_limit(physical, 2.0, limit);
        assert!(
            extent.width <= limit && extent.height <= limit,
            "{extent:?} exceeds the device limit {limit}"
        );
        let logical = ui::scale::logical_size((extent.width, extent.height), extent.scale);
        assert!(
            (logical.0 - window.width()).abs() <= 1.0 && (logical.1 - window.height()).abs() <= 1.0,
            "the frame keeps the window's layout: {logical:?} vs {window:?}"
        );
    }
}

#[test]
fn several_outputs_put_the_card_on_the_built_in_panel_and_the_backdrop_on_each() {
    use crate::output_scale::{Arrangement, Display, Displays};
    // The reported laptop: its 2944x1840 panel at scale 2 beside a 1080p
    // monitor. `cage` stretches the one window over both, so a card
    // centred in the window straddled the seam between the displays.
    let panel = Rect::new(0.0, 0.0, 1472.0, 920.0);
    let monitor = Rect::new(1472.0, 0.0, 3392.0, 1080.0);
    let displays = Displays::fixed(Arrangement {
        displays: vec![
            Display {
                rect: panel,
                internal: true,
            },
            Display {
                rect: monitor,
                internal: false,
            },
        ],
    });
    let mut app = GreeterApp::with_sessions(test_sessions()).with_displays(displays);
    layout_at_with_theme(&mut app, (3392.0, 1080.0), &Theme::dark());
    assert_eq!(app.content, panel);
    assert_eq!(app.layout_outputs, vec![panel, monitor]);
    for id in ["greeter.password", "greeter.clock"] {
        let rect = rect_of(&app, id).unwrap_or_else(|| panic!("{id} is laid out"));
        assert!(
            rect.area() > 0.0 && inside(panel, rect),
            "{id} at {rect:?} is not on the built-in panel"
        );
    }

    // Before the kiosk has resized the window to the new layout, the
    // composition stays centred in the window it has.
    layout_at_with_theme(&mut app, (1472.0, 920.0), &Theme::dark());
    assert_eq!(app.content, panel);
    assert!(app.layout_outputs.is_empty());
}

#[test]
fn the_passcode_keeps_focus_and_takes_keys_when_the_window_spans_two_outputs() {
    use crate::output_scale::{Arrangement, Display, Displays};
    // The reported laptop: its panel at scale 2 beside a 1080p monitor.
    // The window starts at the panel's size and grows to span both once
    // the kiosk has applied the layout, re-laying the card out on the
    // panel after the passcode field already had focus.
    let panel = Rect::new(0.0, 0.0, 1472.0, 920.0);
    let monitor = Rect::new(1472.0, 0.0, 3392.0, 1080.0);
    let displays = Displays::fixed(Arrangement {
        displays: vec![
            Display {
                rect: panel,
                internal: true,
            },
            Display {
                rect: monitor,
                internal: false,
            },
        ],
    });
    let greeter = pinned(
        GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()))
            .with_displays(displays),
    );
    let mut harness =
        ui::headless::FrameHarness::for_root(ui::State::new(), greeter, (1472, 920), Theme::dark());
    harness.tab(false);
    assert_eq!(
        harness.focused_access_node().and_then(|node| node.name),
        Some("Password".to_string())
    );

    harness.resize((3392, 1080)).unwrap();
    assert_eq!(harness.root::<GreeterApp>().content, panel);
    assert_eq!(
        harness
            .focus_access_nodes()
            .first()
            .and_then(|node| node.name.clone()),
        Some("Password".to_string()),
        "the first tab stop, where autofocus lands, is the passcode"
    );
    assert_eq!(
        harness.focused_access_node().and_then(|node| node.name),
        Some("Password".to_string()),
        "re-laying the card out on the panel keeps focus on the passcode"
    );
    let password = find_test_id(harness.root_widget(), "greeter.password")
        .expect("the passcode field is laid out");
    assert!(password.is_focused(), "the caret is in the passcode field");
    assert!(inside(panel, password.get_rect()));

    harness.type_text("abc");
    assert_eq!(
        harness
            .root::<GreeterApp>()
            .password_buffer
            .lock_recover()
            .as_str(),
        "abc",
        "keys typed at the greeter reach the passcode"
    );

    // The App finds focusable controls by walking the widget and taffy
    // trees together; a layout node without a widget sends its focus to
    // the wrong control.
    {
        let mut app = pinned(GreeterApp::with_sessions_and_login_user(
            one_session(),
            Some(quick_user()),
        ));
        app.displays = harness.root::<GreeterApp>().displays.clone();
        app.rect = Rect::new(0.0, 0.0, 3392.0, 1080.0);
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root = app.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        assert_eq!(app.layout_area, Some(panel));
        assert_trees_mirror(&app, &taffy, root, "GreeterApp");
    }
}

#[test]
fn console_reports_input_only_once_the_compositor_focuses_the_window() {
    let accepts = |app: &GreeterApp| {
        app.console_state()["accepts_input"]
            .as_bool()
            .expect("sign-in state reports whether it accepts input")
    };
    let mut app = GreeterApp::with_sessions(test_sessions());
    let mut state = AutoCommit::new();
    assert!(!accepts(&app), "a window nobody focused receives no keys");
    dispatch(&mut app, &mut state, "", &WindowEvent::Focused(true));
    assert!(accepts(&app));
    dispatch(&mut app, &mut state, "", &WindowEvent::Focused(false));
    assert!(!accepts(&app));
    // A key can only arrive at the focused window.
    dispatch(&mut app, &mut state, "", &named_key_event(NamedKey::Tab));
    assert!(accepts(&app));
}

#[test]
fn a_session_choice_appears_only_when_there_is_one_to_make() {
    // The image installs os/usr/share/wayland-sessions as-is, so this is
    // exactly what the greeter on a fresh install reads: the native session,
    // selected first, and the old shell beside it.
    {
        let shipped = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../os/usr/share/wayland-sessions");
        let sessions = read_sessions(&shipped);
        let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Tilo", "Tilo (old shell)"]);

        let app = GreeterApp::with_sessions(sessions);
        assert!(app.offers_session_choice());
        assert!(find_test_id(card_of(&app).as_ref(), "greeter.session").is_some());
        assert_eq!(app.console_state()["session_choice"], true);
        assert_eq!(
            app.console_state()["sessions"],
            serde_json::json!(["Tilo", "Tilo (old shell)"])
        );
        // Signing in without a choice starts the native session.
        assert_eq!(app.session_label(), "Tilo");
        assert_eq!(
            app.sessions[app.selected_session].argv,
            ["/usr/libexec/tilo-session", "--backend", "drm"]
        );
        assert_eq!(
            app.sessions[1].argv,
            ["/usr/libexec/tilo-session", "--shell", "--backend", "drm"]
        );
    }
    {
        let app = GreeterApp::with_sessions(one_session());
        assert!(!app.offers_session_choice());
        assert!(
            !focusables(&app).contains(&"greeter.session".to_string()),
            "a single session must not render a picker: {:?}",
            focusables(&app)
        );
        assert!(find_test_id(card_of(&app).as_ref(), "greeter.session").is_none());
        assert_eq!(app.console_state()["session_choice"], false);
    }
    {
        let app = GreeterApp::with_sessions(test_sessions());
        assert!(app.offers_session_choice());
        assert_eq!(app.console_state()["session_choice"], true);
        assert_eq!(
            focusables(&app),
            vec![
                "greeter.username",
                "greeter.password",
                "greeter.login",
                "greeter.fingerprint",
                "greeter.larger-text",
                "greeter.high-contrast",
                "greeter.screen-reader",
                "greeter.session",
                "greeter.power",
            ],
            "the choice sits after sign-in, beside the power actions"
        );
        assert_eq!(app.session_label(), "Tilo", "the human name, not the id");
    }
    {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let doc = AutoCommit::new();
        let root = automerge::ROOT;

        // The picker starts on the session that signing in would start.
        app.sync(&doc, &root);
        assert_eq!(app.selected_session, 0);
        assert_eq!(app.session_choice.with(Clone::clone), "Tilo");

        app.session_choice.set("Weston".to_owned());
        app.sync(&doc, &root);
        assert_eq!(app.selected_session, 1);
        assert_eq!(
            app.sessions[app.selected_session].argv,
            ["/usr/bin/weston"],
            "the choice must reach the argv handed to greetd"
        );

        // A label that names no known session leaves the choice alone.
        app.session_choice
            .set("a session that went away".to_owned());
        app.sync(&doc, &root);
        assert_eq!(app.selected_session, 1, "choice survives a stale label");
        assert!(untouched(&doc), "the choice stays out of the document");
    }
    {
        let mut app = GreeterApp::with_sessions_and_login_user(Vec::new(), Some(quick_user()));
        let doc = AutoCommit::new();
        app.sync(&doc, &automerge::ROOT);
        seed_password(&app, "pw");
        app.attempt_login(AuthMethod::Password);
        assert_eq!(app.notice, Notice::Error("greeter-error-no-session"));
        assert_eq!(app.phase, AuthPhase::Idle);
    }
}

fn only_bob_has_a_fingerprint(username: &str) -> bool {
    username == "bob"
}

#[test]
fn fingerprint_login_is_offered_only_for_enrolled_accounts_and_sends_no_password() {
    {
        assert!(fingerprint_list_has_enrollment(
            "Fingerprints for user tester:\n - #0: right-index-finger\n"
        ));
        assert!(fingerprint_list_has_enrollment(
            "Fingerprints for user tester:\n - right-index-finger\n"
        ));
        assert!(!fingerprint_list_has_enrollment(
            "Fingerprints for user tester:\nnone\n"
        ));
        assert!(!fingerprint_list_has_enrollment("right-index-finger"));
    }
    {
        let mut app = GreeterApp::with_sessions_login_user_and_fingerprint(
            one_session(),
            Some(quick_user()),
            FingerprintAvailability::Unavailable,
        );
        assert!(
            !focusables(&app).contains(&"greeter.fingerprint".to_string()),
            "an unavailable factor must not appear in the form or focus order"
        );

        let doc = AutoCommit::new();
        app.sync(&doc, &automerge::ROOT);
        app.attempt_login(AuthMethod::Fingerprint);
        assert_eq!(app.notice, Notice::Hint("greeter-hint-no-fingerprint"));
        assert_eq!(app.phase, AuthPhase::Idle);
    }
    {
        let mut app = GreeterApp::with_sessions_login_user_and_fingerprint(
            one_session(),
            None,
            FingerprintAvailability::CheckNamedAccount,
        );
        app.fingerprint_probe = only_bob_has_a_fingerprint;

        assert!(focusables(&app).contains(&"greeter.fingerprint".to_string()));
        assert!(app.fingerprint_available_for("bob"));
        assert!(!app.fingerprint_available_for("alice"));
        assert!(!app.fingerprint_available_for(""));
    }
    {
        let mut app = GreeterApp::with_sessions(one_session());
        let doc = AutoCommit::new();
        app.sync(&doc, &automerge::ROOT);

        assert_eq!(
            app.take_auth_secret(AuthMethod::Password),
            Err("greeter-hint-passcode")
        );
        assert_eq!(
            app.take_auth_secret(AuthMethod::Fingerprint)
                .map(|secret| secret.to_string()),
            Ok(String::new()),
            "fingerprint PAM conversation must begin without a dummy password"
        );
    }
    {
        let app = GreeterApp::for_shot(AuthScenario::Fingerprint);
        assert_eq!(app.phase, AuthPhase::Authenticating);
        assert_eq!(app.status_text(), "Place your finger on the reader");
        assert_eq!(
            live_politeness_for(card_of(&app).as_ref(), "Place your finger on the reader"),
            Some(Politeness::Assertive),
            "a time-sensitive PAM instruction must be announced without moving focus"
        );
    }
}

/// Seed the process-local passcode buffer without touching the document.
fn seed_password(app: &GreeterApp, pw: &str) {
    let mut value = app.password_buffer.lock_recover();
    value.zeroize();
    value.push_str(pw);
}

#[test]
fn submit_validates_fields_keeps_the_passcode_private_and_ignores_repeats() {
    {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut doc = AutoCommit::new();
        press(&mut app, &mut doc, "greeter.login");
        assert_eq!(app.notice, Notice::Hint("greeter-hint-username"));
        assert_eq!(app.status_text(), "Enter your username.");
        assert!(!app.requests.sign_in.get(), "the request is taken once");
        assert!(untouched(&doc), "an attempt writes nothing to the document");
    }
    {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&doc, &root);

        type_username(&mut app, &mut doc, "alice");
        assert_eq!(typed_username(&app), "alice");
        assert!(
            doc.get(&root, "greeter.username").unwrap().is_none(),
            "the typed username stays out of the document"
        );
        assert!(dispatch_enter(&mut app, &mut doc, "greeter.username"));
        app.sync(&doc, &root);
        assert_eq!(app.notice, Notice::Hint("greeter-hint-passcode"));
        assert_eq!(app.phase, AuthPhase::Idle);
    }
    {
        let mut app = GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
        let doc = AutoCommit::new();
        app.sync(&doc, &automerge::ROOT);
        seed_password(&app, "pw");
        // The test runner is not greetd.
        assert!(std::env::var_os("GREETD_SOCK").is_none());
        app.attempt_login(AuthMethod::Password);
        assert_eq!(app.notice, Notice::Error("greeter-error-service"));
        assert!(!app.status_text().contains("GREETD_SOCK"));
    }
    {
        let mut hint = GreeterApp::with_sessions(one_session());
        hint.set_hint("greeter-hint-passcode");
        let mut err = GreeterApp::with_sessions(one_session());
        err.finish_failure(Failure::Credentials);
        // Both are said under the field, at once.
        assert_eq!(
            live_politeness_for(card_of(&hint).as_ref(), "Enter your password."),
            Some(Politeness::Assertive),
            "a hint is said under the field"
        );
        assert_eq!(
            live_politeness_for(
                card_of(&err).as_ref(),
                "Incorrect username or password. Try again."
            ),
            Some(Politeness::Assertive),
            "an error is said under the field"
        );
        assert_eq!(hint.status_text(), "Enter your password.");
        assert_eq!(
            err.status_text(),
            "Incorrect username or password. Try again."
        );
    }
    {
        let app = GreeterApp::with_sessions(one_session());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        seed_password(&app, "correct horse battery staple");

        assert!(doc.get(&root, "greeter.password").unwrap().is_none());
        assert!(
            !doc.save()
                .windows("correct horse battery staple".len())
                .any(|bytes| bytes == b"correct horse battery staple"),
            "the serialized Automerge history must not contain the passcode"
        );
        let mut app = app;
        app.sync(&doc, &root);
        assert!(untouched(&doc));
        let secret = app
            .take_auth_secret(AuthMethod::Password)
            .expect("local passcode is available exactly for authentication");
        assert_eq!(secret.as_str(), "correct horse battery staple");
        assert!(app.password_buffer.lock().unwrap().is_empty());
    }
    for phase in [AuthPhase::Authenticating, AuthPhase::Starting] {
        let mut app = GreeterApp::with_sessions(one_session());
        let doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&doc, &root);
        app.phase = phase;
        app.username.set("alice".to_owned());
        seed_password(&app, "pw");

        app.requests.sign_in.set(true);
        app.sync(&doc, &root);
        assert_eq!(app.phase, phase);
        assert!(
            !app.requests.sign_in.get(),
            "the request is taken, not kept"
        );
        assert_eq!(&*app.password_buffer.lock().unwrap(), "pw");
    }
}

/// The widget tree must mirror the taffy tree **at every depth** — the
/// App recurses both in lock-step to build `focusable_order`.
fn assert_trees_mirror(w: &dyn Widget, taffy: &TaffyTree<()>, node: NodeId, path: &str) {
    let widget_kids = w.children();
    let taffy_kids = taffy.children(node).unwrap();
    assert_eq!(
        widget_kids.len(),
        taffy_kids.len(),
        "{path} has {} widget children but {} taffy children",
        widget_kids.len(),
        taffy_kids.len()
    );
    for (i, (child, &child_node)) in widget_kids.iter().zip(taffy_kids.iter()).enumerate() {
        assert_trees_mirror(
            *child,
            taffy,
            child_node,
            &format!("{path}/{}[{i}]", ident(*child)),
        );
    }
}

#[test]
fn widget_tree_and_focus_order_are_complete_and_stable() {
    // Phase `widget_tree_mirrors_taffy_tree`.
    for app in [
        GreeterApp::with_sessions(test_sessions()),
        GreeterApp::for_shot(AuthScenario::PersonalError),
        GreeterApp::for_shot(AuthScenario::PersonalAuthenticating),
    ] {
        let mut app = app;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root = app.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        assert_eq!(app.children().len(), 1);
        assert_trees_mirror(&app, &taffy, root, "GreeterApp");
    }
    // Phase `focusable_order_is_stable_across_idle_state_changes`.
    // On the form, the focusable order must NOT shift when the session
    // changes, a notice appears, or Caps Lock is detected — the
    // precondition for keeping focus and caret glued to the same field.
    {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let base = focus_shape(&app);
        let card = card_of(&app);
        let prompt = passcode_node(card.as_ref());
        let reveal = prompt
            .children
            .iter()
            .flat_map(|field| field.children.iter())
            .find(|node| node.role == ui::a11y::Role::ToggleButton)
            .expect("the shared field retains an addressable reveal control");
        assert_eq!(reveal.name.as_deref(), Some("Show password"));
        assert!(
            reveal.value.is_none(),
            "the reveal control exposes no passcode"
        );
        assert!(
            !reveal.state.focusable,
            "Tab continues from field to sign-in"
        );
        assert_eq!(
            base,
            vec![
                "TextInput",    // username
                "SecretPrompt", // passcode
                "Button",       // sign in
                "Button",       // fingerprint
                "Button",       // larger text
                "Button",       // high contrast
                "Button",       // screen reader
                "Dropdown",     // session
                "Dropdown",     // power
            ]
        );
        app.selected_session = 1;
        assert_eq!(focus_shape(&app), base, "changing session keeps order");
        app.notice = Notice::Error("greeter-error-credentials");
        assert_eq!(focus_shape(&app), base, "an error keeps order");
        app.notice = Notice::Hint("greeter-hint-passcode");
        assert_eq!(focus_shape(&app), base, "a hint keeps order too");
        let line = |app: &GreeterApp| {
            passcode_node(card_of(app).as_ref())
                .children
                .iter()
                .find(|node| node.role == ui::a11y::Role::StaticText)
                .and_then(|node| node.name.clone())
        };
        let hint = line(&app);
        app.caps_lock.observe('A', false);
        assert_eq!(focus_shape(&app), base, "the Caps-Lock note keeps order");
        assert_ne!(
            line(&app),
            hint,
            "the Caps-Lock note is the line under the field"
        );
    }
}

#[test]
fn a_personal_device_greets_the_person_and_asks_only_for_the_passcode() {
    {
        let mut app = GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
        let doc = AutoCommit::new();
        app.sync(&doc, &automerge::ROOT);
        assert_eq!(
            typed_username(&app),
            "tester",
            "the username is seeded without asking the user to type it"
        );
        assert_eq!(
            focusables(&app),
            vec![
                "greeter.password",
                "greeter.login",
                "greeter.cant-sign-in",
                "greeter.fingerprint",
                "greeter.larger-text",
                "greeter.high-contrast",
                "greeter.screen-reader",
                "greeter.power",
            ],
            "autofocus therefore lands on the passcode field"
        );
        let card = card_of(&app);
        assert_eq!(
            accessible_name_for(card.as_ref(), "greeter.welcome").as_deref(),
            Some("Ada Quick"),
            "the person, not the product, gets the headline"
        );
        assert_eq!(
            accessible_name_for(card.as_ref(), "greeter.account").as_deref(),
            Some("tester")
        );
        assert_eq!(
            accessible_name_for(card.as_ref(), "greeter.password").as_deref(),
            Some("Password")
        );
        assert_eq!(
            accessible_name_for(card.as_ref(), "greeter.login").as_deref(),
            Some("Sign in"),
            "the icon-only action still has a spoken name"
        );
        assert!(accessible_name_for(card.as_ref(), "greeter.username").is_none());
        assert_eq!(count_named(card.as_ref(), "Avatar"), 1);
    }
    {
        let greeter = GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
        let mut app = ui::App::new(ui::State::new(), Box::new(greeter));
        app.autofocus_first = true;
        app.autofocus_on_structure_change = true;
        app.init_layout();
        app.refresh_semantic_tree();
        let focused = app
            .semantic_tree()
            .iter()
            .find(|node| node.state.focused)
            .and_then(|node| node.name.as_deref());
        assert_eq!(focused, Some("Password"));
    }
    {
        let app = GreeterApp::with_sessions_and_login_user(one_session(), None);
        let card = card_of(&app);
        assert_eq!(
            accessible_name_for(card.as_ref(), "greeter.username").as_deref(),
            Some("Username")
        );
        assert_eq!(
            accessible_name_for(card.as_ref(), "greeter.welcome").as_deref(),
            Some("Sign in to Tilo")
        );
        assert_eq!(count_named(card.as_ref(), "Avatar"), 0);
        assert_eq!(
            focusables(&app).first().map(String::as_str),
            Some("greeter.username")
        );
    }
    {
        let app = GreeterApp::for_shot(AuthScenario::Personal);
        let said = spoken(card_of(&app).as_ref());
        let person = said.iter().position(|s| s == "Kai Example");
        let passcode = said.iter().position(|s| s == "Password");
        assert!(person.is_some() && person < passcode, "{said:?}");
    }
}

#[test]
fn enter_signs_in_only_from_text_fields() {
    // Phase `enter_defaults_to_sign_in_from_text_fields`.
    for field in ["greeter.username", "greeter.password"] {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut doc = AutoCommit::new();
        assert!(dispatch_enter(&mut app, &mut doc, field));
        assert!(app.requests.sign_in.get(), "Enter in {field} signs in");
        app.sync(&doc, &automerge::ROOT);
        assert_eq!(app.notice, Notice::Hint("greeter-hint-username"));
        assert!(untouched(&doc));
    }
    for control in [
        "greeter.session",
        "greeter.fingerprint",
        "greeter.larger-text",
        "greeter.high-contrast",
        "greeter.screen-reader",
        "greeter.power",
    ] {
        let mut app = GreeterApp::with_sessions(test_sessions());
        let mut doc = AutoCommit::new();
        let handled = dispatch_enter(&mut app, &mut doc, control);
        assert!(
            !app.requests.sign_in.get(),
            "Enter on {control} must not submit the passcode form"
        );
        if control == "greeter.session" || control == "greeter.power" {
            assert!(handled, "the focused dropdown consumes Enter itself");
        } else {
            assert!(!handled, "{control} remains available to App activation");
        }
    }
}

#[test]
fn the_sign_in_and_fingerprint_actions_each_start_one_attempt() {
    let mut app = GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
    let mut doc = AutoCommit::new();
    press(&mut app, &mut doc, "greeter.login");
    assert_eq!(app.notice, Notice::Hint("greeter-hint-passcode"));
    assert!(!app.requests.sign_in.get(), "one press, one attempt");

    seed_password(&app, "typed before the finger");
    press(&mut app, &mut doc, "greeter.fingerprint");
    // The test runner is not greetd, so the attempt ends at asking for it.
    assert_eq!(app.notice, Notice::Error("greeter-error-service"));
    assert!(
        app.password_buffer.lock().unwrap().is_empty(),
        "a fingerprint attempt scrubs a passcode typed before it"
    );
    assert!(!app.requests.fingerprint.get(), "one press, one attempt");
    assert!(untouched(&doc), "attempts write nothing to the document");
}

#[test]
fn in_flight_feedback_is_said_once_with_appropriate_urgency() {
    {
        let app = GreeterApp::for_shot(AuthScenario::PersonalAuthenticating);
        assert_eq!(app.status_text(), "Signing in…");
        let card = card_of(&app);
        assert!(
            passcode_node(card.as_ref()).state.busy,
            "the prompt says it is checking"
        );
        let said = spoken(card.as_ref());
        assert_eq!(
            said.iter().filter(|s| s.as_str() == "Signing in…").count(),
            1,
            "the in-flight sentence appears exactly once: {said:?}"
        );
    }
    for (scenario, message, expected) in [
        (
            AuthScenario::PersonalHint,
            "Enter your password.",
            Politeness::Assertive,
        ),
        (
            AuthScenario::PersonalError,
            "Incorrect password. Try again.",
            Politeness::Assertive,
        ),
        (
            AuthScenario::PersonalStarting,
            "Starting your session…",
            Politeness::Polite,
        ),
    ] {
        let app = GreeterApp::for_shot(scenario);
        assert_eq!(
            live_politeness_for(card_of(&app).as_ref(), message),
            Some(expected),
            "{scenario:?} feedback must be announced at the right urgency"
        );
    }
    {
        let mut app = GreeterApp::with_sessions(one_session());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&doc, &root);
        app.begin_authenticating();
        assert_eq!(app.status_text(), "Signing in…");
        let payload = AuthUpdate::Prompt {
            text: "Place your finger on the reader".to_string(),
            is_error: false,
        }
        .encode();
        app.on_external_event(&payload, &mut ui::widget::Edits::lend(&mut doc), &root);
        assert_eq!(app.phase, AuthPhase::Authenticating);
        assert_eq!(app.status_text(), "Place your finger on the reader");
    }
    {
        let mut app = GreeterApp::with_sessions(one_session());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&doc, &root);
        app.begin_authenticating();
        app.on_external_event(
            &AuthUpdate::Accepted.encode(),
            &mut ui::widget::Edits::lend(&mut doc),
            &root,
        );
        assert_eq!(app.phase, AuthPhase::Starting);
        assert!(app.phase.is_busy(), "inputs stay inert while handing over");
        assert_eq!(app.status_text(), "Starting your session…");
    }
}

/// A fake greetd on `listener`: it takes the passcode the form held without
/// asking anyone, asks for a second factor, and refuses the attempt once
/// `stale_attempted` says an answer for the prompt that is already gone has
/// been submitted — so what it reads at each step is decided here, not by
/// timing.
fn second_factor_conversation(
    listener: UnixListener,
    stale_attempted: std::sync::mpsc::Receiver<()>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (mut server, _) = listener.accept().expect("the greeter connects");
        assert!(
            matches!(
                Request::read_from(&mut server).unwrap(),
                Request::CreateSession { username } if username == "tester"
            ),
            "the known account signs in"
        );
        Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: "Password: ".to_owned(),
        }
        .write_to(&mut server)
        .unwrap();
        assert!(matches!(
            Request::read_from(&mut server).unwrap(),
            Request::PostAuthMessageResponse { response }
                if response.as_deref() == Some("first-password")
        ));
        // PAM asks for a second factor.
        Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: "One-time code".to_owned(),
        }
        .write_to(&mut server)
        .unwrap();
        assert!(
            matches!(
                Request::read_from(&mut server).unwrap(),
                Request::PostAuthMessageResponse { response }
                    if response.as_deref() == Some("765432")
            ),
            "the answer typed for this challenge is the one it gets"
        );
        stale_attempted.recv().unwrap();
        Response::Error {
            error_type: ErrorType::AuthError,
            description: "pam_authenticate: AUTH_ERR".to_owned(),
        }
        .write_to(&mut server)
        .unwrap();
        assert!(
            matches!(
                Request::read_from(&mut server).unwrap(),
                Request::CancelSession
            ),
            "the refused attempt is cancelled, and no answer to a gone prompt was sent"
        );
        Response::Success.write_to(&mut server).unwrap();
    })
}

/// PAM asking for a second factor: what the challenge is asked with, what
/// the person types into the form, and that the answer goes to the challenge
/// it was asked for — once.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one conversation read from its prompt through its refusal"
)]
fn a_second_factor_is_asked_and_answered_once() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("greetd.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let (updates, reported) = std::sync::mpsc::channel::<AuthUpdate>();
    let (stale_submitted, stale_attempted) = std::sync::mpsc::channel::<()>();

    // The worker `attempt_login` starts, with its updates read here instead
    // of at a winit event loop's proxy.
    let session = crate::auth::spawn_auth_updates(
        sock.to_string_lossy().into_owned(),
        "tester".to_owned(),
        Zeroizing::new("first-password".to_owned()),
        vec!["fixed-session".to_owned()],
        Vec::new(),
        Arc::new(AtomicU64::new(1)),
        move |update| {
            let _ = updates.send(update);
        },
    );
    let greetd = second_factor_conversation(listener, stale_attempted);

    let mut app = GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    app.begin_authenticating();
    app.auth_session = Some(session.clone());
    app.sync(&doc, &root);

    // What the PAM conversation asks reaches the form as the greeter's own
    // update, carrying the identity of the challenge it waits on.
    let challenge = loop {
        let update = reported
            .recv_timeout(Duration::from_secs(10))
            .expect("the conversation asks for a second factor");
        app.on_external_event(
            &update.encode(),
            &mut ui::widget::Edits::lend(&mut doc),
            &root,
        );
        if let AuthUpdate::Challenge(challenge) = update {
            break challenge;
        }
    };
    assert!(challenge.secret, "a second factor is not echoed");

    // The form asks for it: the prompt, the field and the action say so.
    assert_eq!(app.phase, AuthPhase::Authenticating);
    assert_eq!(app.status_text(), "One-time code");
    let card = card_of(&app);
    assert_eq!(
        accessible_name_for(card.as_ref(), "greeter.password").as_deref(),
        Some("Authentication response")
    );
    assert_eq!(
        accessible_name_for(card.as_ref(), "greeter.login").as_deref(),
        Some("Respond to authentication challenge")
    );
    assert!(
        spoken(card.as_ref())
            .iter()
            .any(|said| said == "One-time code"),
        "the question itself is read"
    );
    assert_eq!(
        focusables(&app).first().map(String::as_str),
        Some("greeter.password"),
        "the field is ready for the answer"
    );
    assert!(Widget::take_focus_reset(&mut app));

    // Type the code and send it as this challenge's answer.
    for c in "765432".chars() {
        dispatch(
            &mut app,
            &mut doc,
            "greeter.password",
            &char_event(&c.to_string()),
        );
    }
    assert_eq!(&*app.password_buffer.lock_recover(), "765432");
    assert!(dispatch_enter(&mut app, &mut doc, "greeter.password"));
    app.sync(&doc, &root);
    assert!(
        app.password_buffer.lock_recover().is_empty(),
        "the answer is handed over, not left in the form"
    );

    // The challenge is answered and gone: its identity answers nothing more,
    // and the form asks for no factor.
    assert_eq!(
        session.respond(challenge.id, Zeroizing::new("765432".to_owned())),
        Err("This authentication challenge is no longer waiting for a response")
    );
    assert!(
        app.auth_session
            .as_ref()
            .and_then(AuthSession::challenge)
            .is_none()
    );
    let card = card_of(&app);
    assert_eq!(
        accessible_name_for(card.as_ref(), "greeter.password").as_deref(),
        Some("Password"),
        "no prompt for another factor is drawn"
    );
    assert_eq!(
        accessible_name_for(card.as_ref(), "greeter.login").as_deref(),
        Some("Sign in")
    );

    // A second answer typed now has nothing to answer: the field is closed
    // while the conversation says nothing, so it takes no input, and pressing
    // the action again sends no response and draws no second prompt.
    assert!(
        passcode_node(card.as_ref()).state.busy,
        "the answered prompt waits for the conversation to say more"
    );
    for c in "111111".chars() {
        dispatch(
            &mut app,
            &mut doc,
            "greeter.password",
            &char_event(&c.to_string()),
        );
    }
    assert!(
        app.password_buffer.lock_recover().is_empty(),
        "the closed field holds no second answer"
    );
    assert!(dispatch_enter(&mut app, &mut doc, "greeter.password"));
    app.sync(&doc, &root);
    assert_eq!(
        accessible_name_for(card_of(&app).as_ref(), "greeter.login").as_deref(),
        Some("Sign in"),
        "the form asks for no factor"
    );
    stale_submitted.send(()).unwrap();

    // greetd's refusal returns the person to the form, signed in nowhere.
    let update = reported
        .recv_timeout(Duration::from_secs(10))
        .expect("the refused attempt is reported");
    assert_eq!(update, AuthUpdate::Failed(Failure::Credentials));
    app.on_external_event(
        &update.encode(),
        &mut ui::widget::Edits::lend(&mut doc),
        &root,
    );
    assert_eq!(app.phase, AuthPhase::Idle);
    assert!(app.auth_session.is_none());
    assert_eq!(app.failures, 1, "the attempt was refused once");
    assert_eq!(app.notice, Notice::Error("greeter-error-passcode"));
    assert!(app.password_buffer.lock_recover().is_empty());
    greetd.join().unwrap();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one table row per failure, then the same ready-form checks for each"
)]
fn every_failure_reads_as_a_clear_sentence_and_returns_to_a_ready_form() {
    let cases = [
        (Failure::Credentials, true, "Incorrect password. Try again."),
        (
            Failure::Credentials,
            false,
            "Incorrect username or password. Try again.",
        ),
        (
            Failure::Locked,
            true,
            "Too many failed attempts. Wait a few minutes, then try again.",
        ),
        (
            Failure::AccountExpired,
            true,
            "This account has expired. Ask an administrator to renew it.",
        ),
        (
            Failure::PasscodeExpired,
            true,
            "Your password has expired. Ask an administrator to reset it.",
        ),
        (
            Failure::NotPermitted,
            true,
            "This account can't sign in on this computer.",
        ),
        (
            Failure::Unavailable,
            true,
            "Your password couldn't be checked. Try again.",
        ),
        (
            Failure::SessionStart,
            true,
            "Your desktop couldn't start. Try again, or restart the computer.",
        ),
        (
            Failure::Service,
            true,
            "The sign-in service isn't responding. Restart the computer to try again.",
        ),
    ];
    for (failure, personal, sentence) in cases {
        let mut app =
            GreeterApp::with_sessions_and_login_user(one_session(), personal.then(quick_user));
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&doc, &root);
        if !personal {
            app.username.set("alice".to_owned());
        }
        seed_password(&app, "wrongpw");
        app.begin_authenticating();
        let _ = Widget::take_focus_reset(&mut app);

        app.on_external_event(
            &AuthUpdate::Failed(failure).encode(),
            &mut ui::widget::Edits::lend(&mut doc),
            &root,
        );

        assert_eq!(
            app.phase,
            AuthPhase::Idle,
            "{failure:?} returns to the form"
        );
        assert_eq!(app.status_text(), sentence, "{failure:?}");
        assert!(matches!(app.notice, Notice::Error(_)));
        assert!(
            app.password_buffer.lock().unwrap().is_empty(),
            "{failure:?} clears the passcode"
        );
        if !personal {
            assert_eq!(
                typed_username(&app),
                "alice",
                "the username survives a failure"
            );
        }
        assert!(
            Widget::take_focus_reset(&mut app),
            "{failure:?} puts the keyboard back on the form"
        );
        let card = card_of(&app);
        assert_eq!(
            live_politeness_for(card.as_ref(), sentence),
            Some(Politeness::Assertive)
        );
        assert!(
            focusables(&app).contains(&"greeter.password".to_string()),
            "the passcode field is editable again"
        );
    }
    {
        let mut greeter =
            GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
        greeter.begin_authenticating();
        greeter.finish_failure(Failure::Credentials);
        let mut app = ui::App::new(ui::State::new(), Box::new(greeter));
        app.autofocus_first = true;
        app.autofocus_on_structure_change = true;
        app.init_layout();
        app.rebuild_structure();
        app.refresh_semantic_tree();
        let focused = app
            .semantic_tree()
            .iter()
            .find(|node| node.state.focused)
            .and_then(|node| node.name.as_deref());
        assert_eq!(focused, Some("Password"));
    }
}

#[test]
fn authentication_and_power_actions_lock_input_and_reset_focus() {
    {
        let mut app = GreeterApp::with_sessions(test_sessions());
        app.begin_authenticating();
        assert_eq!(
            focusables(&app),
            vec![
                "greeter.login",
                "greeter.larger-text",
                "greeter.high-contrast",
                "greeter.screen-reader",
                "greeter.power",
            ],
            "the sign-in action stays the first focusable during auth; power stays reachable"
        );
        assert!(Widget::take_focus_reset(&mut app));
        assert!(
            !Widget::take_focus_reset(&mut app),
            "focus reset is one-shot"
        );
    }
    {
        let mut greeter = GreeterApp::with_sessions(one_session());
        greeter.begin_authenticating();
        let mut app = ui::App::new(ui::State::new(), Box::new(greeter));
        app.autofocus_first = true;
        app.autofocus_on_structure_change = true;
        app.init_layout();
        app.rebuild_structure();
        app.refresh_semantic_tree();
        let focused = app
            .semantic_tree()
            .iter()
            .find(|node| node.state.focused)
            .and_then(|node| node.name.as_deref());
        assert_eq!(focused, Some("Sign in"));
    }
    for action in PowerAction::ALL {
        let mut app = GreeterApp::with_sessions(one_session());
        let doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&doc, &root);
        app.requests.power.set(Some(action));
        app.sync(&doc, &root);
        assert_eq!(app.last_power_action, Some(action));
        app.last_power_action = None;
        app.sync(&doc, &root);
        assert_eq!(app.last_power_action, None, "{action:?} ran once");
    }
}

#[test]
fn the_backdrop_is_still_and_asks_for_no_frames() {
    for scenario in [
        AuthScenario::Personal,
        AuthScenario::PersonalError,
        AuthScenario::Form,
        AuthScenario::PersonalHint,
    ] {
        let mut app = GreeterApp::for_shot(scenario);
        layout_at_with_theme(&mut app, (1472.0, 920.0), &Theme::dark());
        assert_eq!(
            app.next_wake_deadline(),
            None,
            "{scenario:?} must let the loop sleep"
        );
        assert!(
            !app.needs_animation_frame(),
            "{scenario:?} must not animate"
        );
        // The same screen an hour on: nothing in it may follow the clock.
        let at = ui::time::Instant::now();
        let first = ui::time::testing::with_monotonic_time(at, || scene_bytes(&mut app));
        let hour_on = at
            .checked_add(Duration::from_secs(3600))
            .expect("an hour on");
        let later = ui::time::testing::with_monotonic_time(hour_on, || scene_bytes(&mut app));
        assert!(
            first == later,
            "{scenario:?} painted a different frame later"
        );
    }
}

/// The native host repaints a widget's reported damage and nothing else
/// unless geometry moved. Typing into the passcode field must therefore
/// leave every layout box where it was and report damage confined to the
/// field's row, whatever the display size.
#[test]
fn a_keystroke_repaints_the_passcode_row_not_the_screen() {
    for (viewport, theme) in [
        ((2944.0, 1840.0), Theme::dark()),
        ((1472.0, 920.0), Theme::dark()),
        ((1280.0, 800.0), Theme::light()),
        ((640.0, 480.0), Theme::dark().with_text_scale(2.0)),
    ] {
        let mut app = pinned(GreeterApp::with_sessions_and_login_user(
            one_session(),
            Some(quick_user()),
        ));
        let mut doc = AutoCommit::new();
        let mut laid = Laid::new(&mut app, &doc, viewport, &theme);
        set_focus_by_id(&mut app, "greeter.password");
        // The first frame paints everything; what counts is every frame
        // after it. Reuse the native host renderer across measurement and
        // paint; replacing it is a separate whole-renderer invalidation.
        paint(&mut app, &theme, &mut laid.text);
        Widget::mark_drawn(&mut app);
        let card = app.composed.element().expect("composed");
        let row = find_test_id(card, "greeter.password")
            .map(Widget::get_rect)
            .expect("passcode field")
            .union(rect_for(card, "greeter.login").expect("sign-in action"));
        let mut before = Vec::new();
        geometry(&app, &mut before);
        // The scroll bar shown with the first frame waits, then fades on
        // its own clock; let both run out first, so the keystrokes'
        // damage is theirs alone however slowly they are typed.
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let wake = Widget::next_wake_deadline(&app);
            if wake.is_none() && !Widget::needs_animation_frame(&app) {
                break;
            }
            assert!(Instant::now() < deadline, "the screen never settled");
            if let Some(wake) = wake {
                std::thread::sleep(wake.saturating_duration_since(Instant::now()));
            }
            paint(&mut app, &theme, &mut laid.text);
            let mut settled = Vec::new();
            app.collect_damage(&mut settled);
            Widget::mark_drawn(&mut app);
        }

        for c in ["h", "u", "n", "t", "e", "r", "2"] {
            laid.deliver(&mut app, &mut doc, &char_event(c));
            app.sync(&doc, &automerge::ROOT);
            assert!(
                !app.take_structure_dirty(),
                "a keystroke at {viewport:?} rebuilt the screen"
            );
            laid.relayout(&mut app);
            let mut after = Vec::new();
            geometry(&app, &mut after);
            assert!(before == after, "a keystroke moved layout at {viewport:?}");

            // As the native host does: paint, then collect what changed.
            paint(&mut app, &theme, &mut laid.text);
            let mut damage = Vec::new();
            app.collect_damage(&mut damage);
            assert!(!damage.is_empty(), "the typed character must be painted");
            let area: f64 = damage.iter().map(Rect::area).sum();
            let screen = f64::from(viewport.0) * f64::from(viewport.1);
            // On a laptop panel the field is a sliver of the screen; on a
            // tiny large-text display it is the field, and nothing more.
            let bound = if viewport.0 >= 1280.0 { 0.02 } else { 0.1 };
            assert!(
                area <= screen * bound,
                "a keystroke at {viewport:?} damaged {:.1}% of the screen: {damage:?}",
                area / screen * 100.0
            );
            for rect in &damage {
                assert!(
                    inside(row.inflate(2.0, 2.0), *rect),
                    "damage {rect:?} escapes the passcode row {row:?}"
                );
            }
            Widget::mark_drawn(&mut app);
        }
        assert_eq!(&*app.password_buffer.lock().unwrap(), "hunter2");
        assert!(
            doc.get(&automerge::ROOT, "greeter.password")
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn the_form_stays_on_screen_at_every_size_and_text_scale() {
    for (viewport, scale) in [
        ((2944.0, 1840.0), 1.0),
        ((1472.0, 920.0), 1.0),
        ((1280.0, 800.0), 1.0),
        ((800.0, 600.0), 1.5),
        ((640.0, 480.0), 2.0),
        ((360.0, 640.0), 1.0),
    ] {
        for scenario in [AuthScenario::PersonalError, AuthScenario::Error] {
            let mut app = GreeterApp::for_shot(scenario);
            app.sessions = test_sessions();
            let theme = Theme::dark().with_text_scale(scale);
            layout_at_with_theme(&mut app, viewport, &theme);
            let screen = Rect::new(0.0, 0.0, f64::from(viewport.0), f64::from(viewport.1));
            let card = app.composed.element().expect("composed");
            for id in [
                "greeter.password",
                "greeter.login",
                "greeter.larger-text",
                "greeter.high-contrast",
                "greeter.screen-reader",
                "greeter.session",
                "greeter.power",
            ] {
                let rect = rect_for(card, id)
                    .unwrap_or_else(|| panic!("{id} missing at {viewport:?}x{scale}"));
                assert!(
                    rect.width() > 0.0 && inside(screen, rect),
                    "{id} {rect:?} is off screen at {viewport:?} x{scale} ({scenario:?})"
                );
            }
            let field = passcode_field_rect(card).unwrap();
            let action = rect_for(card, "greeter.login").unwrap();
            assert!(
                (field.center().y - action.center().y).abs() < 1.0 && action.x0 >= field.x1,
                "the sign-in action sits beside the field at {viewport:?}: {field:?} {action:?}"
            );
        }
    }
}

#[test]
fn copy_uses_words_not_symbols() {
    // The screen says things in words and draws icons with the toolkit's
    // icon set; no emoji or pictographic characters stand in for either.
    for source in [
        include_str!("../../locales/en-US.ftl"),
        include_str!("../../locales/de-DE.ftl"),
    ] {
        for (key, text) in source
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_once(" = "))
        {
            assert!(
                text.chars().all(|c| c.is_alphanumeric()
                    || c.is_whitespace()
                    || c.is_ascii_punctuation()
                    || c == '\u{2026}'
                    || c == '\u{2019}'),
                "{key} draws a symbol: {text:?}"
            );
        }
    }
    for key in crate::i18n::KEYS {
        assert_ne!(t(key), *key, "{key} must resolve");
    }
}

#[test]
fn shot_scenarios_compose() {
    for scenario in [
        AuthScenario::Form,
        AuthScenario::Error,
        AuthScenario::Authenticating,
        AuthScenario::Fingerprint,
        AuthScenario::CapsLock,
        AuthScenario::Hint,
        AuthScenario::Starting,
        AuthScenario::LongName,
        AuthScenario::Personal,
        AuthScenario::PersonalError,
        AuthScenario::PersonalAuthenticating,
        AuthScenario::PersonalCapsLock,
        AuthScenario::PersonalHint,
        AuthScenario::PersonalStarting,
        AuthScenario::PersonalNoFingerprint,
        AuthScenario::PersonalLocked,
        AuthScenario::PersonalSessionFailed,
        AuthScenario::PersonalServiceDown,
        AuthScenario::PersonalPasscodeExpired,
        AuthScenario::PersonalSessions,
        AuthScenario::PersonalPin,
    ] {
        let mut app = GreeterApp::for_shot(scenario);
        layout_at_with_theme(&mut app, (1280.0, 800.0), &Theme::dark());
        assert!(app.composed.element().is_some(), "{scenario:?} composes");
    }
}

#[test]
fn the_time_is_the_hero_and_the_clock_wakes_once_a_minute() {
    let mut app = GreeterApp::for_shot(AuthScenario::Personal);
    layout_at_with_theme(&mut app, (1472.0, 920.0), &Theme::dark());
    let card = app.composed.element().expect("composed");
    let clock = rect_for(card, "greeter.clock").expect("the time is shown");
    let name = rect_for(card, "greeter.welcome").expect("the person is named");
    let field = rect_for(card, "greeter.password").expect("the passcode field");
    assert!(
        clock.height() > name.height() * 3.0,
        "the time is display type: {clock:?} vs {name:?}"
    );
    assert!(
        clock.y1 <= name.y0 && name.y1 <= field.y0,
        "time, then who, then the passcode"
    );
    assert_eq!(
        accessible_name_for(card, "greeter.clock").as_deref(),
        Some("09:41")
    );
    // A live screen wakes at the next minute and not before.
    let live = GreeterApp::with_sessions(one_session());
    let wake = live
        .next_wake_deadline()
        .expect("the clock asks for a wake");
    let until = wake.saturating_duration_since(Instant::now());
    assert!(until <= Duration::from_secs(60), "{until:?}");
}

#[test]
fn a_boot_sign_in_shows_only_the_transition_and_falls_back_without_an_error() {
    let mut app = GreeterApp::for_shot(AuthScenario::Accounts);
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    app.offer_boot_sign_in("grace");
    app.sync(&doc, &root);
    assert_eq!(app.phase, AuthPhase::Starting);
    assert_eq!(
        app.login_user.as_ref().map(|u| u.username.as_str()),
        Some("grace"),
        "the person who signed in at boot is the one starting"
    );
    assert_eq!(app.status_text(), "Starting your session…");
    assert!(
        !focusables(&app).contains(&"greeter.password".to_string()),
        "nothing to type while the session starts"
    );

    app.on_external_event(
        &AuthUpdate::Accepted.encode(),
        &mut ui::widget::Edits::lend(&mut doc),
        &root,
    );
    assert_eq!(app.phase, AuthPhase::Starting);

    // greetd refused, or PAM asked a question: the ordinary screen, with
    // that person chosen, and no error.
    let _ = Widget::take_focus_reset(&mut app);
    app.on_external_event(
        &AuthUpdate::Failed(Failure::Cancelled).encode(),
        &mut ui::widget::Edits::lend(&mut doc),
        &root,
    );
    assert_eq!(app.phase, AuthPhase::Idle);
    assert_eq!(app.notice, Notice::None);
    assert_eq!(app.failures, 0);
    assert!(Widget::take_focus_reset(&mut app));
    app.sync(&doc, &root);
    assert_eq!(
        focusables(&app).first().map(String::as_str),
        Some("greeter.password")
    );
    assert_eq!(
        app.login_user.as_ref().map(|u| u.username.as_str()),
        Some("grace")
    );

    // The handoff is over: a later refusal reads as usual.
    seed_password(&app, "wrongpw");
    app.begin_authenticating();
    app.on_external_event(
        &AuthUpdate::Failed(Failure::Credentials).encode(),
        &mut ui::widget::Edits::lend(&mut doc),
        &root,
    );
    assert_eq!(app.notice, Notice::Error("greeter-error-passcode"));
}

#[test]
fn a_boot_sign_in_for_an_unlisted_account_falls_back_to_the_named_form() {
    let mut app = GreeterApp::for_shot(AuthScenario::Accounts);
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    app.offer_boot_sign_in("svc");
    app.sync(&doc, &root);
    assert_eq!(app.phase, AuthPhase::Starting);
    assert_eq!(app.attempt_username(), "svc");
    app.on_external_event(
        &AuthUpdate::Failed(Failure::NotPermitted).encode(),
        &mut ui::widget::Edits::lend(&mut doc),
        &root,
    );
    assert_eq!(app.phase, AuthPhase::Idle);
    assert_eq!(app.notice, Notice::None);
    assert_eq!(typed_username(&app), "svc");
}

#[test]
fn several_accounts_are_listed_and_choosing_one_resets_the_form() {
    let mut app = GreeterApp::for_shot(AuthScenario::Accounts);
    let doc = AutoCommit::new();
    let root = automerge::ROOT;
    app.sync(&doc, &root);
    let ids = focusables(&app);
    assert_eq!(ids.first().map(String::as_str), Some("greeter.password"));
    assert!(ids.contains(&"greeter.account.ada".to_string()), "{ids:?}");
    assert!(
        ids.contains(&"greeter.account.grace".to_string()),
        "{ids:?}"
    );
    assert!(
        !ids.contains(&"greeter.account.kai".to_string()),
        "the chosen account is the one shown, not listed again"
    );
    seed_password(&app, "typed for kai");
    let _ = Widget::take_focus_reset(&mut app);
    let ada = app
        .users
        .iter()
        .position(|user| user.username == "ada")
        .expect("ada is listed");
    app.requests.account.set(Some(ada));
    app.sync(&doc, &root);
    assert_eq!(
        app.login_user.as_ref().map(|u| u.username.as_str()),
        Some("ada")
    );
    assert!(
        app.password_buffer.lock().unwrap().is_empty(),
        "a passcode never follows to another account"
    );
    assert_eq!(typed_username(&app), "ada");
    assert!(
        Widget::take_focus_reset(&mut app),
        "the keyboard returns to the passcode"
    );
    // A single account is never a list.
    let single = GreeterApp::for_shot(AuthScenario::Personal);
    assert!(
        !focusables(&single)
            .iter()
            .any(|id| id.starts_with("greeter.account."))
    );
}

#[test]
fn escape_clears_the_passcode_and_repeated_failures_say_so() {
    let mut app = GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
    let mut doc = AutoCommit::new();
    seed_password(&app, "half typed");
    app.notice = Notice::Error("greeter-error-passcode");
    dispatch(
        &mut app,
        &mut doc,
        "greeter.password",
        &named_key_event(NamedKey::Escape),
    );
    assert!(app.password_buffer.lock().unwrap().is_empty());
    assert_eq!(app.notice, Notice::None);

    for _ in 0..3 {
        app.begin_authenticating();
        app.finish_failure(Failure::Credentials);
    }
    assert_eq!(
        app.status_text(),
        "Incorrect password, 3 times in a row. Try again."
    );
    app.begin_authenticating();
    app.finish_failure(Failure::Locked);
    assert_eq!(
        app.failures, 0,
        "another kind of failure starts the count over"
    );
}

#[test]
fn the_accessibility_toggles_ask_the_host_once() {
    let mut app = GreeterApp::with_sessions(one_session());
    assert_eq!(app.take_accessibility_request(), None);
    app.requests
        .accessibility
        .set(Some(AccessibilityRequest::HighContrast(true)));
    assert_eq!(
        app.take_accessibility_request(),
        Some(AccessibilityRequest::HighContrast(true))
    );
    assert_eq!(app.take_accessibility_request(), None, "one-shot");
    app.requests
        .accessibility
        .set(Some(AccessibilityRequest::ScreenReader(true)));
    let _ = app.take_accessibility_request();
    assert!(app.screen_reader, "the toggle shows the reader as on");
    // Through the real App: the request reaches the theme.
    let greeter = GreeterApp::with_sessions(one_session());
    greeter
        .requests
        .accessibility
        .set(Some(AccessibilityRequest::HighContrast(true)));
    let mut host = ui::App::new(ui::State::new(), Box::new(greeter));
    assert!(host.apply_accessibility_request());
    assert!(host.high_contrast());
}

/// The on-screen keyboard as the laid-out screen shows it, no keyboard
/// being attached.
fn shown_keyboard(app: &mut GreeterApp) -> ui::passcode_keyboard::PasscodeKeyboard {
    app.keyboards = Some(false);
    layout_at_with_theme(app, (1280.0, 800.0), &Theme::dark());
    app.touch_keyboard.keyboard.get()
}

/// Where the passcode field and the on-screen keyboard are on the
/// laid-out screen.
fn field_and_keys(app: &GreeterApp) -> (Option<Rect>, Option<Rect>) {
    let card = app.composed.element().expect("composed");
    (
        passcode_field_rect(card),
        rect_of(card, "greeter.touch-keyboard"),
    )
}

#[test]
fn the_on_screen_keyboard_is_a_number_pad_for_a_pin_and_letters_for_a_password() {
    let mut pin = GreeterApp::for_shot(AuthScenario::PersonalPin);
    assert!(shown_keyboard(&mut pin).shows_pad());
    let field = passcode_node(pin.composed.element().expect("composed"));
    assert!(
        field
            .children
            .iter()
            .any(|child| child.name.as_deref() == Some("PIN")),
        "the field asks for the PIN by name: {field:?}"
    );

    let mut password = GreeterApp::for_shot(AuthScenario::Personal);
    assert!(!shown_keyboard(&mut password).shows_pad());

    // At boot the supervisor's list says, and choosing another person
    // opens their keyboard.
    let (_supervisor, link) = boot::Link::pair().unwrap();
    let mut app = GreeterApp::for_boot(BootLink::new(link), false);
    let person = |uid, name: &str, secret| boot::Person {
        uid,
        name: name.into(),
        display_name: name.into(),
        secret,
    };
    app.boot_message(ToGreeter::People {
        people: vec![
            person(1000, "ada", boot::SecretKind::Pin),
            person(1001, "ben", boot::SecretKind::Password),
        ],
        highlighted: 0,
    });
    assert!(shown_keyboard(&mut app).shows_pad());
    app.boot_message(ToGreeter::Ask {
        person: 1,
        note: Note::None,
        several: true,
    });
    assert!(!shown_keyboard(&mut app).shows_pad());
}

#[test]
fn the_boot_sign_in_shows_the_card_where_the_greeter_does() {
    let (_supervisor, link) = boot::Link::pair().unwrap();
    let mut boot_app = GreeterApp::for_boot(BootLink::new(link), false);
    boot_app.boot_message(ToGreeter::People {
        people: vec![boot::Person {
            uid: 1000,
            name: "ada".into(),
            display_name: "Ada".into(),
            secret: boot::SecretKind::Password,
        }],
        highlighted: 0,
    });
    let mut greeter = GreeterApp::for_shot(AuthScenario::PersonalNoFingerprint);
    boot_app.clock = greeter.clock.clone();
    boot_app.clock_pinned = true;
    boot_app.device = greeter.device;
    for viewport in [(1280.0, 800.0), (800.0, 1280.0), (1920.0, 1080.0)] {
        layout_at_with_theme(&mut boot_app, viewport, &Theme::dark());
        layout_at_with_theme(&mut greeter, viewport, &Theme::dark());
        let at_boot = boot_app.composed.element().expect("composed");
        let after = greeter.composed.element().expect("composed");
        assert!(
            rect_of(at_boot, "greeter.cant-sign-in").is_some(),
            "the boot offers the other ways in"
        );
        assert_eq!(
            passcode_field_rect(at_boot),
            passcode_field_rect(after),
            "at {viewport:?}"
        );
    }
}

#[test]
fn after_boot_cant_sign_in_sits_under_the_field_for_a_known_account() {
    let mut app = GreeterApp::for_shot(AuthScenario::PersonalNoFingerprint);
    layout_at_with_theme(&mut app, (1280.0, 800.0), &Theme::dark());
    let card = app.composed.element().expect("composed");
    let field = passcode_field_rect(card).expect("the passcode field");
    let button = rect_of(card, "greeter.cant-sign-in").expect("offered after boot");
    assert!(button.y0 >= field.y1, "{button:?} is under {field:?}");
    assert_eq!(
        focusables(&app)[..3],
        ["greeter.password", "greeter.login", "greeter.cant-sign-in"],
        "the next stop after signing in"
    );
    // Without a known account there is nobody to recover.
    let app = GreeterApp::with_sessions(one_session());
    assert!(!focusables(&app).contains(&"greeter.cant-sign-in".to_owned()));
}

fn recovering() -> (GreeterApp, AutoCommit) {
    let mut app = pinned(GreeterApp::with_sessions_and_login_user(
        one_session(),
        Some(quick_user()),
    ));
    let mut doc = AutoCommit::new();
    press(&mut app, &mut doc, "greeter.cant-sign-in");
    (app, doc)
}

fn fill_recovery(app: &GreeterApp, key: &str, new: &str, confirm: &str) {
    let recovery = app.recovery.as_ref().expect("the recovery form is open");
    for (buffer, value) in [
        (&recovery.key, key),
        (&recovery.new, new),
        (&recovery.confirm, confirm),
    ] {
        *buffer.lock().unwrap() = value.to_owned();
    }
}

fn recovery_buffers(app: &GreeterApp) -> [String; 3] {
    let recovery = app.recovery.as_ref().expect("the recovery form is open");
    [&recovery.key, &recovery.new, &recovery.confirm].map(|b| b.lock().unwrap().clone())
}

#[test]
fn cant_sign_in_after_boot_opens_the_recovery_form_and_cancel_returns() {
    let mut app = pinned(GreeterApp::with_sessions_and_login_user(
        one_session(),
        Some(quick_user()),
    ));
    let mut doc = AutoCommit::new();
    seed_password(&app, "half typed");
    press(&mut app, &mut doc, "greeter.cant-sign-in");
    assert!(app.recovery.is_some());
    assert!(app.password_buffer.lock().unwrap().is_empty());
    assert!(app.take_focus_reset(), "focus moves into the form");
    assert_eq!(
        focusables(&app)[..5],
        [
            "greeter.recovery-key",
            "greeter.new-secret",
            "greeter.confirm-secret",
            "greeter.recovery-cancel",
            "greeter.recovery-save",
        ],
        "the recovery key takes focus"
    );
    let card = card_of(&app);
    assert!(find_test_id(card.as_ref(), "greeter.password").is_none());
    assert!(find_test_id(card.as_ref(), "greeter.cant-sign-in").is_none());
    assert_eq!(
        accessible_name_for(card.as_ref(), "greeter.recovery-key").as_deref(),
        Some("Recovery key")
    );
    assert!(
        !shown_keyboard(&mut app).shows_pad(),
        "a recovery key is typed on the letters"
    );

    press(&mut app, &mut doc, "greeter.recovery-cancel");
    assert!(app.recovery.is_none());
    assert_eq!(
        focusables(&app).first().map(String::as_str),
        Some("greeter.password")
    );

    press(&mut app, &mut doc, "greeter.cant-sign-in");
    assert!(dispatch(
        &mut app,
        &mut doc,
        "greeter.recovery-key",
        &named_key_event(NamedKey::Escape)
    ));
    app.sync(&doc, &automerge::ROOT);
    assert!(app.recovery.is_none(), "Esc leaves the form too");
    assert!(untouched(&doc));
}

#[test]
fn mismatched_entries_are_refused_without_asking_tilo_authd() {
    let dir = tempfile::tempdir().unwrap();
    let listener = std::os::unix::net::UnixListener::bind(dir.path().join("socket")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (mut app, mut doc) = recovering();
    app.auth_socket = dir.path().join("socket");

    press(&mut app, &mut doc, "greeter.recovery-save");
    assert_eq!(
        app.recovery.as_ref().unwrap().notice,
        Notice::Hint("greeter-recovery-hint-key")
    );
    fill_recovery(&app, "cccc-dddd", "", "");
    press(&mut app, &mut doc, "greeter.recovery-save");
    assert_eq!(
        app.recovery.as_ref().unwrap().notice,
        Notice::Hint("greeter-recovery-hint-new")
    );
    assert_eq!(recovery_buffers(&app), ["cccc-dddd", "", ""]);

    fill_recovery(&app, "cccc-dddd", "2468", "1357");
    press(&mut app, &mut doc, "greeter.recovery-save");
    let recovery = app.recovery.as_ref().unwrap();
    assert_eq!(recovery.notice, Notice::Error("greeter-recovery-mismatch"));
    assert!(!recovery.is_pending());
    assert_eq!(
        recovery_buffers(&app),
        ["cccc-dddd", "", ""],
        "the two entries are typed again"
    );
    assert!(
        spoken(card_of(&app).as_ref())
            .contains(&"The two entries don't match. Type them again.".to_owned())
    );
    assert!(
        listener
            .accept()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::WouldBlock),
        "tilo-authd was not asked"
    );
    assert!(untouched(&doc));
}

/// Sync until tilo-authd's answer has been taken.
fn answered(app: &mut GreeterApp, doc: &AutoCommit) {
    let started = Instant::now();
    while app
        .recovery
        .as_ref()
        .is_some_and(recovery::Recovery::is_pending)
    {
        assert!(
            started.elapsed() < tilo_test_env::hang_bound(),
            "tilo-authd's answer was taken"
        );
        std::thread::sleep(Duration::from_millis(5));
        app.sync(doc, &automerge::ROOT);
    }
}

#[test]
fn the_recovery_key_sets_a_new_secret_and_a_wrong_one_says_so() {
    use std::io::{BufRead, BufReader, Write};
    let dir = tempfile::tempdir().unwrap();
    let listener = std::os::unix::net::UnixListener::bind(dir.path().join("socket")).unwrap();
    let daemon = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for reply in [
            "{\"ok\":false,\"error\":\"wrong\"}\n",
            "{\"ok\":false,\"error\":\"weak-secret\"}\n",
            "{\"ok\":true}\n",
        ] {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(&stream).read_line(&mut request).unwrap();
            (&stream).write_all(reply.as_bytes()).unwrap();
            requests.push(serde_json::from_str::<serde_json::Value>(&request).unwrap());
        }
        requests
    });
    let (mut app, mut doc) = recovering();
    app.auth_socket = dir.path().join("socket");

    for (refusal, said) in [
        (
            "greeter-recovery-wrong",
            "That recovery key isn't right. Check it and try again.",
        ),
        (
            "greeter-recovery-weak",
            "This computer has no security chip, so it needs a longer password. Choose a longer one.",
        ),
    ] {
        fill_recovery(&app, "cccc-dddd", "2468", "2468");
        press(&mut app, &mut doc, "greeter.recovery-save");
        assert!(app.recovery.as_ref().unwrap().is_pending());
        assert_eq!(recovery_buffers(&app), ["", "", ""]);
        answered(&mut app, &doc);
        assert_eq!(
            app.recovery.as_ref().unwrap().notice,
            Notice::Error(refusal)
        );
        assert_eq!(recovery_buffers(&app), ["", "", ""]);
        assert!(spoken(card_of(&app).as_ref()).contains(&said.to_owned()));
    }

    fill_recovery(&app, "cccc-dddd", "2468", "2468");
    press(&mut app, &mut doc, "greeter.recovery-save");
    answered(&mut app, &doc);
    assert!(app.recovery.is_none(), "back to the sign-in form");
    assert_eq!(app.notice, Notice::Hint("greeter-recovery-done-pin"));
    assert_eq!(app.status_text(), "Sign in with your new PIN.");
    assert_eq!(
        accessible_name_for(card_of(&app).as_ref(), "greeter.password").as_deref(),
        Some("PIN")
    );

    let requests = daemon.join().unwrap();
    assert_eq!(
        requests.last(),
        Some(&serde_json::json!({
            "op": "set-secret",
            "user": "tester",
            "recovery": "cccc-dddd",
            "new": "2468",
        }))
    );
    assert!(untouched(&doc), "no secret reaches the document");
}

#[test]
fn switching_between_the_pad_and_the_letters_keeps_the_card_in_place() {
    use ui::passcode_keyboard::PasscodeKey;
    let mut app = GreeterApp::for_shot(AuthScenario::PersonalPin);
    let mut keyboard = shown_keyboard(&mut app);
    let placed = field_and_keys(&app);
    assert!(placed.0.is_some() && placed.1.is_some(), "{placed:?}");

    keyboard.press(PasscodeKey::Letters);
    app.touch_keyboard.keyboard.set(keyboard);
    assert!(
        !shown_keyboard(&mut app).shows_pad(),
        "a PIN may have letters"
    );
    assert_eq!(field_and_keys(&app), placed);

    keyboard.press(PasscodeKey::Symbols);
    app.touch_keyboard.keyboard.set(keyboard);
    assert!(
        shown_keyboard(&mut app).shows_pad(),
        "?123 comes back to the pad"
    );
    assert_eq!(field_and_keys(&app), placed);
}

/// What the screen reader said, through the toolkit's recording speaker.
#[derive(Clone, Default)]
struct Heard(Arc<Mutex<tts::NullSpeaker>>);

impl Heard {
    fn spoken(&self) -> Vec<String> {
        self.0.lock_recover().spoken().to_vec()
    }
}

impl tts::Speaker for Heard {
    fn speak_utterance(&mut self, utterance: tts::Utterance) {
        self.0.lock_recover().speak_utterance(utterance);
    }
    fn stop(&mut self) {
        self.0.lock_recover().stop();
    }
    fn set_rate(&mut self, rate: f32) {
        self.0.lock_recover().set_rate(rate);
    }
    fn set_voice(&mut self, voice: &str) {
        self.0.lock_recover().set_voice(voice);
    }
    fn is_speaking(&self) -> bool {
        self.0.lock_recover().is_speaking()
    }
    fn backend_name(&self) -> &'static str {
        "heard"
    }
}

/// A tablet's touchscreen with no keyboard, as the KMS host shows the
/// login screen on it.
#[derive(Default)]
struct Touchscreen {
    emulator: std::cell::RefCell<ui::TouchPointerEmulator>,
}

impl ui::app::AppSurface for Touchscreen {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(1280, 800)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _cursor: winit::window::CursorIcon) {}
}

impl Touchscreen {
    fn frame(&self, app: &mut ui::App) {
        let mut scene = vello::Scene::new();
        for _ in 0..3 {
            scene.reset();
            app.surface_frame(self, &mut scene);
        }
    }

    /// A finger at `at`, `t` ms after `zero`, through the host's touch path.
    fn finger(
        &self,
        app: &mut ui::App,
        zero: ui::time::Instant,
        (id, phase): (u64, winit::event::TouchPhase),
        at: vello::kurbo::Point,
        t: u64,
    ) {
        let touch = winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(at.x, at.y),
            force: None,
            id,
        };
        let now = after(zero, t);
        ui::time::testing::with_monotonic_time(now, || {
            app.surface_touch(self, touch, &mut self.emulator.borrow_mut())
        });
    }
}

/// `t` ms after `zero`.
fn after(zero: ui::time::Instant, t: u64) -> ui::time::Instant {
    zero.checked_add(std::time::Duration::from_millis(t))
        .expect("a test's instant")
}

/// Where the screen reader finds the node called `name` with `role`.
fn semantic_centre(app: &mut ui::App, role: ui::a11y::Role, name: &str) -> vello::kurbo::Point {
    app.refresh_semantic_tree();
    app.semantic_tree()
        .iter()
        .find(|node| node.role == role && node.name.as_deref() == Some(name))
        .and_then(|node| node.bounds)
        .unwrap_or_else(|| panic!("{role:?} {name} is on the screen"))
        .center()
}

#[test]
fn with_the_screen_reader_on_a_pin_is_entered_by_touch_and_never_spoken() {
    use winit::event::TouchPhase::{Ended, Started};
    let mut greeter = GreeterApp::for_shot(AuthScenario::PersonalPin);
    greeter.keyboards = Some(false);
    let typed = Arc::clone(&greeter.password_buffer);
    let mut app = ui::App::new(ui::State::new(), Box::new(greeter));
    app.autofocus_first = true;
    app.autofocus_on_structure_change = true;
    let heard = Heard::default();
    app.set_screen_reader(Some(ui::screen_reader::ScreenReader::with_speaker(
        Box::new(heard.clone()),
    )));
    let screen = Touchscreen::default();
    screen.frame(&mut app);
    let zero = ui::time::now();
    let anywhere = vello::kurbo::Point::new(640.0, 120.0);

    for (index, digit) in ["2", "5", "8", "0"].into_iter().enumerate() {
        let key = semantic_centre(&mut app, ui::a11y::Role::Button, digit);
        let base = u64::try_from(index).unwrap().saturating_mul(2_000);
        let before = heard.spoken().len();
        // Rest a finger on the key: it is read, and nothing is typed.
        screen.finger(&mut app, zero, (1, Started), key, base);
        assert!(app.tick_screen_reader(after(zero, base.saturating_add(200))));
        screen.finger(&mut app, zero, (1, Ended), key, base.saturating_add(250));
        screen.frame(&mut app);
        assert_eq!(
            heard.spoken().get(before..),
            Some(&[format!("{digit}, button")][..]),
            "the key under the finger is read"
        );
        assert_eq!(typed.lock_recover().len(), index, "exploring types nothing");
        // A double tap away from the keyboard presses the key read.
        screen.finger(
            &mut app,
            zero,
            (2, Started),
            anywhere,
            base.saturating_add(600),
        );
        screen.finger(
            &mut app,
            zero,
            (2, Ended),
            anywhere,
            base.saturating_add(650),
        );
        screen.finger(
            &mut app,
            zero,
            (3, Started),
            anywhere,
            base.saturating_add(750),
        );
        screen.finger(
            &mut app,
            zero,
            (3, Ended),
            anywhere,
            base.saturating_add(800),
        );
        screen.frame(&mut app);
    }
    assert_eq!(*typed.lock_recover(), "2580");

    // The field is read as protected, with nothing of what it holds.
    let field = semantic_centre(&mut app, ui::a11y::Role::TextInput, "PIN");
    screen.finger(&mut app, zero, (4, Started), field, 10_000);
    app.tick_screen_reader(after(zero, 10_200));
    let said = heard.spoken();
    let last = said.last().expect("the field is read");
    assert!(
        last.starts_with("PIN") && last.contains("protected"),
        "{last}"
    );

    // Each digit was said only as the key's name, never echoed from the
    // field, and the PIN itself, or any run of it, never.
    for utterance in &said {
        assert!(
            !["2", "5", "8", "0"].contains(&utterance.as_str()),
            "a typed digit was echoed: {said:?}"
        );
        for run in ["25", "58", "80", "258", "2580"] {
            assert!(!utterance.contains(run), "{utterance} in {said:?}");
        }
    }
}

#[test]
fn keys_from_the_on_screen_keyboard_reach_the_passcode_a_delete_too() {
    use ui::passcode_keyboard::PasscodeInput;
    let mut app = GreeterApp::for_shot(AuthScenario::PersonalPin);
    shown_keyboard(&mut app);
    app.touch_keyboard.typed.borrow_mut().extend([
        PasscodeInput::Text("1".into()),
        PasscodeInput::Text("2".into()),
        PasscodeInput::Backspace,
        PasscodeInput::Text("3".into()),
    ]);
    app.take_touch_keys();
    assert_eq!(*app.password_buffer.lock_recover(), "13");
}

/// Run `operation` with the monotonic clock `t` ms after `zero` and the
/// wall clock `t` ms after `wall_ms`.
fn in_time<T>(zero: ui::time::Instant, wall_ms: u64, t: u64, operation: impl FnOnce() -> T) -> T {
    let wall = ui::time::UNIX_EPOCH
        .checked_add(Duration::from_millis(wall_ms.saturating_add(t)))
        .expect("a wall time");
    ui::time::testing::with_monotonic_time(after(zero, t), || {
        ui::time::testing::with_wall(wall, operation)
    })
}

/// The boot sign-in on the KMS host, at rest for ten minutes: the host
/// sleeps until the deadline the app gives it, and the screen shows each
/// minute as it turns, waking about once a minute and never on a tick of
/// its own.
#[test]
fn at_rest_on_the_kms_host_the_clock_turns_each_minute_with_one_wake() {
    use chrono::TimeZone as _;
    let started = chrono::Local
        .with_ymd_and_hms(2026, 6, 30, 10, 41, 20)
        .earliest()
        .expect("a local time");
    let wall_ms = u64::try_from(started.timestamp_millis()).expect("after the epoch");
    let zero = ui::time::now();
    let shown = |app: &mut ui::App, t: u64| {
        let time = in_time(zero, wall_ms, t, || {
            ui::clock::time_of_day(&ui::clock::local_now())
        });
        app.refresh_semantic_tree();
        app.semantic_tree()
            .iter()
            .any(|node| node.name.as_deref() == Some(time.as_str()))
            .then_some(time)
    };

    let greeter = in_time(
        zero,
        wall_ms,
        0,
        || GreeterApp::with_sessions(one_session()),
    );
    let mut app = ui::App::new(ui::State::new(), Box::new(greeter));
    let screen = Touchscreen::default();
    let mut scene = vello::Scene::new();
    // The host's loop as `ui_kms::Host::run` runs it with no input: a frame
    // when one is due or the last asked for another, then a sleep until
    // the next deadline.
    let (mut t, mut wants) = (0_u64, true);
    let (mut frame_wakes, mut idle_wakes, mut turns) = (0_u32, 0_u32, 0_u32);
    let mut last = None;
    while t < 600_000 {
        let next = in_time(zero, wall_ms, t, || {
            if wants || ui_kms::due(&app, ui::time::now()) {
                scene.reset();
                wants = app.surface_frame(&screen, &mut scene);
            }
            ui_kms::next_deadline(&app)
        });
        let now_shown = shown(&mut app, t);
        assert!(
            now_shown.is_some(),
            "at {t} ms the screen shows the time it is"
        );
        if last.is_some() && now_shown != last {
            turns += 1;
        }
        last = now_shown;
        let next = next.expect("the clock always asks for a wake");
        let wait = u64::try_from(next.saturating_duration_since(after(zero, t)).as_millis())
            .expect("a wait in ms");
        if wants || wait < 1_000 {
            // Something moves: the host draws at the refresh rate.
            frame_wakes += 1;
            t = t.saturating_add(if wants { 16 } else { wait.max(1) });
        } else {
            idle_wakes += 1;
            t = t.saturating_add(wait);
        }
    }
    eprintln!("ten minutes at rest: {idle_wakes} idle wakes, {frame_wakes} frame wakes");
    // 10:41:20 to 10:51:20 turns ten times, at 10:42 through 10:51.
    assert_eq!(turns, 10, "each minute is shown as it turns");
    assert!(
        (10..=11).contains(&idle_wakes),
        "about one wake a minute at rest, saw {idle_wakes}"
    );
    assert!(
        frame_wakes < 200,
        "only the first moments draw frame by frame, saw {frame_wakes}"
    );
}
