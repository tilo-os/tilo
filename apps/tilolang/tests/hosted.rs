//! A hosted tilolang app as a person uses it: the window process's own window (the UI host's
//! half, `HostedWindow`) driven by pointer and key events, over a real patch connection to a
//! tilolang host running the app's instance on a broker the test plays: in the headless frame
//! harness, and in the live `App` a window process runs, fed input as its surface feeds it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a test: a failure is a panic"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::arithmetic_side_effects,
    reason = "pixels of a small frame: coordinates and counts are tiny, and the arithmetic is the \
              comparison itself"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use collaboration::{
    CollabEvent, CollabRequest, CollabTransport, CollaborationManager, DocumentMutation,
    PeerTransport, RouteSpec, TransportCommand, TransportError, TransportStatus,
};
use tilo_automerge::ApplyIncoming as _;
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilolang_app::hosted::HostedWindow;
use tilolang_host::TilolangHost;
use tilolang_host::hosted::{Hosted, Spec};
use tilolang_host::phrases::Phrases;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::instance::record::InstanceRecord;
use ui::headless::FrameHarness;
use ui::widget::Widget;

const NOTES: &str = "so.tilo.lang.notes";
/// The test's own notebook, whose notes' bodies are rich text.
const RICHBODY: &str = "so.tilo.lang.richbody";
/// The task list the person keeps.
const TASKS: &str = "so.tilo.lang.tasks";
const VIEWPORT: (u32, u32) = (1280, 800);

type Sent = Rc<RefCell<Vec<CollabRequest>>>;
type Inbox = Rc<RefCell<VecDeque<CollabEvent>>>;

/// The broker's end of the instance's connection.
struct Broker {
    route: RouteSpec,
    sent: Sent,
    inbox: Inbox,
    joined: Option<RouteSpec>,
}

impl PeerTransport for Broker {
    fn is_connected(&self) -> bool {
        true
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        if matches!(request, CollabRequest::Hello { .. }) {
            self.joined = Some(self.route.clone());
        }
        self.sent.borrow_mut().push(request);
        Ok(())
    }

    fn poll(&mut self) -> Vec<CollabEvent> {
        self.inbox.borrow_mut().drain(..).collect()
    }

    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        self.joined.take().map(Ok)
    }
}

/// The broker's end of a connection whose queue fills up: while `busy`, it takes no change of
/// the record (`TransportError::Busy`, as the broker transport's queue says under fast typing);
/// the changes it takes are kept in `changes`.
struct Crowded {
    route: RouteSpec,
    busy: Arc<AtomicBool>,
    changes: Arc<Mutex<Vec<Vec<u8>>>>,
    joined: Option<RouteSpec>,
}

impl CollabTransport for Crowded {
    fn status(&self) -> TransportStatus {
        TransportStatus::Ready
    }

