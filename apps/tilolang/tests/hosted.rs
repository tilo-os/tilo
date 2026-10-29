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

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use collaboration::{CollabEvent, CollabRequest, CollaborationManager, PeerTransport, RouteSpec};
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilolang_app::hosted::HostedWindow;
use tilolang_host::TilolangHost;
use tilolang_host::hosted::{Hosted, Spec};
use tilolang_host::phrases::Phrases;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::instance::room::RoomRecord;
use ui::headless::FrameHarness;
use ui::widget::Widget;

const NOTES: &str = "so.tilo.lang.notes";
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

/// The window the person uses: the headless frame harness, or the live `App` a window process
/// runs, fed input as its surface feeds it.
enum Window {
    Harness(Box<FrameHarness>),
    Live(Box<ui::App>),
}

impl Window {
    /// The live window's frames, as its surface runs them, until it is at rest.
    fn live_frame(app: &mut ui::App) {
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
    fn live_input(app: &mut ui::App, event: ui::winit::event::WindowEvent) {
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
    _inbox: Inbox,
}

impl Session {
    /// A person at a Notes window in the frame harness.
    fn notes() -> Self {
        Self::new(NOTES, "../tl/notes", "ada", |_| {})
    }

    /// A person at a Notes window the live `App` runs.
    fn live_notes() -> Self {
        Self::open(NOTES, "../tl/notes", "ada", |hosted| {
            let mut app = ui::App::new(ui::State::new(), Box::new(hosted));
            app.init_layout();
            Window::Live(Box::new(app))
        })
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
        let mut manager = CollaborationManager::with_peer_transport(Broker {
            route: route.clone(),
            sent,
            inbox: Rc::clone(&inbox),
            joined: None,
        });
        assert!(manager.send(CollabRequest::Hello {
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id,
            record: None,
            record_kind: None,
            handle: Some(person.to_owned()),
        }));
        let _ = manager.poll();
        let record = RoomRecord::new(automerge::AutoCommit::new(), manager, route.clone(), person);
        let wake: tilolang_tilo::background::Waker = Arc::new(|| {});
        let mut host = TilolangHost::new(None, Arc::clone(&wake));
        let kept = host.kept(app).unwrap();
        let (ours, theirs) = Channel::pair().unwrap();
        let watched = ours.as_fd().try_clone_to_owned().unwrap();
        let hosted = Hosted::with_record(
            Spec {
                app: app.to_owned(),
                room: route.cluster_id.to_string(),
                instance: 1,
                person: person.to_owned(),
            },
            record,
            Arc::new(package(dir)),
            &kept,
            Box::new(ChannelTransport::new(ours).unwrap()),
            Rc::new(Phrases::new("en-US")),
            wake,
        );
        host.add(hosted, Some(watched));
        let mut session = Self {
            host,
            window: window(HostedWindow::over(theirs)),
            _inbox: inbox,
        };
        session.settle();
        session
    }

    /// The host's frames and the window's, until neither has anything left to do.
    fn settle(&mut self) {
        let mut quiet = 0;
        while quiet < 20 {
            let changed = self.host.turn();
            self.window.frame();
            if changed {
                quiet = 0;
            } else {
                quiet += 1;
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
        let rect = widget_by_id(self.window.root(), "block_editor")
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

/// After a first note made and titled as a person does, "+" and at once the next title, every
/// key typed before the host answered, three times over: each new note takes the keyboard in
/// its title.
fn new_notes_take_the_keys_typed_right_after_new_note(mut session: Session) {
    let plus = session.new_note_button();
    session.window.click_at(plus.0, plus.1);
    session.settle();
    let title = session.field("Title");
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
            "UntitledFirst ",
            "UntitledNote title",
            "UntitledNote title",
            "UntitledNote title"
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

#[test]
fn notes_made_by_presses_the_host_has_not_answered_yet_each_take_what_was_typed_after_their_own() {
    let mut session = Session::live_notes();
    // From the empty notebook's New note, then the header's, before the host answered either.
    let plus = session.new_note_button();
    session.window.click_at(plus.0, plus.1);
    session.window.type_text("First");
    session.window.frame();
    let plus = session.new_note_button();
    session.window.click_at(plus.0, plus.1);
    session.window.type_text("Second");
    session.settle();
    let mut titles = session.titles();
    titles.sort();
    assert_eq!(
        titles,
        ["UntitledFirst", "UntitledSecond"],
        "what was typed after each press is that press's note's title"
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
        let title = session.field("Title");
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
    // The person's id is their key, as a real room's ids are.
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
