use super::*;
use automerge::{AutoCommit, ReadDoc};
use greetd_ipc::codec::SyncCodec as _;
use greetd_ipc::{AuthMessageType, ErrorType, Request, Response};
use std::collections::HashMap;
use std::os::unix::net::UnixListener;
use taffy::AvailableSpace;
use tilo_ui_base::a11y::Politeness;
use tilo_ui_base::presence::PresenceCtx;
use tilo_ui_base::widget::DrawChild;
use tilo_ui_base::widget::find_test_id;

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
    let event =
        tilo_ui_base::synthetic_input::key_event(key, text.map(Into::into), ElementState::Pressed);
    tilo_ui_base::synthetic_input::keyboard_input(event)
}

fn named_key_event(key: NamedKey) -> WindowEvent {
    key_event(Key::Named(key), None)
}

fn char_event(c: &str) -> WindowEvent {
    key_event(Key::Character(c.into()), Some(c))
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

/// One sign-in screen driven the way a person's machine drives it: a press
/// publishes the control's own action and the host answers it, keys go through
/// the laid-out widget tree, and Automerge is lent exactly as the host lends it
/// (the greeter must never write what a person types into it).
struct Screen {
    app: GreeterApp,
    doc: AutoCommit,
    viewport: (f32, f32),
    theme: Theme,
}

impl Screen {
    /// A personal machine after its first boot: one known account, a still
    /// clock so a test's minute never turns.
    fn personal() -> Self {
        Self::of(GreeterApp::with_sessions_and_login_user(
            one_session(),
            Some(quick_user()),
        ))
    }

    /// The screen with `app`, its clock pinned so a test's minute never turns.
    fn of(mut app: GreeterApp) -> Self {
        if !app.clock_pinned {
            app.clock = ClockFace {
                time: "09:41".into(),
                date: "Saturday, October 4".into(),
            };
            app.clock_pinned = true;
        }
        Self {
            app,
            doc: AutoCommit::new(),
            viewport: (1280.0, 800.0),
            theme: Theme::dark(),
        }
    }

    /// A pre-canned shot state, as the headless `--shots` path renders it.
    fn shot(scenario: AuthScenario) -> Self {
        Self::of(GreeterApp::for_shot(scenario))
    }

    fn sync(&mut self) {
        self.app.sync(&self.doc, &automerge::ROOT);
    }

    fn card(&self) -> Box<dyn Widget> {
        card_of(&self.app)
    }

    /// Lay the page out at the size and theme it is being viewed at.
    fn layout(&mut self) -> &mut Self {
        let _ = Laid::new(&mut self.app, &self.doc, self.viewport, &self.theme);
        self
    }

    fn at(&mut self, viewport: (f32, f32)) -> &mut Self {
        self.viewport = viewport;
        self
    }

    /// Activate the control `id` as Enter, Space or assistive technology
    /// does, then let the greeter take what it reported.
    ///
    /// A press publishes the control's own action; the answer a window's host
    /// gives it is what runs the greeter's handler, so the test answers it here.
    fn press(&mut self, id: &str) {
        self.app.sync(&self.doc, &automerge::ROOT);
        self.app
            .composed
            .replace(self.app.build_composition(&Theme::dark()));
        let composed = self
            .app
            .composed
            .element_mut()
            .expect("the greeter composed");
        let control = find_mut(composed, id).unwrap_or_else(|| panic!("{id} is on the screen"));
        assert!(
            control.perform_action(
                &tilo_ui_base::a11y::Action::Activate,
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut self.doc),
                &automerge::ROOT
            ),
            "{id} accepts activation"
        );
        ui::widget::resources::start_pending_control_invocations(
            composed,
            &mut tilo_ui_base::widget::Edits::lend(&mut self.doc),
            &automerge::ROOT,
        );
        self.sync();
    }

    /// Type `text` into the field `id`, one keystroke at a time, through the
    /// laid-out widget tree.
    fn type_text(&mut self, id: &str, text: &str) {
        for c in text.chars() {
            self.key(id, &char_event(&c.to_string()));
        }
        self.sync();
    }

    /// Deliver `event` to the laid-out screen with `focused` holding focus.
    /// Answers whether the focused widget consumed it.
    fn key(&mut self, focused: &str, event: &WindowEvent) -> bool {
        self.app.sync(&self.doc, &automerge::ROOT);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = self.app.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: AvailableSpace::Definite(1280.0),
                    height: AvailableSpace::Definite(800.0),
                },
            )
            .unwrap();
        self.app.apply_layout(&taffy, node, Point::ZERO);
        set_focus_by_id(&mut self.app, focused);

        ui::testing::Dispatch::new()
            .doc(&mut self.doc)
            .layout(&taffy, node)
            .send(&mut self.app, event)
            .handled
    }

    fn enter(&mut self, focused: &str) -> bool {
        self.key(focused, &named_key_event(NamedKey::Enter))
    }

    fn escape(&mut self, focused: &str) -> bool {
        self.key(focused, &named_key_event(NamedKey::Escape))
    }

    fn focusables(&self) -> Vec<String> {
        focusables(&self.app)
    }
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
    fn said(node: tilo_ui_base::a11y::AccessNode, out: &mut Vec<String>) {
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
    fn named<'a>(
        node: &'a tilo_ui_base::a11y::AccessNode,
        name: &str,
    ) -> Option<&'a tilo_ui_base::a11y::AccessNode> {
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
fn passcode_node(w: &dyn Widget) -> tilo_ui_base::a11y::AccessNode {
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
        .find(|child| child.role == tilo_ui_base::a11y::Role::TextInput)?;
    let b = field.bounds?;
    Some(Rect::new(b.x0, b.y0, b.x1, b.y1))
}