    fn submit(&mut self, command: TransportCommand) -> Result<(), TransportError> {
        match command {
            TransportCommand::Primary(CollabRequest::Hello { .. }) => {
                self.joined = Some(self.route.clone());
            }
            TransportCommand::Primary(CollabRequest::Document { mutation, .. }) => {
                if self.busy.load(Ordering::SeqCst) {
                    return Err(TransportError::Busy);
                }
                match mutation {
                    DocumentMutation::Incremental(bytes) | DocumentMutation::Snapshot(bytes) => {
                        self.changes.lock().unwrap().push(bytes);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn drain(&mut self, _: &mut Vec<CollabEvent>) {}

    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        self.joined.take().map(Ok)
    }

    fn take_action_directory_requests(
        &mut self,
        _: &mut Vec<collaboration::ActionDirectoryRequest>,
    ) {
    }
}

/// The package in `dir` (from this crate's), built with the packages it uses.
fn package(dir: &str) -> Bundle {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    let dirs = tilolang::source::dependency_order(&dir, &[]).expect("the packages resolve");
    let built = tilolang::source::compile_dirs(&dirs, &[], &mut tilolang::OsEntropy::default())
        .expect("the packages compile");
    let (package, used) = built.split_last().expect("the package is built last");
    Bundle::new(&package.compiled, used.iter().map(|used| &used.compiled))
}

/// A window-sized region of a host's scene, as a window process's surface is.
struct Region;

impl ui::app::AppSurface for Region {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> ui::winit::dpi::PhysicalSize<u32> {
        ui::winit::dpi::PhysicalSize::new(VIEWPORT.0, VIEWPORT.1)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: ui::winit::window::CursorIcon) {}
}

thread_local! {
    /// The serial of the last input event the live window was handed.
    static INPUT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The window the person uses: the headless frame harness, or the live `App` a window process
/// runs, fed input as its surface feeds it.
enum Window {
    Harness(Box<FrameHarness>),
    Live(Box<ui::App>),
}

impl Window {
    /// The live window's frames, as its surface runs them, until it is at rest.
    fn live_frame(app: &mut ui::App) {
        // The wake's input is all handled, as the window's event loop says once it is.
        app.input_handled(INPUT.with(std::cell::Cell::get));
        for _ in 0..3 {
            let mut scene = ui::advanced::Scene::new();
            if !app.surface_frame(&Region, &mut scene) || !app.unsettled() {
                break;
            }
        }
    }

    /// `event` as the live window's surface delivers it. A quick hand's events (and
    /// tilo-control's) come faster than the window's frames: none runs between them, only once
    /// the session settles.
    /// Each carries the next input serial, as the compositor's do: the window has handled the
    /// input before it when it gets it.
    fn live_input(app: &mut ui::App, event: ui::winit::event::WindowEvent) {
        let before = INPUT.with(|input| input.replace(input.get().wrapping_add(1)));
        app.input_handled(before);
        app.surface_input(&Region, event, false);
    }

    fn frame(&mut self) {
        match self {
            Self::Harness(window) => window.relayout(),
            Self::Live(app) => Self::live_frame(app),
        }
    }

    /// A click at `(x, y)`: the move, the press and the release.
    fn click_at(&mut self, x: f64, y: f64) {
        match self {
            Self::Harness(window) => window.click_at(x, y),
            Self::Live(app) => {
                Self::live_input(app, ui::synthetic_input::cursor_moved(x, y));
                Self::live_input(app, ui::synthetic_input::mouse_press());
                Self::live_input(app, ui::synthetic_input::mouse_release());
            }
        }
    }

    /// `text` typed key by key, each key down and up.
    fn type_text(&mut self, text: &str) {
        use ui::winit::event::ElementState;
        use ui::winit::keyboard::{Key, NamedKey};
        match self {
            Self::Harness(window) => window.type_text(text),
            Self::Live(app) => {
                for ch in text.chars() {
                    let typed = ch.to_string();
                    let key = match ch {
                        ' ' => Key::Named(NamedKey::Space),
                        _ => Key::Character(typed.as_str().into()),
                    };
                    Self::live_input(
                        app,
                        ui::synthetic_input::key_press(key.clone(), Some(&typed)),
                    );
                    Self::live_input(
                        app,
                        ui::synthetic_input::key_input(&key, ElementState::Released),
                    );
                }
            }
        }
    }

    /// Opens the window's own command palette on `request`, as its search key does.
    fn open_search(&mut self, request: ui::search::SearchRequest) {
        match self {
            Self::Harness(window) => window.open_search(&request).expect("the palette opens"),
            Self::Live(app) => app.open_search(request),
        }
    }

    /// The named key `key` pressed and released.
    fn press_key(&mut self, key: ui::winit::keyboard::NamedKey) {
        use ui::winit::event::ElementState;
        use ui::winit::keyboard::Key;
        match self {
            Self::Harness(window) => window.key(&format!("{key:?}")),
            Self::Live(app) => {
                Self::live_input(app, ui::synthetic_input::named_key(key));
                Self::live_input(
                    app,
                    ui::synthetic_input::key_input(&Key::Named(key), ElementState::Released),
                );
            }
        }
    }

    /// The character key `key` pressed and released with `modifiers` held (`ctrl`, `shift`),
    /// as a keyboard sends it: the modifiers first, then the key, then the modifiers let go.
    fn chord(&mut self, modifiers: &[&str], key: &str) {
        use ui::winit::event::ElementState;
        use ui::winit::keyboard::{Key, ModifiersState};
        match self {
            Self::Harness(window) => {
                let names: Vec<String> = modifiers.iter().map(|name| (*name).to_owned()).collect();
                window.set_modifiers(&names);
                window.key(key);
                window.set_modifiers(&[]);
            }
            Self::Live(app) => {
                let mut held = ModifiersState::empty();
                for name in modifiers {
                    held |= match *name {
                        "ctrl" => ModifiersState::CONTROL,
                        "shift" => ModifiersState::SHIFT,
                        other => panic!("no modifier {other}"),
                    };
                }
                // The surface reports the modifiers held beside the key events.
                app.set_surface_modifiers(held.into());
                let key = Key::Character(key.into());
                Self::live_input(app, ui::synthetic_input::key_press(key.clone(), None));
                Self::live_input(
                    app,
                    ui::synthetic_input::key_input(&key, ElementState::Released),
                );
                app.set_surface_modifiers(ModifiersState::empty().into());
            }
        }
    }

    fn root(&mut self) -> &mut dyn Widget {
        match self {
            Self::Harness(window) => window.root_widget_mut(),
            Self::Live(app) => app.root_widget_mut(),
        }
    }

    fn harness(&mut self) -> &mut FrameHarness {
        match self {
            Self::Harness(window) => window,
            Self::Live(_) => panic!("the window is the live one"),
        }
    }
}

/// A person at a window whose instance a tilolang host runs.
struct Session {
    host: TilolangHost,
    window: Window,
    /// The broker's end of the instance's connection.
    _broker: Box<dyn std::any::Any>,
}

impl Session {
    /// A person at a Tasks window in the frame harness.
    fn tasks() -> Self {
        Self::new(TASKS, "../tl/tasks", "ada", |_| {})
    }

    /// A person at a Notes window in the frame harness.
    fn notes() -> Self {
        Self::new(NOTES, "../tl/notes", "ada", |_| {})
    }

    /// A person at a Tasks window the live `App` runs.
    fn live_tasks() -> Self {
        Self::open(TASKS, "../tl/tasks", "ada", |hosted| {
            let mut app = ui::App::new(ui::State::new(), Box::new(hosted));
            app.init_layout();
            Window::Live(Box::new(app))
        })
    }

    /// A person at a Notes window the live `App` runs.
    fn live_notes() -> Self {
        Self::open(NOTES, "../tl/notes", "ada", |hosted| {
            let mut app = ui::App::new(ui::State::new(), Box::new(hosted));
            app.init_layout();
            Window::Live(Box::new(app))
        })
    }

    /// A person at a Notebook window whose notes' bodies are rich text (`tests/richbody`), the
    /// frame harness making it.
    fn rich() -> Self {
        Self::new(RICHBODY, "tests/richbody", "ada", |_| {})
    }

    /// A person at a Notebook window whose notes' bodies are rich text (`tests/richbody`), the
    /// live `App` running it: the same bar, fields and list as Notes, with a rich body.
    fn live_rich() -> Self {
        Self::open(RICHBODY, "tests/richbody", "ada", |hosted| {
            let mut app = ui::App::new(ui::State::new(), Box::new(hosted));
            app.init_layout();
            Window::Live(Box::new(app))
        })
    }

    /// A person at a Notes window the live `App` runs, whose connection's queue is full while
    /// `busy`; the broker's end keeps the record's changes it took in `changes`.
    fn crowded_notes(busy: &Arc<AtomicBool>, changes: &Arc<Mutex<Vec<Vec<u8>>>>) -> Self {
        let route = RouteSpec::app(NOTES, uuid::Uuid::new_v4());
        let manager = CollaborationManager::with_transport(Box::new(Crowded {
            route: route.clone(),
            busy: Arc::clone(busy),
            changes: Arc::clone(changes),
            joined: None,
        }));
        Self::over(
            manager,
            &route,
            NOTES,
            "../tl/notes",
            "ada",
            Box::new(()),
            |hosted| {
                let mut app = ui::App::new(ui::State::new(), Box::new(hosted));
                app.init_layout();
                Window::Live(Box::new(app))
            },
        )
    }

    /// `person` at a harness window of the package `app` in `dir`, whose presence `present`
    /// sets up.
    fn new(
        app: &str,
        dir: &str,
        person: &str,
        present: impl FnOnce(&mut ui::presence::PresenceCtx),
    ) -> Self {
        Self::open(app, dir, person, |hosted| {
            let mut window = FrameHarness::new(ui::State::new(), Box::new(hosted), VIEWPORT);
            window.update_presence(|presence| {
                present(presence);
                true
            });
            Window::Harness(Box::new(window))
        })
    }

    /// `person` at the window `window` makes of the hosted window of the package `app` in
    /// `dir`.
    fn open(
        app: &str,
        dir: &str,
        person: &str,
        window: impl FnOnce(HostedWindow) -> Window,
    ) -> Self {
        let route = RouteSpec::app(app, uuid::Uuid::new_v4());
        let (sent, inbox): (Sent, Inbox) = (Rc::default(), Rc::default());
        let manager = CollaborationManager::with_peer_transport(Broker {
            route: route.clone(),
            sent,
            inbox: Rc::clone(&inbox),
            joined: None,
        });
        Self::over(manager, &route, app, dir, person, Box::new(inbox), window)
    }

    /// `person` at the window `window` makes of the hosted window of the package `app` in
    /// `dir`, whose instance's connection is `manager`, on `route`; `broker` is what the broker's
    /// end holds.
    fn over(
        mut manager: CollaborationManager,
        route: &RouteSpec,
        app: &str,
        dir: &str,
        person: &str,
        broker: Box<dyn std::any::Any>,
        window: impl FnOnce(HostedWindow) -> Window,
    ) -> Self {
        assert!(manager.send(CollabRequest::Hello {
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id,
            record: None,
            record_kind: None,
            handle: Some(person.to_owned()),
        }));
        let _ = manager.poll();
        let record =
            InstanceRecord::new(automerge::AutoCommit::new(), manager, route.clone(), person);
        let wake: tilolang_tilo::background::Waker = Arc::new(|| {});
        let mut host = TilolangHost::new(None, Arc::clone(&wake));
        let kept = host.kept(app).unwrap();
        let (ours, theirs) = Channel::pair().unwrap();
        let watched = ours.as_fd().try_clone_to_owned().unwrap();
        let hosted = Hosted::with_record(
            Spec {
                app: app.to_owned(),
                workspace: route.cluster_id.to_string(),
                instance: 1,
                person: person.to_owned(),
            },
            record,
            Arc::new(package(dir)),
            &kept,
            Box::new(ChannelTransport::new(ours).unwrap()),
            &Phrases::new("en-US"),
            wake,
        );
        host.add(hosted, Some(watched));
        let mut session = Self {
            host,
            window: window(HostedWindow::over(theirs)),
            _broker: broker,
        };
        session.settle();
        session
    }

    /// The host's frames and the window's, until neither has anything left to do.
    fn settle(&mut self) {
        let mut quiet = 0_u32;
        while quiet < 20 {
            let changed = self.host.turn();
            self.window.frame();
            if changed {
                quiet = 0;
            } else {
                quiet = quiet.saturating_add(1);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }

    fn hosted(&mut self) -> &HostedWindow {
        self.window
            .root()
            .as_any()
            .and_then(|any| any.downcast_ref::<HostedWindow>())
            .expect("the window is the hosted window")
    }

    /// The texts the window's views show.
    fn shown(&mut self) -> Vec<String> {
        fn texts(node: &tilo_ui_host::UiNode, out: &mut Vec<String>) {
            for value in node.props.values() {
                match value {
                    tilo_ui_host::Value::Text(text) => out.push(text.clone()),
                    tilo_ui_host::Value::Principal(who) => out.push(who.name.clone()),
                    _ => {}
                }
            }
            for child in &node.children {
                texts(child, out);
            }
        }
        let view = self.hosted().view().expect("the window is connected");
        let mut out = Vec::new();
        for tree in [view.root_tree(), view.detail_tree()].into_iter().flatten() {
            texts(&tree, &mut out);
        }
        out
    }

    /// The titles of the notebook's notes, as its list shows them.
    fn titles(&mut self) -> Vec<String> {
        let view = self.hosted().view().expect("the window is connected");
        let mut titles = Vec::new();
        for list in view.lists() {
            if list.starts_with(tilo_ui_host::shell::DETAIL) {
                continue;
            }
            let rows = view.rows_of(&list).expect("the list's rows");
            let rows = rows.borrow();
            for key in rows.rendered() {
                let row = rows.row(&key).expect("a rendered row");
                let title = find(row, &|node| node.kind == "ui.ListRow")
                    .and_then(|row| match row.props.get("title") {
                        Some(tilo_ui_host::Value::Text(title)) => Some(title.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("a row shows its note's title: {row:?}"));
                titles.push(title);
            }
        }
        titles
    }

    /// The centre of the open note's body.
    fn body(&mut self) -> (f64, f64) {
        let body = self.field_key("Write something");
        let rect = widget_by_id(self.window.root(), &body)
            .expect("the open note's body is laid out")
            .get_rect();
        (rect.center().x, rect.center().y)
    }

    /// The key in the composed tree of the open note's field whose placeholder is
    /// `placeholder`.
    fn field_key(&mut self, placeholder: &str) -> String {
        let detail = self
            .hosted()
            .view()
            .and_then(tilo_ui_host::AppWindow::detail_tree)
            .expect("a note is open");
        let key = find(&detail, &|node| {
            matches!(node.props.get("placeholder"), Some(tilo_ui_host::Value::Text(p)) if p == placeholder)
        })
        .unwrap_or_else(|| panic!("the open note has a field {placeholder:?}"))
        .key
        .clone();
        format!("{}{key}", tilo_ui_host::shell::DETAIL)
    }

    /// The centre of the open note's field whose placeholder is `placeholder`.
    fn field(&mut self, placeholder: &str) -> (f64, f64) {
        let id = self.field_key(placeholder);
        let rect = widget_by_id(self.window.root(), &id)
            .unwrap_or_else(|| panic!("the field {id} is laid out"))
            .get_rect();
        (rect.center().x, rect.center().y)
    }

    /// What the open note's field whose placeholder is `placeholder` shows.
    fn field_value(&mut self, placeholder: &str) -> Option<tilo_ui_host::Value> {
        let id = self.field_key(placeholder);
        self.hosted().view()?.bound_value(&id)
    }

    /// The centre of the header's New note button.
    fn new_note_button(&mut self) -> (f64, f64) {
        let mut found = Vec::new();
        named(self.window.root(), "New note", &mut found);
        let rect = *found.last().expect("the window offers New note");
        (rect.center().x, rect.center().y)
    }
}

fn find<'a>(
    node: &'a tilo_ui_host::UiNode,
    want: &dyn Fn(&tilo_ui_host::UiNode) -> bool,
) -> Option<&'a tilo_ui_host::UiNode> {
    if want(node) {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, want))
}

fn widget_by_id<'a>(widget: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
    if widget.test_id() == Some(id) {
        return Some(widget);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| widget_by_id(child, id))
}

/// The rects of the laid out widgets whose accessible name is `label`.
fn named(widget: &dyn Widget, label: &str, out: &mut Vec<ui::kurbo::Rect>) {
    if widget
        .accessibility()
        .is_some_and(|access| access.name.as_deref() == Some(label))
        && widget.get_rect().width() > 0.0
    {
        out.push(widget.get_rect());
    }
    for child in widget.children() {
        named(child, label, out);
    }
}

/// Every checkbox the window lays out on screen, with whether it is marked.
fn boxes(widget: &dyn Widget, out: &mut Vec<(ui::kurbo::Rect, bool)>) {
    if let Some(access) = widget.accessibility()
        && access.role == ui::a11y::Role::Checkbox
    {
        let rect = widget.get_rect();
        if rect.width() > 0.0 && rect.height() > 0.0 {
            out.push((rect, access.state.checked == Some(true)));
        }
    }
    for child in widget.children() {
        boxes(child, out);
    }
}

/// The pixels of `rect` in a frame the harness drew, at its scale of one.
fn pixels_of(pixels: &[u8], rect: ui::kurbo::Rect) -> Vec<[u8; 4]> {
    let width = VIEWPORT.0 as usize;
    let x0 = rect.x0.round().max(0.0) as usize;
    let y0 = rect.y0.round().max(0.0) as usize;
    let x1 = (rect.x1.round() as usize).min(width);
    let y1 = rect.y1.round().max(0.0) as usize;
    let rows = pixels.len() / 4 / width.max(1);
    let mut out = Vec::new();
    for row in y0..y1.min(rows) {
        for col in x0..x1 {
            let at = (row * width + col) * 4;
            out.push([pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]);
        }
    }
    out
}

/// How far apart two pictures of the same space are, from 0 to 1.
fn apart(one: &[[u8; 4]], other: &[[u8; 4]]) -> f64 {
    let common = one.len().min(other.len());
    if common == 0 {
        return 0.0;
    }
    let total: f64 = one
        .iter()
        .zip(other)
        .take(common)
        .map(|(a, b)| {
            (f64::from(a[0].abs_diff(b[0]))
                + f64::from(a[1].abs_diff(b[1]))
                + f64::from(a[2].abs_diff(b[2])))
                / 3.0
                / 255.0
        })
        .sum();
    total / common as f64
}

/// After a first note made and titled as a person does, "+" and at once the next title, every
/// key typed before the host answered, three times over: each new note takes the keyboard in
/// its title.
fn new_notes_take_the_keys_typed_right_after_new_note(mut session: Session) {
    let plus = session.new_note_button();
    session.window.click_at(plus.0, plus.1);
    session.settle();
    let title = session.field("Untitled");
    session.window.click_at(title.0, title.1);
    session.window.type_text("First ");
    session.settle();
    for _ in 0..3 {
        let plus = session.new_note_button();
        session.window.click_at(plus.0, plus.1);
        session.window.type_text("Note title");
        session.settle();
    }
    let mut titles = session.titles();
    titles.sort();
    assert_eq!(
        titles,
        [
            // Clicked past its end: typed after it.
            "First ",
            "Note title",
            "Note title",
            "Note title"
        ],
        "every key typed after New note lands in the new note's title, none lost"
    );
}

#[test]
fn keys_typed_right_after_new_note_land_in_the_new_notes_title_whenever_the_host_answers() {
    new_notes_take_the_keys_typed_right_after_new_note(Session::notes());
}

#[test]
fn keys_typed_right_after_new_note_land_in_the_new_notes_title_in_the_live_window() {
    new_notes_take_the_keys_typed_right_after_new_note(Session::live_notes());
}

/// "+" and its title typed at once, several times over before the host answers any: each new
/// note gets the keys typed after its own press, none another note's.
fn keys_typed_after_each_of_several_presses_land_in_the_note_that_press_made(mut session: Session) {
    let names = ["Alpha", "Beta", "Gamma"];
    for name in names {
        let plus = session.new_note_button();
        session.window.click_at(plus.0, plus.1);
        session.window.type_text(name);
    }
    session.settle();
    let mut titles = session.titles();
    titles.sort();
    assert_eq!(
        titles,
        ["Alpha", "Beta", "Gamma"],
        "each note has the keys typed after its own press"
    );
}

#[test]
fn keys_typed_after_each_of_several_presses_land_in_their_own_note() {
    keys_typed_after_each_of_several_presses_land_in_the_note_that_press_made(Session::notes());
}

#[test]
fn keys_typed_after_each_of_several_presses_land_in_their_own_note_in_the_live_window() {
    keys_typed_after_each_of_several_presses_land_in_the_note_that_press_made(Session::live_notes());
}

#[test]
fn every_key_typed_while_the_connection_is_full_is_kept_and_reaches_the_workspace_once_it_has_workspace()
 {
    let busy = Arc::new(AtomicBool::new(false));
    let changes = Arc::default();
    let mut session = Session::crowded_notes(&busy, &changes);
    let plus = session.new_note_button();
    session.window.click_at(plus.0, plus.1);
    session.settle();
    // The connection's queue fills up while the person types fast into notes, open and new.
    busy.store(true, Ordering::SeqCst);
    let before = changes.lock().unwrap().len();
    let title = session.field("Untitled");
    session.window.click_at(title.0 + 40.0, title.1);
    for key in ["Be", "ta", "Gam", "ma"] {
        session.window.type_text(key);
        session.settle();
    }
    let body = session.body();
    session.window.click_at(body.0, body.1);
    session.window.type_text("Body");
    session.settle();
    let plus = session.new_note_button();
    session.window.click_at(plus.0, plus.1);
    session.window.type_text("WV");
    session.settle();
    let mut titles = session.titles();
    titles.sort();
    assert_eq!(
        titles,
        ["BetaGamma", "WV"],
        "every key is kept while the workspace cannot take it yet"
    );
    assert_eq!(
        changes.lock().unwrap().len(),
        before,
        "nothing went out while the queue was full"
    );
    // The queue has workspace again: the host's next frames hand the workspace all of it.
    busy.store(false, Ordering::SeqCst);
    session.settle();
    let mut workspace = automerge::AutoCommit::new();
    for change in changes.lock().unwrap().iter() {
        workspace.load_incoming(change).unwrap();
    }
    let mut record = session
        .host
        .instances()
        .next()
        .unwrap()
        .record()
        .doc()
        .clone();
    assert_eq!(
        workspace.get_heads(),
        record.get_heads(),
        "the workspace holds every key typed while its connection was full"
    );
}

#[test]
fn clicking_a_notes_title_takes_the_keyboard_from_its_body_in_the_live_window() {
    let mut session = Session::live_notes();
    let plus = session.new_note_button();
    session.window.click_at(plus.0, plus.1);
    session.settle();
    for round in 1..=2_u8 {
        // Each round clicks elsewhere in the fields, past their text: two clicks at one place
        // count as a double click, which selects a word.
        let shift = 40.0 * f64::from(round);
        let body = session.body();
        session.window.click_at(body.0 + shift, body.1);
        session.window.type_text("Body");
        session.settle();
        let title = session.field("Untitled");
        session.window.click_at(title.0 + shift, title.1);
        session.window.type_text("XY");
        session.settle();
        let titles = session.titles();
        let wanted = "XY".repeat(usize::from(round));
        assert!(
            titles.len() == 1 && titles[0].contains(&wanted),
            "round {round}: the keys went to the title the person clicked: {titles:?}"
        );
        let body = format!("{:?}", session.field_value("Write something"));
        assert!(
            body.contains(&"Body".repeat(usize::from(round))) && !body.contains("XY"),
            "round {round}: and none on into the body: {body}"
        );
    }
}

#[test]
fn who_edited_a_page_reads_as_the_name_the_window_knows_them_by_never_their_key() {
    // The person's id is their key, as a real workspace's ids are.
    let ada = "95522b97c3d1e0f4a7b8c9d0e1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4";
    let mut session = Session::new("so.tilo.lang.signed", "tests/signed", ada, |presence| {
        presence.set_self_participant(ui::permissions::UserId(ada.to_owned()), "Ada");
    });
    let root = session
        .hosted()
        .view()
        .and_then(tilo_ui_host::AppWindow::root_tree)
        .expect("the page is shown");
    let key = find(&root, &|node| {
        matches!(node.props.get("placeholder"), Some(tilo_ui_host::Value::Text(p)) if p == "Title")
    })
    .expect("the page has its title field")
    .key
    .clone();
    let rect = widget_by_id(session.window.root(), &key)
        .expect("the title field is laid out")
        .get_rect();
    session.window.click_at(rect.center().x, rect.center().y);
    session.window.type_text("Plan");
    session.settle();
    // The person's name changes, as a roster renames them: the page follows.
    session.window.harness().update_presence(|presence| {
        presence.set_self_participant(ui::permissions::UserId(ada.to_owned()), "Ada Lovelace");
        true
    });
    session.settle();
    let shown = session.shown();
    assert!(
        shown.iter().any(|text| text == "Edited by Ada Lovelace"),
        "the page names who edited it as the window does: {shown:?}"
    );
    assert!(
        !shown.iter().any(|text| text.contains("95522b97c3d1")),
        "nothing shows the person's key: {shown:?}"
    );
}

/// The desktop's bar over the live window, as the shell runs it: the window declares its slots
/// there, and what the person does with them reaches the window on its shell-ui connection,
/// naming the input the shell had sent the window by then.
struct Bar {
    shell: std::os::unix::net::UnixStream,
}

impl Bar {
    /// The bar over `session`'s live window, which the desktop hosts.
    fn over(session: &mut Session) -> Self {
        let (shell, app) = std::os::unix::net::UnixStream::pair().unwrap();
        let Window::Live(window) = &mut session.window else {
            panic!("the bar is over the live window");
        };
        window.test_floating_service_connection(
            ui::floating_surface::FloatingSurfaceServiceClient::from_test_stream(app).unwrap(),
        );
        Self { shell }
    }

    /// The person presses the bar's slot `name` after the input with the serial `after`; the
    /// window reads it at once, before the input it has not handled yet.
    fn press(&mut self, session: &mut Session, name: &str, after: u32) {
        use std::io::Write as _;
        let id = ui::window_toolbar::declared()
            .expect("the window declares its bar's slots")
            .actions
            .iter()
            .find(|action| action.name == name)
            .unwrap_or_else(|| panic!("the bar has {name:?}"))
            .id
            .clone();
        let mut frame = serde_json::to_vec(
            &ui::floating_surface::FloatingSurfaceHostMessage::ToolbarEvent {
                event: ui::window_toolbar::ToolbarEvent::Action { id },
                after_input: Some(after),
            },
        )
        .unwrap();
        frame.push(b'\n');
        self.shell.write_all(&frame).unwrap();
        let Window::Live(window) = &mut session.window else {
            panic!("the bar is over the live window");
        };
        window.test_pump_floating_service();
    }
}

/// The serial the next `count` input events end at.
fn input_after(count: u32) -> u32 {
    INPUT.with(std::cell::Cell::get).wrapping_add(count)
}

/// The person types in a note's body, presses "+" in the window's bar and types the new
/// note's title, quicker than the window reads its input: the press reaches the window before
/// the keys typed before it. It runs where the person made it, after those keys and before the
/// ones after it, so each lands where it was typed.
#[test]
fn a_bar_press_runs_between_the_keys_before_and_after_it_and_typing_after_it_where_the_keyboard_was_follows_into_what_it_opens()
 {
    ui::window_toolbar::set_hosted(true);
    let mut session = Session::live_notes();
    bar_press_between_the_keys(&mut session);
}

#[test]
fn a_bar_press_runs_between_the_keys_before_and_after_it_from_a_rich_body_too() {
    ui::window_toolbar::set_hosted(true);
    let mut session = Session::live_rich();
    bar_press_between_the_keys(&mut session);
}

/// The person types in a note's body, presses "+" in the bar and types the new note's title:
/// the keys typed after the press are the new note's title, wherever the keyboard was.
fn bar_press_between_the_keys(session: &mut Session) {
    let mut bar = Bar::over(session);
    bar.press(session, "New note", input_after(0));
    session.settle();
    session.window.type_text("First");
    session.settle();
    let body = session.body();
    session.window.click_at(body.0, body.1);
    session.settle();
    // "Body" is eight key events; the press comes after them, and reaches the window first.
    bar.press(session, "New note", input_after(8));
    session.window.type_text("Body");
    session.window.type_text("Echo");
    session.settle();
    let mut titles = session.titles();
    titles.sort();
    assert_eq!(
        titles,
        ["Echo", "First"],
        "the keys typed after the press are the new note's title"
    );
    let shown = session.shown();
    assert!(
        !shown
            .iter()
            .any(|text| text.contains("Body") || text.contains("Echo\n")),
        "the new note's body has none of the other's typing: {shown:?}"
    );
}

/// The texts of the open note's title and body, as the window shows them. The title is a plain
/// field; the body is rich text, read a line per paragraph.
fn open_note(session: &mut Session) -> (String, String) {
    let title = match session.field_value("Untitled") {
        Some(tilo_ui_host::Value::Text(text)) => text,
        None => String::new(),
        Some(other) => panic!("a title: {other:?}"),
    };
    (title, body_text(session))
}

/// With the keyboard in a note's body, "+" in the window's bar, run by the window before the
/// next keys reach it, and the new note's title typed: the keys go to the new note's title,
/// never its body, round after round.
#[test]
fn after_a_bar_press_the_window_ran_from_the_body_typing_goes_to_the_new_notes_title() {
    ui::window_toolbar::set_hosted(true);
    for notebook in 0..4 {
        typing_after_a_press_from_the_body_goes_to_the_new_notes_title(
            Session::live_notes(),
            notebook,
        );
    }
}

#[test]
fn after_a_bar_press_from_a_rich_body_typing_goes_to_the_new_notes_title() {
    ui::window_toolbar::set_hosted(true);
    for notebook in 0..4 {
        typing_after_a_press_from_the_body_goes_to_the_new_notes_title(
            Session::live_rich(),
            notebook,
        );
    }
}

fn typing_after_a_press_from_the_body_goes_to_the_new_notes_title(
    mut session: Session,
    notebook: usize,
) {
    let mut bar = Bar::over(&mut session);
    for round in 0..6 {
        if round % 2 == 1 {
            let body = session.body();
            session.window.click_at(body.0, body.1);
            session.settle();
            session.window.type_text("Text");
            session.settle();
        }
        bar.press(&mut session, "New note", input_after(0));
        // The press reaches the host, which runs it and answers, before the window's next
        // frame: the keys come first.
        if round % 3 == 2 {
            session.settle();
        } else {
            for _ in 0..20 {
                session.host.turn();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        session.window.type_text("Note title");
        session.settle();
        let title = match session.field_value("Untitled") {
            Some(tilo_ui_host::Value::Text(text)) => text,
            other => panic!("a title: {other:?}"),
        };
        assert_eq!(
            title, "Note title",
            "notebook {notebook}, round {round}: the typing is the new note's title"
        );
        assert_eq!(
            body_text(&mut session),
            "",
            "notebook {notebook}, round {round}: none of the typing went into the body"
        );
    }
}

/// The text of the open note's body, a line per paragraph, whether it is plain or rich.
fn body_text(session: &mut Session) -> String {
    match session.field_value("Write something") {
        Some(tilo_ui_host::Value::Text(text)) => text,
        Some(tilo_ui_host::Value::Rich(rich)) => rich
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        None => String::new(),
        Some(other) => panic!("a body: {other:?}"),
    }
}

/// "+", the title, Enter, the body: Enter in the title, which submits nothing, moves the
/// keyboard to the body, as native Notes does.
fn enter_in_a_new_notes_title_moves_to_its_body(mut session: Session) {
    let plus = session.new_note_button();
    session.window.click_at(plus.0, plus.1);
    session.settle();
    session.window.type_text("Groceries");
    session.settle();
    session
        .window
        .press_key(ui::winit::keyboard::NamedKey::Enter);
    session.settle();
    session.window.type_text("Milk");
    session.settle();
    let (title, body) = open_note(&mut session);
    assert_eq!(title, "Groceries");
    assert_eq!(body.trim_end(), "Milk");
}

#[test]
fn enter_in_a_new_notes_title_moves_the_keyboard_to_its_body() {
    enter_in_a_new_notes_title_moves_to_its_body(Session::notes());
}

#[test]
fn enter_in_a_new_notes_title_moves_the_keyboard_to_its_body_in_the_live_window() {
    enter_in_a_new_notes_title_moves_to_its_body(Session::live_notes());
}

/// The window is made by the component shown before its record and taken over by the one the
/// record opens, which closes the first. What its instance sends from then on (a change another
/// writer made at the record's origin, an undo) wakes the component running the window: the
/// window shows it then, not whenever something else wakes it.
#[test]
fn what_the_instance_sends_wakes_the_component_that_took_the_window_over() {
    use tilo_render_protocol::patch::Transport as _;
    use ui::wake::WakeScope;

    fn woken(scope: &mut WakeScope) -> std::sync::mpsc::Receiver<()> {
        let (woke, woken) = std::sync::mpsc::channel();
        scope
            .bind(move |_| {
                let _ = woke.send(());
            })
            .unwrap();
        woken
    }

    let (ours, theirs) = Channel::pair().unwrap();
    // The component shown before the record makes the window, its patch connection at hand.
    let mut opening = WakeScope::new();
    let _opening_woken = woken(&mut opening);
    let mut window = {
        let _entry = opening.enter();
        HostedWindow::over(theirs)
    };
    // The record opened: the component it opens takes the window over and closes the first.
    let mut running = WakeScope::new();
    let running_woken = woken(&mut running);
    drop(opening);
    {
        let _entry = running.enter();
        Widget::update(&mut window, &(), &());
    }
    // The instance sends what changed; the window's component is woken to show it.
    let mut instance = ChannelTransport::new(ours).unwrap();
    let nothing: [tilo_render_protocol::patch::ToUi; 0] = [];
    instance
        .send(tilo_render_protocol::patch::encode_batch(&nothing).unwrap())
        .unwrap();
    assert!(
        running_woken
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok(),
        "what the instance sent did not wake the component running the window"
    );
    {
        let _entry = running.enter();
        assert!(
            Widget::update(&mut window, &(), &()),
            "the window took nothing in"
        );
    }
    // Woken again for what it sends next: the watch waits again once the window took it.
    instance
        .send(tilo_render_protocol::patch::encode_batch(&nothing).unwrap())
        .unwrap();
    assert!(
        running_woken
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok(),
        "the next thing the instance sent did not wake the window"
    );
}

/// "+", a title typed, Ctrl+Z, Ctrl+Shift+Z: the window's undo takes back what was typed into the
/// bound field, as one step of the person's, and redo brings it back. One undo, the record's: the
/// field keeps no history of its own beside it.
fn typing_in_a_title_is_undone_and_redone(mut session: Session) {
    let plus = session.new_note_button();
    session.window.click_at(plus.0, plus.1);
    session.settle();
    session.window.type_text("Groceries");
    session.settle();
    assert_eq!(session.titles(), ["Groceries"]);
    session.window.chord(&["ctrl"], "z");
    session.settle();
    assert_eq!(
        session.titles(),
        ["Untitled"],
        "Ctrl+Z takes back the typing, and the note stays, untitled"
    );
    assert_eq!(
        session.field_value("Untitled"),
        Some(tilo_ui_host::Value::Text(String::new())),
        "the field shows it"
    );
    session.window.chord(&["ctrl", "shift"], "Z");
    session.settle();
    assert_eq!(
        session.titles(),
        ["Groceries"],
        "Ctrl+Shift+Z brings it back"
    );
    session.window.type_text("s");
    session.settle();
    session.window.chord(&["ctrl"], "z");
    session.settle();
    assert_eq!(
        session.titles(),
        ["Groceries"],
        "what was typed after the redo is a step of its own"
    );
    // Typed key by key, frames between them: each word is a step, as in every other app.
    for key in " for sunday".chars() {
        session.window.type_text(&key.to_string());
        session.settle();
    }
    session.window.chord(&["ctrl"], "z");
    session.settle();
    assert_eq!(
        session.titles(),
        ["Groceries for "],
        "Ctrl+Z takes back the last word typed"
    );
}

#[test]
fn typing_in_a_title_is_undone_and_redone_in_the_frame_harness() {
    typing_in_a_title_is_undone_and_redone(Session::notes());
}

#[test]
fn typing_in_a_title_is_undone_and_redone_in_the_live_window() {
    typing_in_a_title_is_undone_and_redone(Session::live_notes());
}

/// The centre of the notebook's row named `title`.
fn note_row(session: &mut Session, title: &str) -> (f64, f64) {
    let mut found = Vec::new();
    named(session.window.root(), title, &mut found);
    let rect = found
        .last()
        .copied()
        .unwrap_or_else(|| panic!("the notebook offers {title:?}"));
    (rect.center().x, rect.center().y)
}

/// A new note named `title`, its body open for typing.
fn new_note(session: &mut Session, title: &str) {
    let plus = session.new_note_button();
    session.window.click_at(plus.0, plus.1);
    session.settle();
    session.window.type_text(title);
    session
        .window
        .press_key(ui::winit::keyboard::NamedKey::Enter);
    session.settle();
}

/// A note's body is a document: Markdown typed into it is the structure the moment it closes
/// (`# `, `- `, `[ ] `, `**`), and what the app keeps are the document's paragraphs, with no
/// markers left in the text.
#[test]
fn a_rich_body_is_a_document_and_markdown_typed_into_it_becomes_its_paragraphs() {
    use tilo_render_protocol::patch::rich::{Attr, Mark as RichMark};
    let mut session = Session::live_rich();
    new_note(&mut session, "Groceries");
    let enter = ui::winit::keyboard::NamedKey::Enter;
    session.window.type_text("# Plan");
    session.window.press_key(enter);
    session.settle();
    session.window.type_text("- milk");
    // Enter continues the list; Enter on the empty item after it leaves it.
    for _ in 0..2 {
        session.window.press_key(enter);
        session.settle();
    }
    session.window.type_text("[ ] coffee");
    for _ in 0..2 {
        session.window.press_key(enter);
        session.settle();
    }
    session.window.type_text("**bold**");
    session.settle();
    let blocks = match session.field_value("Write something") {
        Some(tilo_ui_host::Value::Rich(rich)) => rich.blocks,
        other => panic!("a rich body: {other:?}"),
    };
    let written: Vec<(&str, &str)> = blocks
        .iter()
        .map(|block| (block.kind.as_str(), block.text.as_str()))
        .collect();
    assert_eq!(
        written,
        [
            ("heading", "Plan"),
            ("list_item", "milk"),
            ("list_item", "coffee"),
            ("paragraph", "bold"),
        ],
        "each marker is the structure, not text"
    );
    assert_eq!(
        blocks[0].attrs.get("level"),
        Some(&Attr::Int(1)),
        "a first-level heading"
    );
    assert_eq!(
        blocks[2].attrs.get("checked"),
        Some(&Attr::Bool(false)),
        "a checklist with its box empty"
    );
    assert_eq!(
        blocks[3].marks,
        [RichMark {
            start: 0,
            end: 4,
            name: "strong".into(),
            value: Attr::Bool(true),
        }],
        "the closing `**` made the run bold"
    );
}

/// A document typed one marker after another stays on the page's level: a list item, a task or a
/// quote typed after leaving a list does not nest one deeper for each line, and every block
/// paints inside the rect the layout gave it — a stack that ends short of its editor cuts the
/// text it paints.
#[test]
fn a_document_typed_after_a_list_stays_on_the_page_level_and_fills_its_editor() {
    let mut session = Session::rich();
    new_note(&mut session, "Errands");
    let enter = ui::winit::keyboard::NamedKey::Enter;
    for line in [
        "# Errands",
        "- milk",
        "- eggs",
        "[ ] coffee",
        "> Ask whether Tuesday morning works",
        "```rust",
    ] {
        session.window.type_text(line);
        session.settle();
        session.window.press_key(enter);
        session.settle();
        // A list, a checklist and a quote each take the Enter on their empty line after them.
        if matches!(line.chars().next(), Some('-' | '[' | '>')) {
            session.window.press_key(enter);
            session.settle();
        }
    }
    session.window.type_text("fn main() {}");
    session.settle();
    let blocks = match session.field_value("Write something") {
        Some(tilo_ui_host::Value::Rich(rich)) => rich.blocks,
        other => panic!("a rich body: {other:?}"),
    };
    let int_attr = |block: &tilo_render_protocol::patch::rich::Block, name: &str| match block
        .attrs
        .get(name)
    {
        Some(tilo_render_protocol::patch::rich::Attr::Int(value)) => Some(*value),
        _ => None,
    };
    let written: Vec<(&str, Option<i64>)> = blocks
        .iter()
        .map(|block| (block.kind.as_str(), int_attr(block, "level")))
        .collect();
    assert_eq!(
        written,
        [
            ("heading", Some(1)),
            ("list_item", Some(0)),
            ("list_item", Some(0)),
            ("list_item", Some(0)),
            ("quote", None),
            ("paragraph", None),
        ],
        "each marker starts its block on the page, not inside the list above it"
    );
    assert!(
        blocks
            .iter()
            .all(|block| int_attr(block, "nesting").is_none()),
        "no block carries a level from the list before it: {blocks:?}"
    );
    assert!(
        matches!(
            blocks.last().map(|block| block.attrs.get("code")),
            Some(Some(tilo_render_protocol::patch::rich::Attr::Bool(true)))
        ),
        "the fence made a code block: {blocks:?}"
    );
    let (editor, rects) = session
        .window
        .harness()
        .widget_mut::<ui::widget::BlockEditor>()
        .map(|editor| {
            let rects: Vec<ui::kurbo::Rect> = editor
                .blocks()
                .iter()
                .map(|block| block.line.get_rect())
                .collect();
            (editor.get_rect(), rects)
        })
        .expect("the body is the block editor");
    assert_eq!(rects.len(), 6, "one rect per block");
    let list_lane = rects[1].x0;
    assert!(
        rects.iter().all(|rect| rect.x0 <= list_lane),
        "no block starts right of a top-level list item's line, so a quote or a code block \
         typed after the list sits on the page and not inside it: {rects:?}"
    );
    assert!(
        rects.windows(2).all(|pair| pair[0].y1 <= pair[1].y0 + 0.5),
        "no block is drawn over the one after it: {rects:?}"
    );
    assert!(
        rects
            .iter()
            .all(|rect| rect.y0 >= editor.y0 - 0.5 && rect.y1 <= editor.y1 + 0.5),
        "every block sits inside the editor: {editor:?} {rects:?}"
    );
    assert!(
        (editor.y1 - rects.last().expect("the last block").y1).abs() < 0.5,
        "the blocks fill the editor: the height each block got is the height its text took"
    );
}

/// What a note's body was written as comes back when its note is opened again: the document is
/// the field's value, kept in the record, not a draft the window holds.
#[test]
fn a_rich_body_comes_back_as_its_document_when_its_note_is_opened_again() {
    let mut session = Session::live_rich();
    new_note(&mut session, "Groceries");
    session.window.type_text("# Plan");
    session
        .window
        .press_key(ui::winit::keyboard::NamedKey::Enter);
    session.settle();
    session.window.type_text("- milk");
    session.settle();
    new_note(&mut session, "Other");
    let row = note_row(&mut session, "Groceries");
    session.window.click_at(row.0, row.1);
    session.settle();
    let (title, body) = open_note(&mut session);
    assert_eq!(title, "Groceries");
    assert_eq!(body, "Plan\nmilk", "the document as it was written");
    let kinds: Vec<String> = match session.field_value("Write something") {
        Some(tilo_ui_host::Value::Rich(rich)) => {
            rich.blocks.iter().map(|block| block.kind.clone()).collect()
        }
        other => panic!("a rich body: {other:?}"),
    };
    assert_eq!(kinds, ["heading", "list_item"]);
}

/// The document editor as captured, when shots are asked for with `TILO_RICHBODY_SHOTS`: a
/// relative directory is under the build's target directory, where a remote builder hands it
/// back.
#[test]
fn shots() {
    let Some(dir) = std::env::var_os("TILO_RICHBODY_SHOTS") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let dir = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(target) if dir.is_relative() => std::path::PathBuf::from(target).join(dir),
        _ => dir,
    };
    std::fs::create_dir_all(&dir).unwrap();
    let _settled = ui::motion::testing::settled();
    for (shade, theme) in [("light", ui::Theme::light()), ("dark", ui::Theme::dark())] {
        let mut session = Session::rich();
        session.window.harness().set_theme(theme);
        new_note(&mut session, "Saturday");
        let enter = ui::winit::keyboard::NamedKey::Enter;
        // Typed as a person types it: Markdown markers become the document's blocks.
        let enter_again = |session: &mut Session| {
            session.window.press_key(enter);
            session.settle();
        };
        for line in [
            "# Errands",
            "- milk",
            "- eggs",
            "[ ] coffee",
            "> Ask whether Tuesday morning works",
            "```rust",
        ] {
            session.window.type_text(line);
            session.settle();
            enter_again(&mut session);
            // A list, a checklist and a quote each need the Enter on their empty line after
            // them to end and start the next block the person wants.
            if matches!(line.chars().next(), Some('-' | '[' | '>')) {
                enter_again(&mut session);
            }
        }
        session.window.type_text("fn main() {}");
        session.settle();
        session
            .window
            .harness()
            .render_png(&dir.join(format!("rich-body-{shade}.png")));
    }
}

/// A task checked off through the host: the mark the window draws fills over frames on its way
/// to marked, and the task ends checked. The frames are the window's own, the click is a
/// person's, and the change comes back through the host's patch connection.
#[test]
fn a_task_checked_off_through_the_host_fills_its_box_over_frames() {
    use ui::time::testing::with_monotonic_time;
    use ui::winit::keyboard::NamedKey;

    let mut session = Session::new(TASKS, "../tl/tasks", "ada", |_| {});
    // A task, added through the entry the window shows.
    let mut entries = Vec::new();
    named(session.window.root(), "Draft", &mut entries);
    let entry = *entries.first().expect("the inbox shows its entry");
    session.window.click_at(entry.center().x, entry.center().y);
    session.window.type_text("Water the plants");
    session.window.press_key(NamedKey::Enter);
    session.settle();

    // Opened: the task's own window has the box that stays checked, where a list row's closes
    // the row out of the list.
    let mut rows = Vec::new();
    named(session.window.root(), "Water the plants", &mut rows);
    let row = *rows.last().expect("the inbox shows the task");
    session.window.click_at(row.center().x, row.center().y);
    session.settle();

    let mut found = Vec::new();
    boxes(session.window.root(), &mut found);
    let (boxed, marked) = *found.first().expect("the task's window shows its box");
    assert!(!marked, "the task starts open");

    let start = ui::time::Instant::now();
    let rest = session.window.harness().render_rgba();
    let was = pixels_of(&rest, boxed);

    let pressed = start + ui::time::Duration::from_millis(100);
    with_monotonic_time(pressed, || {
        session.window.click_at(boxed.center().x, boxed.center().y);
    });
    let mut series = Vec::new();
    for step in 1..=40 {
        let at = pressed + ui::time::Duration::from_millis(16 * step);
        let pixels = with_monotonic_time(at, || {
            session.host.turn();
            session.window.frame();
            session.window.harness().render_rgba()
        });
        series.push(apart(&pixels_of(&pixels, boxed), &was));
        let moving = session
            .window
            .harness()
            .frame_schedule()
            .needs_animation_frame;
        if series.len() > 2 && !moving {
            break;
        }
    }

    let mut after = Vec::new();
    boxes(session.window.root(), &mut after);
    assert_eq!(
        after.first().map(|(_, marked)| *marked),
        Some(true),
        "the task is checked"
    );

    let settled = series.last().copied().unwrap_or_default();
    assert!(settled > 0.02, "the box changed: the frames say {series:?}");
    let between = series
        .iter()
        .filter(|at| **at > settled * 0.05 && **at < settled * 0.95)
        .count();
    assert!(
        between >= 2,
        "the mark fills over frames rather than landing at once: the frames say {series:?}"
    );
    assert!(
        series
            .iter()
            .rev()
            .take(3)
            .all(|at| (*at - settled).abs() < settled * 0.05),
        "it settles: the frames say {series:?}"
    );
}

/// The accessibility node of the window's first virtual list: the headings it carries and the
/// rows under them.
fn list_nodes(widget: &dyn Widget) -> Option<ui::a11y::AccessNode> {
    if widget.name() == "DescribedList"
        && let Some(list) = widget
            .as_any()
            .and_then(|any| any.downcast_ref::<ui::widget::DescribedList>())
            .and_then(ui::widget::DescribedList::list)
    {
        return Widget::accessibility(list);
    }
    widget.children().into_iter().find_map(list_nodes)
}

/// The task open beside the list is the list's selected row, and a task nobody has named yet
/// shows its place: an "Untitled" row under the heading for the days it does not have.
#[test]
fn a_new_task_shows_an_untitled_row_under_no_day_and_the_open_tasks_row_is_selected() {
    let mut session = Session::tasks();
    let mut found = Vec::new();
    named(session.window.root(), "New task", &mut found);
    let button = *found.last().expect("the window offers New task");
    session
        .window
        .click_at(button.center().x, button.center().y);
    session.settle();

    let list = list_nodes(session.window.root()).expect("the window shows its list");
    let heading = list
        .children
        .iter()
        .position(|node| {
            node.role == ui::a11y::Role::Button && node.name.as_deref() == Some("No date")
        })
        .expect("a task with no day stands under the heading for the ones without");
    let row = &list.children[heading.saturating_add(1)];
    assert_eq!(row.role, ui::a11y::Role::ListItem);
    assert_eq!(
        row.name.as_deref(),
        Some("Untitled"),
        "a row of a task nobody has named yet says so"
    );
    assert!(
        row.state.selected,
        "the row of the task open beside the list is the selected one"
    );
}

/// The actions the tasks app offers the person at this window, as the window's catalogue
/// lists them.
fn offered(session: &mut Session) -> Vec<(String, Option<String>)> {
    session
        .hosted()
        .view()
        .expect("the window is connected")
        .actions()
        .iter()
        .map(|action| (action.action_label.clone(), action.state.shortcut.clone()))
        .collect()
}

/// Whether a control the window shows is bound to `true` now: the box of a task the palette
/// checked off.
fn a_checked_control(session: &mut Session) -> bool {
    /// The keys of the bound controls under `node`, each carrying its tree's `prefix`.
    fn bound(node: &tilo_ui_host::UiNode, prefix: &str, out: &mut Vec<String>) {
        if node.binding.is_some() {
            out.push(format!("{prefix}{}", node.key));
        }
        for child in &node.children {
            bound(child, prefix, out);
        }
    }
    let view = session.hosted().view().expect("the window is connected");
    let mut keys = Vec::new();
    for (tree, prefix) in [
        (view.root_tree(), ""),
        (view.detail_tree(), tilo_ui_host::shell::DETAIL),
    ] {
        if let Some(tree) = tree {
            bound(&tree, prefix, &mut keys);
        }
    }
    let view = session.hosted().view().expect("the window is connected");
    keys.iter()
        .any(|key| matches!(view.bound_value(key), Some(tilo_ui_host::Value::Bool(true))))
}

/// Opens the window's own command palette on `query` and runs the row it leads with.
fn choose(session: &mut Session, query: &str) {
    session
        .window
        .open_search(ui::search::SearchRequest::this_app().with_query(query));
    session.settle();
    session
        .window
        .press_key(ui::winit::keyboard::NamedKey::Enter);
    session.settle();
}

#[test]
fn the_windows_command_palette_offers_the_apps_actions_with_their_keys() {
    let _settled = ui::motion::testing::settled();
    let mut session = Session::tasks();
    // What the app publishes for this window is what the window offers: the labels people
    // read, and the keys the app gives its actions.
    let actions = offered(&mut session);
    let labels: Vec<&str> = actions.iter().map(|(label, _)| label.as_str()).collect();
    assert!(labels.contains(&"New task"), "{labels:?}");
    let key = |label: &str| {
        actions
            .iter()
            .find(|(offered, _)| offered == label)
            .unwrap_or_else(|| panic!("{label} is offered"))
            .1
            .clone()
    };
    assert_eq!(key("New task").as_deref(), Some("Ctrl+n"));
    assert_eq!(key("Remove").as_deref(), Some("delete"));

    // The window's own palette lists them, as a native app window's does.
    let listed: Vec<(String, Option<String>)> = session
        .window
        .harness()
        .resources()
        .into_iter()
        .filter(|resource| resource.id.0.starts_with("tl:so.tilo.lang.tasks:"))
        .flat_map(|resource| resource.actions)
        .map(|action| (action.label.clone(), action.state.shortcut.clone()))
        .collect();
    assert!(
        listed.contains(&("New task".to_owned(), Some("Ctrl+n".to_owned()))),
        "{listed:?}"
    );
    // A session's palette builds each action row from that same published state, and paints a
    // declared key as the platform's keycaps: the app's keys are caps, not free text.
    let platform = ui::PlatformConventions::new(ui::Platform::Linux);
    let mut row = |label: &str| {
        let resources = session.window.harness().resources();
        let (resource, action) = resources
            .iter()
            .filter(|resource| resource.id.0.starts_with("tl:so.tilo.lang.tasks:"))
            .find_map(|resource| {
                resource
                    .actions
                    .iter()
                    .find(|action| action.label == label)
                    .map(|action| (resource, action))
            })
            .unwrap_or_else(|| panic!("{label} is published"));
        ui::CommandEntry::from_flat_action(&ui::tilo_actions::FlatAction {
            key: format!("{}\u{1f}{}", resource.id.as_str(), action.id),
            resource_id: resource.id.clone(),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            resource_presentation: None,
            action,
            depth: 0,
        })
    };
    let new_task = row("New task");
    assert_eq!(
        new_task.keybinding_label(platform).as_deref(),
        Some("Ctrl+N")
    );
    assert_eq!(
        row("Remove").keybinding_label(platform).as_deref(),
        Some("Delete")
    );
    session
        .window
        .open_search(ui::search::SearchRequest::this_app().with_query("New task"));
    session.settle();
    assert!(
        session.window.harness().palette_shown(),
        "the palette shows"
    );
    let shown: Vec<String> = session
        .window
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    assert!(
        shown.iter().any(|label| label.contains("New task")),
        "{shown:?}"
    );
}

#[test]
fn a_chosen_action_runs_at_the_app_and_the_task_offers_its_own() {
    let _settled = ui::motion::testing::settled();
    let mut session = Session::live_tasks();
    // Choosing the app's New task in the window's palette inserts a task at the app, which
    // opens it, so its own actions reach the palette with their key.
    choose(&mut session, "New task");
    let actions = offered(&mut session);
    let complete = actions
        .iter()
        .find(|(label, _)| label == "Complete")
        .expect("the opened task offers Complete");
    assert_eq!(complete.1.as_deref(), Some("Ctrl+enter"));

    // Running it there checks the task off at the app: the box the window shows for it is
    // checked, on the same record the palette wrote.
    assert!(
        !a_checked_control(&mut session),
        "the task starts unchecked"
    );
    choose(&mut session, "Complete");
    assert!(
        a_checked_control(&mut session),
        "the task the palette ran Complete on shows checked"
    );
}

/// The window's command palette as captured, when shots are asked for with
/// `TILO_PALETTE_SHOTS`: a relative directory is under the build's target directory, where a
/// remote builder hands it back.
#[test]
fn palette_shots() {
    let Some(dir) = std::env::var_os("TILO_PALETTE_SHOTS") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let dir = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(target) if dir.is_relative() => std::path::PathBuf::from(target).join(dir),
        _ => dir,
    };
    std::fs::create_dir_all(&dir).unwrap();
    let _settled = ui::motion::testing::settled();
    for (shade, theme) in [("light", ui::Theme::light()), ("dark", ui::Theme::dark())] {
        for (what, query) in [("palette", ""), ("palette-search", "new")] {
            let mut session = Session::tasks();
            session.window.harness().set_theme(theme.clone());
            let request = ui::search::SearchRequest::this_app();
            let request = if query.is_empty() {
                request
            } else {
                request.with_query(query)
            };
            session.window.open_search(request);
            session.settle();
            session
                .window
                .harness()
                .render_png(&dir.join(format!("{what}-{shade}.png")));
            // What the palette reads row by row, for a check that cannot look at the image.
            let labels: Vec<String> = session
                .window
                .harness()
                .a11y_nodes()
                .into_iter()
                .filter_map(|(_, node)| node.label().map(str::to_owned))
                .collect();
            std::fs::write(dir.join(format!("{what}-{shade}.txt")), labels.join("\n")).unwrap();
        }
    }
}

type Shown = (String, Option<ui::icons::Icon>, Option<Vec<String>>);

/// What the window's own palette reads for `query`, row by row: the title it
/// shows, the picture it paints and the chord it names.
fn palette_rows(session: &mut Session, query: &str) -> Vec<Shown> {
    if session.window.harness().palette_shown() {
        session
            .window
            .press_key(ui::winit::keyboard::NamedKey::Escape);
        session.settle();
    }
    session
        .window
        .open_search(ui::search::SearchRequest::this_app().with_query(query));
    session.settle();
    session
        .window
        .harness()
        .palette_rows()
        .into_iter()
        .map(|row| {
            let keycaps = match &row.row {
                Some(ui::search::SearchRow::Command(command)) => command.keycaps.clone(),
                _ => None,
            };
            (row.title, row.icon, keycaps)
        })
        .collect()
}

/// The modifier a command chord is written with.
fn command_modifier(conventions: ui::PlatformConventions) -> String {
    ui::HostShortcut::Search
        .shortcut(conventions)
        .keycap_labels(conventions)
        .first()
        .cloned()
        .expect("the command modifier has a name")
}

#[test]
fn the_windows_palette_lists_the_apps_own_resources_and_actions() {
    let _settled = ui::motion::testing::settled();
    let mut session = Session::tasks();
    session.settle();
    let conventions = ui::PlatformConventions::default();
    an_empty_windows_palette_lists_what_a_person_can_do_now(&mut session, conventions);
    an_open_tasks_palette_lists_what_the_task_lets_a_person_do(&mut session, conventions);
}

fn an_empty_windows_palette_lists_what_a_person_can_do_now(
    session: &mut Session,
    conventions: ui::PlatformConventions,
) {
    // Unasked, an empty window lists the app itself once and each command a
    // person can take now, drawn from what the action declares. An action that
    // asks for a pick a row cannot collect (Open, Remove, Move to, Delete
    // project outside a project) is not a row, and neither is one the app keeps
    // for agents.
    let undo = Some(
        ui::HostShortcut::Undo
            .shortcut(conventions)
            .keycap_labels(conventions),
    );
    let redo = Some(
        ui::HostShortcut::Redo
            .shortcut(conventions)
            .keycap_labels(conventions),
    );
    let command = command_modifier(conventions);
    let empty = palette_rows(session, "");
    assert_eq!(
        empty,
        vec![
            ("Tasks".to_owned(), Some(ui::icons::Icon::Layers), None),
            (
                "New task".to_owned(),
                Some(ui::icons::Icon::Plus),
                Some(vec![command, "N".to_owned()]),
            ),
            (
                "New project".to_owned(),
                Some(ui::icons::Icon::FolderPlus),
                None,
            ),
            (
                "Clear done".to_owned(),
                Some(ui::icons::Icon::CircleCheck),
                None,
            ),
            ("Undo".to_owned(), Some(ui::icons::Icon::Undo2), undo),
            ("Redo".to_owned(), Some(ui::icons::Icon::Redo2), redo),
            (
                "Checkpoint".to_owned(),
                Some(ui::icons::Icon::Bookmark),
                None,
            ),
        ],
        "the empty window's palette"
    );
    let labels: Vec<&str> = empty.iter().map(|(title, ..)| title.as_str()).collect();
    for internal in [
        "tilolang",
        "Submit",
        "Edit a text field",
        "Set a flag field",
        "Commit a planned action",
        "Run a block",
        "Read a resource's fields",
        "List the catalogue",
        "Focus",
        "Go to",
        "Quick add",
        "Add subtask",
        "Move to",
    ] {
        assert!(
            !labels.contains(&internal),
            "{internal} is never a person's row: {labels:?}"
        );
    }
    // Typed, only what it names.
    let new: Vec<String> = palette_rows(session, "new")
        .into_iter()
        .map(|(title, ..)| title)
        .collect();
    assert_eq!(
        new,
        ["New task", "New project"],
        "the query narrows the list to what it names"
    );
}

fn an_open_tasks_palette_lists_what_the_task_lets_a_person_do(
    session: &mut Session,
    conventions: ui::PlatformConventions,
) {
    let command = command_modifier(conventions);
    // With a task open, the palette lists what that task lets a person do, once
    // each: the task and the app it belongs to, then the actions bound to it.
    // Picks a row cannot collect (Assign to…, Move to, Open) stay where they
    // live, and no row repeats another.
    session
        .window
        .press_key(ui::winit::keyboard::NamedKey::Escape);
    session.settle();
    let mut found = Vec::new();
    named(session.window.root(), "New task", &mut found);
    let rect = *found.last().expect("the header offers New task");
    let (x, y) = (rect.center().x, rect.center().y);
    session.window.click_at(x, y);
    session.settle();
    let open = palette_rows(session, "");
    let titles: Vec<&str> = open.iter().map(|(title, ..)| title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Task",
            "Tasks",
            "New task",
            "New project",
            "Clear done",
            "Task summary",
            "Complete",
            "Snooze until…",
            "Move to inbox",
            "Set due date",
            "Prioritize",
            "Indent",
            "Outdent",
            "Undo",
        ],
        "the open task's palette"
    );
    let task = open
        .iter()
        .find(|(title, ..)| title == "Task")
        .expect("the open task is a row");
    assert_eq!(
        task.1,
        Some(ui::icons::Icon::Layers),
        "a resource row shows the picture of what it is"
    );
    assert!(
        open.iter().all(|(_, icon, _)| icon.is_some()),
        "no row is a bare line: {open:?}"
    );
    let complete = open
        .iter()
        .find(|(title, ..)| title == "Complete")
        .expect("the open task offers Complete");
    assert_eq!(
        complete.2,
        Some(vec![command, "Enter".to_owned()]),
        "and its own chord"
    );
}
