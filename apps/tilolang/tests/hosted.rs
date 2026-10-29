//! A hosted tilolang app as a person uses it: the window process's own window (the UI host's
//! half, `HostedWindow`) driven by pointer and key events, over a real patch connection to a
//! tilolang host running the app's instance on a broker the test plays. What the person types
//! lands in the field focused when they pressed the key, whenever the host answers.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a test: a failure is a panic"
)]

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
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
use ui::headless::{FrameHarness, InspectionNode};

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

/// A person at a Notes window whose instance a tilolang host runs.
struct Session {
    host: TilolangHost,
    window: FrameHarness,
    _inbox: Inbox,
}

impl Session {
    /// A person at a Notes window.
    fn notes() -> Self {
        Self::new(NOTES, "../tl/notes", "ada", |_| {})
    }

    /// `person` at a window of the package `app` in `dir`, whose presence `present` sets up.
    fn new(
        app: &str,
        dir: &str,
        person: &str,
        present: impl FnOnce(&mut ui::presence::PresenceCtx),
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
        let mut window = FrameHarness::new(
            ui::State::new(),
            Box::new(HostedWindow::over(theirs)),
            VIEWPORT,
        );
        window.update_presence(|presence| {
            present(presence);
            true
        });
        let mut session = Self {
            host,
            window,
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
            self.window.relayout();
            if changed {
                quiet = 0;
            } else {
                quiet += 1;
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }

    fn hosted(&self) -> &HostedWindow {
        self.window
            .root_widget()
            .as_any()
            .and_then(|any| any.downcast_ref::<HostedWindow>())
            .expect("the window is the hosted window")
    }

    /// The texts the window's views show.
    fn shown(&self) -> Vec<String> {
        fn texts(node: &tilo_ui_host::UiNode, out: &mut Vec<String>) {
            for value in node.props.values() {
                if let tilo_ui_host::Value::Text(text) = value {
                    out.push(text.clone());
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

    /// The titles of the notebook's notes, as the instance publishes them.
    fn titles(&self) -> Vec<String> {
        let hosted = self.host.instances().next().unwrap();
        let notes: BTreeSet<(String, String)> = hosted
            .published()
            .iter()
            // The notes, not the notebook: each note is an item of its record.
            .filter(|action| action.address.resource.contains("/i"))
            .map(|action| {
                (
                    action.address.resource.clone(),
                    action.resource_label.clone(),
                )
            })
            .collect();
        notes.into_iter().map(|(_, label)| label).collect()
    }

    /// The centre of the detail view's field whose placeholder is `placeholder`.
    fn field(&self, placeholder: &str) -> (f64, f64) {
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
        let id = format!("{}{key}", tilo_ui_host::shell::DETAIL);
        let inspection = self.window.inspection();
        let node = find_widget(&inspection.root, &id).unwrap_or_else(|| {
            let mut ids = Vec::new();
            test_ids(&inspection.root, &mut ids);
            panic!("the field {id} is laid out: {ids:?}")
        });
        let [x, y, w, h] = node.rect;
        (x + w / 2.0, y + h / 2.0)
    }

    /// The centre of the header's New note button.
    fn new_note_button(&mut self) -> (f64, f64) {
        let inspection = self.window.inspection();
        let mut found = Vec::new();
        named(&inspection.root, "New note", &mut found);
        let [x, y, w, h] = *found.last().expect("the window offers New note");
        (x + w / 2.0, y + h / 2.0)
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

fn test_ids(node: &InspectionNode, out: &mut Vec<String>) {
    if let Some(id) = &node.test_id {
        out.push(format!("{id}{:?}", node.rect));
    }
    for child in &node.children {
        test_ids(child, out);
    }
}

fn find_widget<'a>(node: &'a InspectionNode, id: &str) -> Option<&'a InspectionNode> {
    if node.test_id.as_deref() == Some(id) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_widget(child, id))
}

/// The rects of the widgets whose accessible label is `label`.
fn named(node: &InspectionNode, label: &str, out: &mut Vec<[f64; 4]>) {
    if node
        .semantics
        .as_ref()
        .is_some_and(|access| access.name.as_deref() == Some(label))
        && node.rect[2] > 0.0
    {
        out.push(node.rect);
    }
    for child in &node.children {
        named(child, label, out);
    }
}

#[test]
fn who_edited_a_page_reads_as_the_name_the_window_knows_them_by_never_their_key() {
    // The person's id is their key, as a real room's ids are.
    let ada = "95522b97c3d1e0f4a7b8c9d0e1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4";
    let mut session = Session::new("so.tilo.lang.signed", "tests/signed", ada, |presence| {
        presence.set_self_participant(ui::permissions::UserId(ada.to_owned()), "Ada Lovelace");
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
    let inspection = session.window.inspection();
    let [x, y, w, h] = find_widget(&inspection.root, &key)
        .expect("the title field is laid out")
        .rect;
    session.window.click_at(x + w / 2.0, y + h / 2.0);
    session.window.type_text("Plan");
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