/// A settled greeter laid out the way App does it, kept alive so it can
/// be relaid out in place after input — exactly the per-keystroke path.
struct Laid {
    taffy: TaffyTree<()>,
    node: NodeId,
    node_map: HashMap<NodeId, *mut dyn Widget>,
    bindings: tilo_ui_base::widget::bound_nodes::BoundNodeMap,
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
                bindings: tilo_ui_base::widget::bound_nodes::BoundNodeMap::new(),
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
            tilo_ui_base::widget::compute_widget_layout(
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
    app.draw_child(
        &mut scene,
        Affine::IDENTITY,
        text,
        theme,
        &PresenceCtx::default().draw_env(),
    );
}

fn scene_bytes(app: &mut GreeterApp) -> (Vec<u32>, Vec<u32>) {
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    app.draw_child(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &Theme::dark(),
        &PresenceCtx::default().draw_env(),
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

fn only_bob_has_a_fingerprint(username: &str) -> bool {
    username == "bob"
}

/// Seed the process-local passcode buffer without touching the document.
fn seed_password(app: &GreeterApp, pw: &str) {
    let mut value = app.password_buffer.lock_recover();
    value.zeroize();
    value.push_str(pw);
}

/// A person types their name and password on their own machine and signs in,
/// once: the greeting names them, the field holds the passcode and nothing
/// else does, and the attempt is started exactly once.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one person's first sign-in, from the greeting to the session starting"
)]
fn a_person_signs_in_on_a_device_of_theirs() {
    let screen = Screen::personal();
    assert_eq!(
        typed_username(&screen.app),
        "tester",
        "the username is seeded without asking the user to type it"
    );
    assert_eq!(
        screen.focusables(),
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
    let card = screen.card();
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

    // The toolkit host seats focus on the passcode before a key is typed.
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

    // A machine whose accounts are unknown asks for the name too, and says
    // where the person is signing in.
    let unknown = Screen::of(GreeterApp::with_sessions_and_login_user(
        one_session(),
        None,
    ));
    let card = unknown.card();
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
        unknown.focusables().first().map(String::as_str),
        Some("greeter.username")
    );

    // The greeting reads the person before it asks for the passcode.
    let a_shot = Screen::shot(AuthScenario::Personal);
    let said = spoken(a_shot.card().as_ref());
    let person = said.iter().position(|s| s == "Kai Example");
    let passcode = said.iter().position(|s| s == "Password");
    assert!(person.is_some() && person < passcode, "{said:?}");

    // Signing in with no name says what is missing, and writes nothing.
    let mut form = Screen::of(GreeterApp::with_sessions(test_sessions()));
    form.press("greeter.login");
    assert_eq!(form.app.notice, Notice::Hint("greeter-hint-username"));
    assert_eq!(form.app.status_text(), "Enter your username.");
    assert!(
        !form.app.requests.sign_in.get(),
        "the request is taken once"
    );
    assert!(
        untouched(&form.doc),
        "an attempt writes nothing to the document"
    );

    // What the person types stays out of the document and moves the form on.
    form.type_text("greeter.username", "alice");
    assert_eq!(typed_username(&form.app), "alice");
    assert!(
        form.doc
            .get(&automerge::ROOT, "greeter.username")
            .unwrap()
            .is_none(),
        "the typed username stays out of the document"
    );
    assert!(form.enter("greeter.username"));
    form.sync();
    assert_eq!(form.app.notice, Notice::Hint("greeter-hint-passcode"));
    assert_eq!(form.app.phase, AuthPhase::Idle);

    // Only the text fields submit the form; the footer's controls never do.
    for control in [
        "greeter.session",
        "greeter.fingerprint",
        "greeter.larger-text",
        "greeter.high-contrast",
        "greeter.screen-reader",
        "greeter.power",
    ] {
        let mut screen = Screen::of(GreeterApp::with_sessions(test_sessions()));
        let handled = screen.enter(control);
        assert!(
            !screen.app.requests.sign_in.get(),
            "Enter on {control} must not submit the passcode form"
        );
        if control == "greeter.session" || control == "greeter.power" {
            assert!(handled, "the focused dropdown consumes Enter itself");
        } else {
            assert!(!handled, "{control} remains available to App activation");
        }
    }
    for field in ["greeter.username", "greeter.password"] {
        let mut screen = Screen::of(GreeterApp::with_sessions(test_sessions()));
        assert!(screen.enter(field));
        assert!(
            screen.app.requests.sign_in.get(),
            "Enter in {field} signs in"
        );
        screen.sync();
        assert_eq!(screen.app.notice, Notice::Hint("greeter-hint-username"));
        assert!(untouched(&screen.doc));
    }
    // The hint is said under the field, in words.
    let mut screen = Screen::of(GreeterApp::with_sessions(test_sessions()));
    screen.app.set_hint("greeter-hint-passcode");
    assert_eq!(screen.app.status_text(), "Enter your password.");
    assert_eq!(
        live_politeness_for(screen.card().as_ref(), "Enter your password."),
        Some(Politeness::Assertive),
        "a hint is said under the field"
    );

    // A passcode is held only in memory, is handed over exactly once and is
    // never written to the document or its serialized history.
    let mut screen = Screen::personal();
    seed_password(&screen.app, "correct horse battery staple");
    assert!(
        screen
            .doc
            .get(&automerge::ROOT, "greeter.password")
            .unwrap()
            .is_none()
    );
    assert!(
        !screen
            .doc
            .save()
            .windows("correct horse battery staple".len())
            .any(|bytes| bytes == b"correct horse battery staple"),
        "the serialized Automerge history must not contain the passcode"
    );
    screen.sync();
    assert!(untouched(&screen.doc));
    let secret = screen
        .app
        .take_auth_secret(AuthMethod::Password)
        .expect("local passcode is available exactly for authentication");
    assert_eq!(secret.as_str(), "correct horse battery staple");
    assert!(screen.app.password_buffer.lock().unwrap().is_empty());

    // The sign-in action and the fingerprint each start one attempt, and a
    // fingerprint attempt scrubs a passcode typed before it.
    let mut screen = Screen::personal();
    screen.press("greeter.login");
    assert_eq!(screen.app.notice, Notice::Hint("greeter-hint-passcode"));
    assert!(!screen.app.requests.sign_in.get(), "one press, one attempt");
    seed_password(&screen.app, "typed before the finger");
    screen.press("greeter.fingerprint");
    // The test runner is not greetd, so the attempt ends at asking for it.
    assert_eq!(screen.app.notice, Notice::Error("greeter-error-service"));
    assert!(
        screen.app.password_buffer.lock().unwrap().is_empty(),
        "a fingerprint attempt scrubs a passcode typed before it"
    );
    assert!(
        !screen.app.requests.fingerprint.get(),
        "one press, one attempt"
    );
    assert!(
        untouched(&screen.doc),
        "attempts write nothing to the document"
    );

    // An attempt that cannot reach greetd fails as such and says nothing
    // about the socket.
    let mut screen = Screen::personal();
    screen.sync();
    seed_password(&screen.app, "pw");
    assert!(std::env::var_os("GREETD_SOCK").is_none());
    screen.app.attempt_login(AuthMethod::Password);
    assert_eq!(screen.app.notice, Notice::Error("greeter-error-service"));
    assert!(!screen.app.status_text().contains("GREETD_SOCK"));

    // Nothing starts a second attempt while one runs, and the passcode stays
    // until the conversation takes it.
    for phase in [AuthPhase::Authenticating, AuthPhase::Starting] {
        let mut screen = Screen::personal();
        screen.sync();
        screen.app.phase = phase;
        screen.app.username.set("alice".to_owned());
        seed_password(&screen.app, "pw");
        screen.app.requests.sign_in.set(true);
        screen.sync();
        assert_eq!(screen.app.phase, phase);
        assert!(
            !screen.app.requests.sign_in.get(),
            "the request is taken, not kept"
        );
        assert_eq!(&*screen.app.password_buffer.lock().unwrap(), "pw");
    }

    // While it runs the screen says what it is doing, once, and the prompt
    // says it is busy.
    let mut screen = Screen::personal();
    screen.app.begin_authenticating();
    assert_eq!(screen.app.status_text(), "Signing in…");
    let card = screen.card();
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
    screen.app.on_external_event(
        &AuthUpdate::Prompt {
            text: "Place your finger on the reader".to_string(),
            is_error: false,
        }
        .encode(),
        &mut tilo_ui_base::widget::Edits::lend(&mut screen.doc),
        &automerge::ROOT,
    );
    assert_eq!(screen.app.phase, AuthPhase::Authenticating);
    assert_eq!(screen.app.status_text(), "Place your finger on the reader");
    screen.app.on_external_event(
        &AuthUpdate::Accepted.encode(),
        &mut tilo_ui_base::widget::Edits::lend(&mut screen.doc),
        &automerge::ROOT,
    );
    assert_eq!(screen.app.phase, AuthPhase::Starting);
    assert!(
        screen.app.phase.is_busy(),
        "inputs stay inert while handing over"
    );
    assert_eq!(screen.app.status_text(), "Starting your session…");
}

/// A wrong password is refused, said plainly, and leaves a form the person
/// can type in again — for every way an attempt can fail.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one table row per failure, then the same ready-form checks for each"
)]
fn a_refused_passcode_leaves_a_ready_form_and_the_next_attempt_works() {
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
        let mut screen = if personal {
            Screen::personal()
        } else {
            Screen::of(GreeterApp::with_sessions(one_session()))
        };
        screen.sync();
        if !personal {
            screen.app.username.set("alice".to_owned());
        }
        seed_password(&screen.app, "wrongpw");
        screen.app.begin_authenticating();
        let _ = Widget::take_focus_reset(&mut screen.app);

        screen.app.on_external_event(
            &AuthUpdate::Failed(failure).encode(),
            &mut tilo_ui_base::widget::Edits::lend(&mut screen.doc),
            &automerge::ROOT,
        );

        assert_eq!(
            screen.app.phase,
            AuthPhase::Idle,
            "{failure:?} returns to the form"
        );
        assert_eq!(screen.app.status_text(), sentence, "{failure:?}");
        assert!(matches!(screen.app.notice, Notice::Error(_)));
        assert!(
            screen.app.password_buffer.lock().unwrap().is_empty(),
            "{failure:?} clears the passcode"
        );
        if !personal {
            assert_eq!(
                typed_username(&screen.app),
                "alice",
                "the username survives a failure"
            );
        }
        assert!(
            Widget::take_focus_reset(&mut screen.app),
            "{failure:?} puts the keyboard back on the form"
        );
        let card = screen.card();
        assert_eq!(
            live_politeness_for(card.as_ref(), sentence),
            Some(Politeness::Assertive)
        );
        assert!(
            screen
                .focusables()
                .contains(&"greeter.password".to_string()),
            "the passcode field is editable again"
        );
    }

    // The host's autofocus brings the caret back to the passcode, not to a
    // footer control.
    let mut greeter = GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
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

    // The form's notes are announced at the urgency each deserves.
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
        let screen = Screen::shot(scenario);
        assert_eq!(
            live_politeness_for(screen.card().as_ref(), message),
            Some(expected),
            "{scenario:?} feedback must be announced at the right urgency"
        );
    }

    // Escape throws away a half-typed passcode and the note about it, and
    // repeated failures count up until another kind of failure starts over.
    let mut screen = Screen::personal();
    seed_password(&screen.app, "half typed");
    screen.app.notice = Notice::Error("greeter-error-passcode");
    screen.escape("greeter.password");
    assert!(screen.app.password_buffer.lock().unwrap().is_empty());
    assert_eq!(screen.app.notice, Notice::None);

    for _ in 0..3 {
        screen.app.begin_authenticating();
        screen.app.finish_failure(Failure::Credentials);
    }
    assert_eq!(
        screen.app.status_text(),
        "Incorrect password, 3 times in a row. Try again."
    );
    screen.app.begin_authenticating();
    screen.app.finish_failure(Failure::Locked);
    assert_eq!(
        screen.app.failures, 0,
        "another kind of failure starts the count over"
    );
}

