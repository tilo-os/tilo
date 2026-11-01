mod actions;
#[cfg(feature = "live")]
mod agent_review;
mod attachments;
mod compose;
#[cfg(feature = "live")]
mod custody;
mod presentation;
mod shell;
mod sync;

use super::*;
use crate::resource::TestMail;
use automerge::ReadDoc;

fn cursor_at(p: Point) -> WindowEvent {
    ui::synthetic_input::cursor_moved(p.x, p.y)
}
fn left_press() -> WindowEvent {
    ui::synthetic_input::mouse_press()
}
fn esc_event() -> WindowEvent {
    named_key(NamedKey::Escape)
}
fn named_key(k: NamedKey) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        Key::Named(k),
        None,
        ElementState::Pressed,
    ))
}
fn char_key(s: &str) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        Key::Character(s.into()),
        Some(s.into()),
        ElementState::Pressed,
    ))
}

/// Route `ev` to the app through the tree it was last laid out in (a
/// bare node before any layout).
fn dispatch(app: &mut MailApp, ev: &WindowEvent, doc: &mut AutoCommit) -> bool {
    let (taffy, node) = app.test_layout.take().unwrap_or_else(|| {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        (taffy, node)
    });
    let handled = ui::testing::Dispatch::new()
        .doc(doc)
        .layout(&taffy, node)
        .send(app, ev)
        .handled;
    if app.test_layout.is_none() {
        app.test_layout = Some((taffy, node));
    }
    handled
}

fn left_release() -> WindowEvent {
    ui::synthetic_input::mouse_release()
}

fn app() -> (MailApp, AutoCommit) {
    let mut app = MailApp::new_sample();
    let mut doc = crate::resource::TestMail::doc();
    app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
    app.sync(&doc, &automerge::ROOT);
    app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
    app.sync(&doc, &automerge::ROOT);
    (app, doc)
}

fn fetched(account: &str, uid: u32, validity: u32, at: i64) -> Message {
    let raw = format!(
        "From: Ada <ada@x.example>\r\nTo: {account}\r\nSubject: S{uid}\r\n\
Message-ID: <{uid}.{validity}@x.example>\r\n\r\nBody {uid}.\r\n"
    );
    let mut message = crate::transport::parse_eml(raw.as_bytes(), at, Folder::Inbox).unwrap();
    message.account = account.to_owned();
    message.server = Some(crate::model::ServerPlace {
        folder: Folder::Inbox,
        uid,
        uid_validity: validity,
    });
    message
}

impl MailApp {
    /// The undo time of every held message is up: they go.
    fn undo_time_is_up(&mut self, doc: &mut AutoCommit) {
        for held in &mut self.held_sends {
            held.due = ui::time::now();
        }
        self.release_held_sends(&ui::widget::Edits::lend(doc));
    }
}

struct InterleavedEngineFixture {
    app: MailApp,
    doc: AutoCommit,
    incoming: std::sync::mpsc::Sender<EngineEvent>,
    refresh_id: EngineRequestId,
    send_id: EngineRequestId,
    unrelated_id: EngineRequestId,
    invocation: tilo_actions::InvocationId,
}

impl InterleavedEngineFixture {
    fn new() -> Self {
        let (app, doc) = app();
        let (outgoing, _outgoing_rx) = std::sync::mpsc::channel();
        let (incoming, incoming_rx) = std::sync::mpsc::channel();
        app.state().engine = Some(EngineHandle {
            tx: outgoing,
            rx: incoming_rx,
        });
        let refresh_id = uuid::Uuid::new_v4();
        let send_id = uuid::Uuid::new_v4();
        let unrelated_id = uuid::Uuid::new_v4();
        let invocation = tilo_actions::InvocationId::new();
        app.state().resource_refresh_invocation = Some((refresh_id, invocation));
        let message = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].clone();
        let outgoing = Outgoing::new(
            "me@example.test",
            &crate::model::Draft {
                to: "you@example.test".into(),
                ..Default::default()
            },
        );
        app.state().pending_send.insert(
            send_id,
            PendingSend {
                outgoing,
                message,
                done: Box::new(|_| {}),
            },
        );
        for (id, kind) in [
            (refresh_id, EngineRequestKind::Fetch),
            (send_id, EngineRequestKind::Send),
            (unrelated_id, EngineRequestKind::StoreFlag),
        ] {
            app.state().pending_engine_requests.insert(
                id,
                PendingEngineRequest {
                    kind,
                    account: "me@example.test".into(),
                },
            );
        }
        app.state().awaiting_engine = true;
        Self {
            app,
            doc,
            incoming,
            refresh_id,
            send_id,
            unrelated_id,
            invocation,
        }
    }

    fn send(&self, event: EngineEvent) {
        self.incoming.send(event).unwrap();
    }

    fn drain(&mut self) {
        self.app
            .drain_engine(&mut ui::widget::Edits::lend(&mut self.doc));
    }
}

fn click_at(app: &mut MailApp, doc: &mut AutoCommit, p: Point) -> bool {
    dispatch(app, &cursor_at(p), doc);
    let pressed = dispatch(app, &left_press(), doc);
    let released = dispatch(app, &left_release(), doc);
    pressed || released
}

/// The first list row's centre, as the shell laid the list out.
fn first_row_centre(app: &MailApp) -> Point {
    let geometry = app.nav.geometry();
    let column = geometry.overview_column;
    Point::new(
        column.center().x,
        column.y0 + geometry.header_height + f64::from(Theme::dark().tokens.spacing.s6),
    )
}

/// The first accessibility node under `widget` that `matches`.
fn find_access(
    widget: &dyn Widget,
    matches: &dyn Fn(&ui::a11y::AccessNode) -> bool,
) -> Option<ui::a11y::AccessNode> {
    widget
        .accessibility()
        .filter(|node| matches(node))
        .or_else(|| {
            widget
                .children()
                .into_iter()
                .find_map(|child| find_access(child, matches))
        })
}