/// A machine with a second session installed, and one with several accounts,
/// offers each as a choice; a machine with one of either never shows a picker.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the sessions directory, the session picker and the account list, each on the machine that has it"
)]
fn a_machine_with_a_choice_offers_it() {
    // The image installs os/usr/share/wayland-sessions as-is, so this is
    // exactly what the greeter on a fresh install reads: the one Tilo session,
    // which signing in starts.
    {
        let shipped = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../os/usr/share/wayland-sessions");
        let sessions = read_sessions(&shipped);
        let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Tilo"]);

        let app = GreeterApp::with_sessions(sessions);
        assert!(!app.offers_session_choice());
        assert!(find_test_id(card_of(&app).as_ref(), "greeter.session").is_none());
        assert_eq!(app.console_state()["session_choice"], false);
        assert_eq!(app.session_label(), "Tilo");
        assert_eq!(
            app.sessions[app.selected_session].argv,
            ["/usr/libexec/tilo-session", "--backend", "drm"]
        );
    }
    {
        let screen = Screen::of(GreeterApp::with_sessions(one_session()));
        assert!(!screen.app.offers_session_choice());
        assert!(
            !screen.focusables().contains(&"greeter.session".to_string()),
            "a single session must not render a picker: {:?}",
            screen.focusables()
        );
        assert!(find_test_id(screen.card().as_ref(), "greeter.session").is_none());
        assert_eq!(screen.app.console_state()["session_choice"], false);
    }
    {
        let screen = Screen::of(GreeterApp::with_sessions(test_sessions()));
        assert!(screen.app.offers_session_choice());
        assert_eq!(screen.app.console_state()["session_choice"], true);
        assert_eq!(
            screen.focusables(),
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
        assert_eq!(
            screen.app.session_label(),
            "Tilo",
            "the human name, not the id"
        );
    }
    {
        let mut screen = Screen::of(GreeterApp::with_sessions(test_sessions()));

        // The picker starts on the session that signing in would start.
        screen.sync();
        assert_eq!(screen.app.selected_session, 0);
        assert_eq!(screen.app.session_choice.with(Clone::clone), "Tilo");

        screen.app.session_choice.set("Weston".to_owned());
        screen.sync();
        assert_eq!(screen.app.selected_session, 1);
        assert_eq!(
            screen.app.sessions[screen.app.selected_session].argv,
            ["/usr/bin/weston"],
            "the choice must reach the argv handed to greetd"
        );

        // A label that names no known session leaves the choice alone.
        screen
            .app
            .session_choice
            .set("a session that went away".to_owned());
        screen.sync();
        assert_eq!(
            screen.app.selected_session, 1,
            "choice survives a stale label"
        );
        assert!(
            untouched(&screen.doc),
            "the choice stays out of the document"
        );
    }

    // With no session at all there is nothing to start, and signing in says so.
    {
        let mut screen = Screen::of(GreeterApp::with_sessions_and_login_user(
            Vec::new(),
            Some(quick_user()),
        ));
        screen.sync();
        seed_password(&screen.app, "pw");
        screen.app.attempt_login(AuthMethod::Password);
        assert_eq!(screen.app.notice, Notice::Error("greeter-error-no-session"));
        assert_eq!(screen.app.phase, AuthPhase::Idle);
    }

    // A fingerprint is offered only for an account that is enrolled, and its
    // conversation begins without a dummy password.
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
        let mut screen = Screen::of(GreeterApp::with_sessions_login_user_and_fingerprint(
            one_session(),
            Some(quick_user()),
            FingerprintAvailability::Unavailable,
        ));
        assert!(
            !screen
                .focusables()
                .contains(&"greeter.fingerprint".to_string()),
            "an unavailable factor must not appear in the form or focus order"
        );
        screen.sync();
        screen.app.attempt_login(AuthMethod::Fingerprint);
        assert_eq!(
            screen.app.notice,
            Notice::Hint("greeter-hint-no-fingerprint")
        );
        assert_eq!(screen.app.phase, AuthPhase::Idle);
    }
    {
        let mut screen = Screen::of(GreeterApp::with_sessions_login_user_and_fingerprint(
            one_session(),
            None,
            FingerprintAvailability::CheckNamedAccount,
        ));
        screen.app.fingerprint_probe = only_bob_has_a_fingerprint;

        assert!(
            screen
                .focusables()
                .contains(&"greeter.fingerprint".to_string())
        );
        assert!(screen.app.fingerprint_available_for("bob"));
        assert!(!screen.app.fingerprint_available_for("alice"));
        assert!(!screen.app.fingerprint_available_for(""));
    }
    {
        let mut screen = Screen::of(GreeterApp::with_sessions(one_session()));
        screen.sync();
        assert_eq!(
            screen.app.take_auth_secret(AuthMethod::Password),
            Err("greeter-hint-passcode")
        );
        assert_eq!(
            screen
                .app
                .take_auth_secret(AuthMethod::Fingerprint)
                .map(|secret| secret.to_string()),
            Ok(String::new()),
            "fingerprint PAM conversation must begin without a dummy password"
        );
    }
    {
        let screen = Screen::shot(AuthScenario::Fingerprint);
        assert_eq!(screen.app.phase, AuthPhase::Authenticating);
        assert_eq!(screen.app.status_text(), "Place your finger on the reader");
        assert_eq!(
            live_politeness_for(screen.card().as_ref(), "Place your finger on the reader"),
            Some(Politeness::Assertive),
            "a time-sensitive PAM instruction must be announced without moving focus"
        );
    }

    // Several accounts are listed in database order and choosing one brings
    // its own blank form and its own keyboard.
    {
        let mut screen = Screen::shot(AuthScenario::Accounts);
        screen.sync();
        let ids = screen.focusables();
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
        seed_password(&screen.app, "typed for kai");
        let _ = Widget::take_focus_reset(&mut screen.app);
        let ada = screen
            .app
            .users
            .iter()
            .position(|user| user.username == "ada")
            .expect("ada is listed");
        screen.app.requests.account.set(Some(ada));
        screen.sync();
        assert_eq!(
            screen.app.login_user.as_ref().map(|u| u.username.as_str()),
            Some("ada")
        );
        assert!(
            screen.app.password_buffer.lock().unwrap().is_empty(),
            "a passcode never follows to another account"
        );
        assert_eq!(typed_username(&screen.app), "ada");
        assert!(
            Widget::take_focus_reset(&mut screen.app),
            "the keyboard returns to the passcode"
        );
        // A single account is never a list.
        let single = Screen::shot(AuthScenario::Personal);
        assert!(
            !single
                .focusables()
                .iter()
                .any(|id| id.starts_with("greeter.account."))
        );
    }
}

/// The environment a signed-in session starts with is exactly the fixed
/// desktop it needs plus the diagnostics a caller explicitly turned on.
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

/// The reported laptop: a 2944x1840 panel at scale 2 beside a 1080p monitor
/// on a USB-C hub, as `cage` places them at the window's own scale.
fn laptop() -> (crate::output_scale::Displays, Rect, Rect) {
    use crate::output_scale::{Display, Displays, Layout};
    let panel = Rect::new(0.0, 0.0, 1472.0, 920.0);
    let monitor = Rect::new(1472.0, 0.0, 3392.0, 1080.0);
    (
        Displays::fixed(Layout {
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
        }),
        panel,
        monitor,
    )
}

/// The sign-in screen is laid out on the outputs the kiosk gives it: the card
/// on the built-in panel, the backdrop across every output, and the form on
/// screen at every size and text scale. Its widget tree mirrors its layout
/// tree, typing never rebuilds or moves it, and each frame fits the device.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one screen, laid out and painted on each machine it runs on"
)]
fn the_screen_is_laid_out_on_the_outputs_it_runs_on() {
    use crate::output_scale::{Display, Displays, Layout};

    // "The login screen must appear with a 2944x1840 built-in panel plus a
    // 1920x1080 monitor attached."
    //
    // The kiosk stretches the one sign-in window over both displays. With
    // each display at its own scale that is 3392x1080; when the kiosk keeps
    // both at 1x it is 4864x1840, which at the panel's scale 2 was a
    // 9728-wide texture the device refused, and the greeter exited until
    // greetd gave up. Either way the card is on the panel, the monitor shows
    // the time, and the window's frame fits the device.
    let layouts = [
        (
            Rect::new(0.0, 0.0, 1472.0, 920.0),
            Rect::new(1472.0, 0.0, 3392.0, 1080.0),
        ),
        (
            Rect::new(0.0, 0.0, 2944.0, 1840.0),
            Rect::new(2944.0, 0.0, 4864.0, 1080.0),
        ),
    ];
    for (panel, monitor) in layouts {
        let displays = Displays::fixed(Layout {
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
            let mut screen = Screen::of(
                GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()))
                    .with_displays(displays.clone()),
            );
            #[allow(
                clippy::cast_possible_truncation,
                reason = "display sizes in logical px fit f32"
            )]
            let viewport = (window.width() as f32, window.height() as f32);
            screen.at(viewport).theme = theme;
            screen.layout();
            assert_eq!(
                screen.app.content, panel,
                "the card fills the built-in panel"
            );
            for id in ["greeter.password", "greeter.clock"] {
                let rect = rect_for(&screen.app, id).unwrap_or_else(|| panic!("{id} is laid out"));
                assert!(
                    rect.area() > 0.0 && inside(panel, rect),
                    "{id} at {rect:?} is not on the built-in panel {panel:?}"
                );
            }
            assert!(
                rect_for(&screen.app, "greeter.password")
                    .is_some_and(|field| !inside(monitor, field)),
                "the monitor carries no password field"
            );
            for id in ["greeter.display-clock", "greeter.display-date"] {
                let rect = rect_for(&screen.app, id).unwrap_or_else(|| panic!("{id} is laid out"));
                assert!(
                    rect.area() > 0.0 && inside(monitor, rect),
                    "{id} at {rect:?} is not on the monitor {monitor:?}"
                );
                assert_eq!(
                    hidden_from_access(&screen.app, id, false),
                    Some(true),
                    "the monitor's {id} is read once, on the panel"
                );
            }
            assert_eq!(
                hidden_from_access(&screen.app, "greeter.password", false),
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
        let limit = tilo_ui_base::gpu::requested_texture_limit();
        let extent = tilo_ui_base::scale::fit_to_texture_limit(physical, 2.0, limit);
        assert!(
            extent.width <= limit && extent.height <= limit,
            "{extent:?} exceeds the device limit {limit}"
        );
        let logical =
            tilo_ui_base::scale::logical_size((extent.width, extent.height), extent.scale);
        assert!(
            (logical.0 - window.width()).abs() <= 1.0 && (logical.1 - window.height()).abs() <= 1.0,
            "the frame keeps the window's layout: {logical:?} vs {window:?}"
        );
    }

    // The reported laptop: `cage` stretches the one window over both
    // outputs, so a card centred in the window straddled the boundary.
    {
        let (displays, panel, monitor) = laptop();
        let mut screen =
            Screen::of(GreeterApp::with_sessions(test_sessions()).with_displays(displays));
        screen.at((3392.0, 1080.0)).layout();
        assert_eq!(screen.app.content, panel);
        assert_eq!(screen.app.layout_outputs, vec![panel, monitor]);
        for id in ["greeter.password", "greeter.clock"] {
            let rect = rect_for(&screen.app, id).unwrap_or_else(|| panic!("{id} is laid out"));
            assert!(
                rect.area() > 0.0 && inside(panel, rect),
                "{id} at {rect:?} is not on the built-in panel"
            );
        }

        // Before the kiosk has resized the window to the new layout, the
        // composition stays centred in the window it has.
        screen.at((1472.0, 920.0)).layout();
        assert_eq!(screen.app.content, panel);
        assert!(screen.app.layout_outputs.is_empty());
    }

    // The form stays on screen at every size and text scale, whatever it has
    // to say, and the sign-in action sits beside the field.
    for (viewport, scale) in [
        ((2944.0, 1840.0), 1.0),
        ((1472.0, 920.0), 1.0),
        ((1280.0, 800.0), 1.0),
        ((800.0, 600.0), 1.5),
        ((640.0, 480.0), 2.0),
        ((360.0, 640.0), 1.0),
    ] {
        for scenario in [AuthScenario::PersonalError, AuthScenario::Error] {
            let mut screen = Screen::shot(scenario);
            screen.app.sessions = test_sessions();
            screen.theme = Theme::dark().with_text_scale(scale);
            screen.at(viewport).layout();
            let screen_rect = Rect::new(0.0, 0.0, f64::from(viewport.0), f64::from(viewport.1));
            let card = screen.app.composed.element().expect("composed");
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
                    rect.width() > 0.0 && inside(screen_rect, rect),
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

    // Every state the headless snapshot path can capture composes.
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
        let mut screen = Screen::shot(scenario);
        screen.at((1280.0, 800.0)).layout();
        assert!(
            screen.app.composed.element().is_some(),
            "{scenario:?} composes"
        );
    }

    // The boot sign-in shows the passcode field where the greeter does.
    {
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
        let greeter = GreeterApp::for_shot(AuthScenario::PersonalNoFingerprint);
        boot_app.clock = greeter.clock.clone();
        boot_app.clock_pinned = true;
        boot_app.device = greeter.device;
        let mut boot_screen = Screen::of(boot_app);
        let mut greeter_screen = Screen::of(greeter);
        for viewport in [(1280.0, 800.0), (800.0, 1280.0), (1920.0, 1080.0)] {
            boot_screen.at(viewport).layout();
            greeter_screen.at(viewport).layout();
            let at_boot = boot_screen.app.composed.element().expect("composed");
            let after = greeter_screen.app.composed.element().expect("composed");
            assert!(
                rect_for(at_boot, "greeter.cant-sign-in").is_some(),
                "the boot offers the other ways in"
            );
            assert_eq!(
                passcode_field_rect(at_boot),
                passcode_field_rect(after),
                "at {viewport:?}"
            );
        }
    }

    // The reported laptop: the window starts at the panel's size and grows to
    // span both once the kiosk has applied the layout, re-laying the card out
    // on the panel after the passcode field already had focus.
    {
        let (displays, panel, _monitor) = laptop();
        let greeter = GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()))
            .with_displays(displays);
        let mut harness =
            ui::headless::Driver::for_root(ui::State::new(), greeter, (1472, 920), Theme::dark());
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
        let mut app = GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
        app.clock = ClockFace {
            time: "09:41".into(),
            date: "Saturday, October 4".into(),
        };
        app.clock_pinned = true;
        app.displays = harness.root::<GreeterApp>().displays.clone();
        app.rect = Rect::new(0.0, 0.0, 3392.0, 1080.0);
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root = app.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        assert_eq!(app.layout_area, Some(panel));
        assert_trees_mirror(&app, &taffy, root, "GreeterApp");
    }

    // Every greeter's widget tree mirrors its taffy tree at every depth.
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

    // On the form, the focusable order must NOT shift when the session
    // changes, a notice appears, or Caps Lock is detected — the
    // precondition for keeping focus and caret glued to the same field.
    {
        let mut screen = Screen::of(GreeterApp::with_sessions(test_sessions()));
        let base = focus_shape(&screen.app);
        let card = screen.card();
        let prompt = passcode_node(card.as_ref());
        let reveal = prompt
            .children
            .iter()
            .flat_map(|field| field.children.iter())
            .find(|node| node.role == tilo_ui_base::a11y::Role::ToggleButton)
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
        screen.app.selected_session = 1;
        assert_eq!(
            focus_shape(&screen.app),
            base,
            "changing session keeps order"
        );
        screen.app.notice = Notice::Error("greeter-error-credentials");
        assert_eq!(focus_shape(&screen.app), base, "an error keeps order");
        screen.app.notice = Notice::Hint("greeter-hint-passcode");
        assert_eq!(focus_shape(&screen.app), base, "a hint keeps order too");
        let line = |app: &GreeterApp| {
            passcode_node(card_of(app).as_ref())
                .children
                .iter()
                .find(|node| node.role == tilo_ui_base::a11y::Role::StaticText)
                .and_then(|node| node.name.clone())
        };
        let hint = line(&screen.app);
        screen.app.caps_lock.observe('A', false);
        assert_eq!(
            focus_shape(&screen.app),
            base,
            "the Caps-Lock note keeps order"
        );
        assert_ne!(
            line(&screen.app),
            hint,
            "the Caps-Lock note is the line under the field"
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
        let mut app = GreeterApp::with_sessions_and_login_user(one_session(), Some(quick_user()));
        app.clock = ClockFace {
            time: "09:41".into(),
            date: "Saturday, October 4".into(),
        };
        app.clock_pinned = true;
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

/// Between sign-ins the screen is still, asking the host for no frame; the
/// time is the largest thing on it, and the only thing that turns — once a
/// minute, on one wake the host can sleep until.
#[test]
fn the_screen_is_still_and_the_clock_turns_once_a_minute() {
    for scenario in [
        AuthScenario::Personal,
        AuthScenario::PersonalError,
        AuthScenario::Form,
        AuthScenario::PersonalHint,
    ] {
        let mut screen = Screen::shot(scenario);
        screen.at((1472.0, 920.0)).layout();
        assert_eq!(
            screen.app.next_wake_deadline(),
            None,
            "{scenario:?} must let the loop sleep"
        );
        assert!(
            !screen.app.needs_animation_frame(),
            "{scenario:?} must not animate"
        );
        // The same screen an hour on: nothing in it may follow the clock.
        let at = ui::time::Instant::now();
        let first = ui::time::testing::with_monotonic_time(at, || scene_bytes(&mut screen.app));
        let hour_on = at
            .checked_add(Duration::from_secs(3600))
            .expect("an hour on");
        let later =
            ui::time::testing::with_monotonic_time(hour_on, || scene_bytes(&mut screen.app));
        assert!(
            first == later,
            "{scenario:?} painted a different frame later"
        );
    }

    // The time is display type, above who is signing in, above the passcode.
    let mut screen = Screen::shot(AuthScenario::Personal);
    screen.at((1472.0, 920.0)).layout();
    let card = screen.app.composed.element().expect("composed");
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
    let started = tilo_ui_base::clock::instant_of(jiff::civil::date(2026, 6, 30).at(10, 41, 20, 0))
        .expect("a local time");
    let wall_ms = u64::try_from(started.as_millisecond()).expect("after the epoch");
    let zero = ui::time::now();
    let shown = |app: &mut ui::App, t: u64| {
        let time = in_time(zero, wall_ms, t, || {
            tilo_ui_base::clock::time_of_day(tilo_ui_base::clock::local_now())
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

/// The window reports whether it accepts input from the compositor alone, and
/// the on-screen keyboard a touchscreen needs suits the secret: a number pad
/// for a PIN, letters for a password, and its keys reach the passcode.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one screen with no keyboard attached, from its focus to its keys and its reader"
)]
fn the_on_screen_keyboard_suits_the_secret_and_types_into_the_passcode() {
    let accepts = |app: &GreeterApp| {
        app.console_state()["accepts_input"]
            .as_bool()
            .expect("sign-in state reports whether it accepts input")
    };
    let mut screen = Screen::of(GreeterApp::with_sessions(test_sessions()));
    assert!(
        !accepts(&screen.app),
        "a window nobody focused receives no keys"
    );
    screen.key("", &WindowEvent::Focused(true));
    assert!(accepts(&screen.app));
    screen.key("", &WindowEvent::Focused(false));
    assert!(!accepts(&screen.app));
    // A key can only arrive at the focused window.
    screen.key("", &named_key_event(NamedKey::Tab));
    assert!(accepts(&screen.app));

    // The keyboard is as tall as the rows the secret needs.
    let theme = Theme::dark();
    let (key, gap) = (
        f64::from(theme.tokens.metrics.panel_row_height),
        f64::from(theme.tokens.spacing.s2),
    );
    let rows_of = |screen: &mut Screen| {
        let keyboard = shown_keyboard(screen);
        let rect = field_and_keys(screen).1.expect("the keyboard is laid out");
        assert!(
            (rect.height() - keyboard.height(key, gap)).abs() < 0.5,
            "{rect:?}"
        );
        ((rect.height() + gap) / (key + gap)).round()
    };
    let rows = rows_of(&mut Screen::shot(AuthScenario::PersonalPin));
    assert!((rows - 4.0).abs() < 0.01, "{rows}");
    let rows = rows_of(&mut Screen::shot(AuthScenario::Personal));
    assert!((rows - 5.0).abs() < 0.01, "{rows}");

    // A PIN is typed on a number pad, a password on letters.
    let mut pin = Screen::shot(AuthScenario::PersonalPin);
    assert!(shown_keyboard(&mut pin).is_for_pin());
    let field = passcode_node(pin.app.composed.element().expect("composed"));
    assert!(
        field
            .children
            .iter()
            .any(|child| child.name.as_deref() == Some("PIN")),
        "the field asks for the PIN by name: {field:?}"
    );
    let mut password = Screen::shot(AuthScenario::Personal);
    assert!(!shown_keyboard(&mut password).is_for_pin());

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
    let mut boot_screen = Screen::of(app);
    assert!(shown_keyboard(&mut boot_screen).is_for_pin());
    boot_screen.app.boot_message(ToGreeter::Ask {
        person: 1,
        note: Note::None,
        several: true,
    });
    assert!(!shown_keyboard(&mut boot_screen).is_for_pin());

    // What the on-screen keyboard types reaches the passcode, a delete too.
    {
        use ui::passcode_keyboard::PasscodeInput;
        let mut screen = Screen::shot(AuthScenario::PersonalPin);
        shown_keyboard(&mut screen);
        screen.app.touch_keyboard.typed.borrow_mut().extend([
            PasscodeInput::Text("1".into()),
            PasscodeInput::Text("2".into()),
            PasscodeInput::Backspace,
            PasscodeInput::Text("3".into()),
        ]);
        screen.app.take_touch_keys();
        assert_eq!(*screen.app.password_buffer.lock_recover(), "13");
    }

    // With the screen reader on, a PIN is entered by exploring the pad by
    // touch and double-tapping, and no digit is ever spoken back.
    {
        use winit::event::TouchPhase::{Ended, Started};
        let mut greeter = GreeterApp::for_shot(AuthScenario::PersonalPin);
        greeter.keyboards = Some(false);
        let typed = Arc::clone(&greeter.password_buffer);
        let mut app = ui::App::new(ui::State::new(), Box::new(greeter));
        app.autofocus_first = true;
        app.autofocus_on_structure_change = true;
        let heard = Heard::default();
        app.set_screen_reader(Some(
            tilo_ui_base::screen_reader::ScreenReader::with_speaker(Box::new(heard.clone())),
        ));
        let touchscreen = Touchscreen::default();
        touchscreen.frame(&mut app);
        let zero = ui::time::now();
        let anywhere = vello::kurbo::Point::new(640.0, 120.0);

        for (index, digit) in ["2", "5", "8", "0"].into_iter().enumerate() {
            let key = semantic_centre(&mut app, tilo_ui_base::a11y::Role::Button, digit);
            let base = u64::try_from(index).unwrap().saturating_mul(2_000);
            let before = heard.spoken().len();
            // Rest a finger on the key: it is read, and nothing is typed.
            touchscreen.finger(&mut app, zero, (1, Started), key, base);
            assert!(app.tick_screen_reader(after(zero, base.saturating_add(200))));
            touchscreen.finger(&mut app, zero, (1, Ended), key, base.saturating_add(250));
            touchscreen.frame(&mut app);
            assert_eq!(
                heard.spoken().get(before..),
                Some(&[format!("{digit}, button")][..]),
                "the key under the finger is read"
            );
            assert_eq!(typed.lock_recover().len(), index, "exploring types nothing");
            // A double tap away from the keyboard presses the key read.
            touchscreen.finger(
                &mut app,
                zero,
                (2, Started),
                anywhere,
                base.saturating_add(600),
            );
            touchscreen.finger(
                &mut app,
                zero,
                (2, Ended),
                anywhere,
                base.saturating_add(650),
            );
            touchscreen.finger(
                &mut app,
                zero,
                (3, Started),
                anywhere,
                base.saturating_add(750),
            );
            touchscreen.finger(
                &mut app,
                zero,
                (3, Ended),
                anywhere,
                base.saturating_add(800),
            );
            touchscreen.frame(&mut app);
        }
        assert_eq!(*typed.lock_recover(), "2580");

        // The field is read as protected, with nothing of what it holds.
        let field = semantic_centre(&mut app, tilo_ui_base::a11y::Role::TextInput, "PIN");
        touchscreen.finger(&mut app, zero, (4, Started), field, 10_000);
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
}

/// The on-screen keyboard as the laid-out screen shows it, no keyboard
/// being attached.
fn shown_keyboard(screen: &mut Screen) -> ui::passcode_keyboard::PasscodeKeyboard {
    screen.app.keyboards = Some(false);
    screen.at((1280.0, 800.0));
    screen.theme = Theme::dark();
    screen.layout();
    screen.app.touch_keyboard.keyboard.get()
}

/// Where the passcode field and the on-screen keyboard are on the
/// laid-out screen.
fn field_and_keys(screen: &Screen) -> (Option<Rect>, Option<Rect>) {
    let card = screen.app.composed.element().expect("composed");
    (
        passcode_field_rect(card),
        rect_for(card, "greeter.touch-keyboard"),
    )
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
    emulator: std::cell::RefCell<tilo_ui_base::TouchPointerEmulator>,
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
fn semantic_centre(
    app: &mut ui::App,
    role: tilo_ui_base::a11y::Role,
    name: &str,
) -> vello::kurbo::Point {
    app.refresh_semantic_tree();
    app.semantic_tree()
        .iter()
        .find(|node| node.role == role && node.name.as_deref() == Some(name))
        .and_then(|node| node.bounds)
        .unwrap_or_else(|| panic!("{role:?} {name} is on the screen"))
        .center()
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

    let mut screen = Screen::personal();
    let root = automerge::ROOT;
    screen.app.begin_authenticating();
    screen.app.auth_session = Some(session.clone());
    screen.sync();

    // What the PAM conversation asks reaches the form as the greeter's own
    // update, carrying the identity of the challenge it waits on.
    let challenge = loop {
        let update = reported
            .recv_timeout(Duration::from_secs(10))
            .expect("the conversation asks for a second factor");
        screen.app.on_external_event(
            &update.encode(),
            &mut tilo_ui_base::widget::Edits::lend(&mut screen.doc),
            &root,
        );
        if let AuthUpdate::Challenge(challenge) = update {
            break challenge;
        }
    };
    assert!(challenge.secret, "a second factor is not echoed");

    // The form asks for it: the prompt, the field and the action say so.
    assert_eq!(screen.app.phase, AuthPhase::Authenticating);
    assert_eq!(screen.app.status_text(), "One-time code");
    let card = screen.card();
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
        screen.focusables().first().map(String::as_str),
        Some("greeter.password"),
        "the field is ready for the answer"
    );
    assert!(Widget::take_focus_reset(&mut screen.app));

    // Type the code and send it as this challenge's answer.
    screen.type_text("greeter.password", "765432");
    assert_eq!(&*screen.app.password_buffer.lock_recover(), "765432");
    assert!(screen.enter("greeter.password"));
    screen.sync();
    assert!(
        screen.app.password_buffer.lock_recover().is_empty(),
        "the answer is handed over, not left in the form"
    );

    // The challenge is answered and gone: its identity answers nothing more,
    // and the form asks for no factor.
    assert_eq!(
        session.respond(challenge.id, Zeroizing::new("765432".to_owned())),
        Err("This authentication challenge is no longer waiting for a response")
    );
    assert!(
        screen
            .app
            .auth_session
            .as_ref()
            .and_then(AuthSession::challenge)
            .is_none()
    );
    let card = screen.card();
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
    screen.type_text("greeter.password", "111111");
    assert!(
        screen.app.password_buffer.lock_recover().is_empty(),
        "the closed field holds no second answer"
    );
    assert!(screen.enter("greeter.password"));
    screen.sync();
    assert_eq!(
        accessible_name_for(screen.card().as_ref(), "greeter.login").as_deref(),
        Some("Sign in"),
        "the form asks for no factor"
    );
    stale_submitted.send(()).unwrap();

    // greetd's refusal returns the person to the form, signed in nowhere.
    let update = reported
        .recv_timeout(Duration::from_secs(10))
        .expect("the refused attempt is reported");
    assert_eq!(update, AuthUpdate::Failed(Failure::Credentials));
    screen.app.on_external_event(
        &update.encode(),
        &mut tilo_ui_base::widget::Edits::lend(&mut screen.doc),
        &root,
    );
    assert_eq!(screen.app.phase, AuthPhase::Idle);
    assert!(screen.app.auth_session.is_none());
    assert_eq!(screen.app.failures, 1, "the attempt was refused once");
    assert_eq!(screen.app.notice, Notice::Error("greeter-error-passcode"));
    assert!(screen.app.password_buffer.lock_recover().is_empty());
    greetd.join().unwrap();
}

/// A person who cannot sign in opens the recovery form, which refuses
/// mismatched or wrong entries without asking tilo-authd, and sets a new
/// secret through it when the key is right.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one recovery, from the way in through the new secret"
)]
fn cant_sign_in_opens_recovery_and_sets_a_new_secret() {
    {
        let mut screen = Screen::shot(AuthScenario::PersonalNoFingerprint);
        screen.at((1280.0, 800.0)).layout();
        let card = screen.app.composed.element().expect("composed");
        let field = passcode_field_rect(card).expect("the passcode field");
        let button = rect_for(card, "greeter.cant-sign-in").expect("offered after boot");
        assert!(button.y0 >= field.y1, "{button:?} is under {field:?}");
        assert_eq!(
            screen.focusables()[..3],
            ["greeter.password", "greeter.login", "greeter.cant-sign-in"],
            "the next stop after signing in"
        );
        // Without a known account there is nobody to recover.
        let no_account = Screen::of(GreeterApp::with_sessions(one_session()));
        assert!(
            !no_account
                .focusables()
                .contains(&"greeter.cant-sign-in".to_owned())
        );
    }

    {
        let mut screen = Screen::personal();
        seed_password(&screen.app, "half typed");
        screen.press("greeter.cant-sign-in");
        assert!(screen.app.recovery.is_some());
        assert!(screen.app.password_buffer.lock().unwrap().is_empty());
        assert!(screen.app.take_focus_reset(), "focus moves into the form");
        assert_eq!(
            screen.focusables()[..5],
            [
                "greeter.recovery-key",
                "greeter.new-secret",
                "greeter.confirm-secret",
                "greeter.recovery-cancel",
                "greeter.recovery-save",
            ],
            "the recovery key takes focus"
        );
        let card = screen.card();
        assert!(find_test_id(card.as_ref(), "greeter.password").is_none());
        assert!(find_test_id(card.as_ref(), "greeter.cant-sign-in").is_none());
        assert_eq!(
            accessible_name_for(card.as_ref(), "greeter.recovery-key").as_deref(),
            Some("Recovery key")
        );
        assert!(
            !shown_keyboard(&mut screen).is_for_pin(),
            "a recovery key is typed on the letters"
        );

        screen.press("greeter.recovery-cancel");
        assert!(screen.app.recovery.is_none());
        assert_eq!(
            screen.focusables().first().map(String::as_str),
            Some("greeter.password")
        );

        screen.press("greeter.cant-sign-in");
        assert!(screen.escape("greeter.recovery-key"));
        screen.sync();
        assert!(screen.app.recovery.is_none(), "Esc leaves the form too");
        assert!(untouched(&screen.doc));
    }

    // Both entries are checked here, so tilo-authd is never asked.
    {
        let dir = tempfile::tempdir().unwrap();
        let listener = std::os::unix::net::UnixListener::bind(dir.path().join("socket")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut screen = recovering();
        screen.app.auth_socket = dir.path().join("socket");

        screen.press("greeter.recovery-save");
        assert_eq!(
            screen.app.recovery.as_ref().unwrap().notice,
            Notice::Hint("greeter-recovery-hint-key")
        );
        fill_recovery(&screen.app, "cccc-dddd", "", "");
        screen.press("greeter.recovery-save");
        assert_eq!(
            screen.app.recovery.as_ref().unwrap().notice,
            Notice::Hint("greeter-recovery-hint-new")
        );
        assert_eq!(recovery_buffers(&screen.app), ["cccc-dddd", "", ""]);

        fill_recovery(&screen.app, "cccc-dddd", "2468", "1357");
        screen.press("greeter.recovery-save");
        let recovery = screen.app.recovery.as_ref().unwrap();
        assert_eq!(recovery.notice, Notice::Error("greeter-recovery-mismatch"));
        assert!(!recovery.is_pending());
        assert_eq!(
            recovery_buffers(&screen.app),
            ["cccc-dddd", "", ""],
            "the two entries are typed again"
        );
        assert!(
            spoken(screen.card().as_ref())
                .contains(&"The two entries don't match. Type them again.".to_owned())
        );
        assert!(
            listener
                .accept()
                .is_err_and(|e| e.kind() == std::io::ErrorKind::WouldBlock),
            "tilo-authd was not asked"
        );
        assert!(untouched(&screen.doc));
    }

    // A wrong key and a secret too weak to store are said plainly; the right
    // key sets the new secret.
    {
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
        let mut screen = recovering();
        screen.app.auth_socket = dir.path().join("socket");

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
            fill_recovery(&screen.app, "cccc-dddd", "2468", "2468");
            screen.press("greeter.recovery-save");
            assert!(screen.app.recovery.as_ref().unwrap().is_pending());
            assert_eq!(recovery_buffers(&screen.app), ["", "", ""]);
            answered(&mut screen.app, &screen.doc);
            assert_eq!(
                screen.app.recovery.as_ref().unwrap().notice,
                Notice::Error(refusal)
            );
            assert_eq!(recovery_buffers(&screen.app), ["", "", ""]);
            assert!(spoken(screen.card().as_ref()).contains(&said.to_owned()));
        }

        fill_recovery(&screen.app, "cccc-dddd", "2468", "2468");
        screen.press("greeter.recovery-save");
        answered(&mut screen.app, &screen.doc);
        assert!(screen.app.recovery.is_none(), "back to the sign-in form");
        assert_eq!(screen.app.notice, Notice::Hint("greeter-recovery-done-pin"));
        assert_eq!(screen.app.status_text(), "Sign in with your new PIN.");
        assert_eq!(
            accessible_name_for(screen.card().as_ref(), "greeter.password").as_deref(),
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
        assert!(untouched(&screen.doc), "no secret reaches the document");
    }
}

fn recovering() -> Screen {
    let mut screen = Screen::personal();
    screen.press("greeter.cant-sign-in");
    screen
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

/// The footer's power actions and accessibility toggles stay usable while an
/// attempt runs and each takes effect exactly once, and the keyboard focus
/// follows the form rather than the footer.
#[test]
fn power_and_accessibility_work_before_signing_in() {
    {
        let mut screen = Screen::of(GreeterApp::with_sessions(test_sessions()));
        screen.app.begin_authenticating();
        assert_eq!(
            screen.focusables(),
            vec![
                "greeter.login",
                "greeter.larger-text",
                "greeter.high-contrast",
                "greeter.screen-reader",
                "greeter.power",
            ],
            "the sign-in action stays the first focusable during auth; power stays reachable"
        );
        assert!(Widget::take_focus_reset(&mut screen.app));
        assert!(
            !Widget::take_focus_reset(&mut screen.app),
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
        let mut screen = Screen::of(GreeterApp::with_sessions(one_session()));
        screen.sync();
        screen.app.requests.power.set(Some(action));
        screen.sync();
        assert_eq!(screen.app.last_power_action, Some(action));
        screen.app.last_power_action = None;
        screen.sync();
        assert_eq!(screen.app.last_power_action, None, "{action:?} ran once");
    }

    // Each accessibility toggle asks the host once and reaches the theme.
    {
        let mut screen = Screen::of(GreeterApp::with_sessions(one_session()));
        assert_eq!(screen.app.take_accessibility_request(), None);
        screen
            .app
            .requests
            .accessibility
            .set(Some(AccessibilityRequest::HighContrast(true)));
        assert_eq!(
            screen.app.take_accessibility_request(),
            Some(AccessibilityRequest::HighContrast(true))
        );
        assert_eq!(screen.app.take_accessibility_request(), None, "one-shot");
        screen
            .app
            .requests
            .accessibility
            .set(Some(AccessibilityRequest::ScreenReader(true)));
        let _ = screen.app.take_accessibility_request();
        assert!(
            screen.app.screen_reader,
            "the toggle shows the reader as on"
        );
    }
    let greeter = GreeterApp::with_sessions(one_session());
    greeter
        .requests
        .accessibility
        .set(Some(AccessibilityRequest::HighContrast(true)));
    let mut host = ui::App::new(ui::State::new(), Box::new(greeter));
    assert!(host.apply_accessibility_request());
    assert!(host.high_contrast());
}

/// A boot sign-in shows the session starting and, if it does not start, falls
/// back to the ordinary form without an error, with that person chosen.
#[test]
fn a_boot_sign_in_shows_the_transition_and_falls_back_to_the_form() {
    let root = automerge::ROOT;
    {
        let mut screen = Screen::shot(AuthScenario::Accounts);
        screen.app.offer_boot_sign_in("grace");
        screen.sync();
        assert_eq!(screen.app.phase, AuthPhase::Starting);
        assert_eq!(
            screen.app.login_user.as_ref().map(|u| u.username.as_str()),
            Some("grace"),
            "the person who signed in at boot is the one starting"
        );
        assert_eq!(screen.app.status_text(), "Starting your session…");
        assert!(
            !screen
                .focusables()
                .contains(&"greeter.password".to_string()),
            "nothing to type while the session starts"
        );

        screen.app.on_external_event(
            &AuthUpdate::Accepted.encode(),
            &mut tilo_ui_base::widget::Edits::lend(&mut screen.doc),
            &root,
        );
        assert_eq!(screen.app.phase, AuthPhase::Starting);

        // greetd refused, or PAM asked a question: the ordinary screen, with
        // that person chosen, and no error.
        let _ = Widget::take_focus_reset(&mut screen.app);
        screen.app.on_external_event(
            &AuthUpdate::Failed(Failure::Cancelled).encode(),
            &mut tilo_ui_base::widget::Edits::lend(&mut screen.doc),
            &root,
        );
        assert_eq!(screen.app.phase, AuthPhase::Idle);
        assert_eq!(screen.app.notice, Notice::None);
        assert_eq!(screen.app.failures, 0);
        assert!(Widget::take_focus_reset(&mut screen.app));
        screen.sync();
        assert_eq!(
            screen.focusables().first().map(String::as_str),
            Some("greeter.password")
        );
        assert_eq!(
            screen.app.login_user.as_ref().map(|u| u.username.as_str()),
            Some("grace")
        );

        // The handoff is over: a later refusal reads as usual.
        seed_password(&screen.app, "wrongpw");
        screen.app.begin_authenticating();
        screen.app.on_external_event(
            &AuthUpdate::Failed(Failure::Credentials).encode(),
            &mut tilo_ui_base::widget::Edits::lend(&mut screen.doc),
            &root,
        );
        assert_eq!(screen.app.notice, Notice::Error("greeter-error-passcode"));
    }

    // An account the screen does not list falls back to the named form.
    {
        let mut screen = Screen::shot(AuthScenario::Accounts);
        screen.app.offer_boot_sign_in("svc");
        screen.sync();
        assert_eq!(screen.app.phase, AuthPhase::Starting);
        assert_eq!(screen.app.attempt_username(), "svc");
        screen.app.on_external_event(
            &AuthUpdate::Failed(Failure::NotPermitted).encode(),
            &mut tilo_ui_base::widget::Edits::lend(&mut screen.doc),
            &root,
        );
        assert_eq!(screen.app.phase, AuthPhase::Idle);
        assert_eq!(screen.app.notice, Notice::None);
        assert_eq!(typed_username(&screen.app), "svc");
    }
}

/// The screen says things in words and draws icons with the toolkit's icon
/// set; no emoji or pictographic characters stand in for either, and every
/// catalogue key resolves.
#[test]
fn copy_uses_words_not_symbols() {
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
}
