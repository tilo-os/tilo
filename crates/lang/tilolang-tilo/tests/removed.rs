//! A window whose page shows a resource the workspace no longer holds: the page closes and the
//! window goes back where it came from, whoever removed it. A draft discarded by another caller
//! closes its composer; a note another device deleted closes its page; a removal of something
//! else leaves the page alone.

use std::rc::Rc;
use std::sync::Arc;

use tilo_render_protocol::patch::local::{End, pair};
use tilo_render_protocol::patch::{self, Level, ToUi, Transport as _};
use tilolang_runtime::Meta;
use tilolang_tilo::OpenEffect;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::ids::MemberId;
use tilolang_tilo::runtime::{Actor, ResourceRef, Value};

use crate::fixtures::{Replica, Workspace, action_id, target};
use crate::legibility::call;

const MAIL_APP: &str = "so.tilo.mail";
const MAIL: (&str, &str) = (
    include_str!("../../../../apps/tl/mail/app.tl"),
    include_str!("../../../../apps/tl/mail/app.ids"),
);
const NOTES_APP: &str = "so.tilo.notes";
const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);
const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);

/// The packages a window of this app loads.
struct Apps(Vec<(&'static str, &'static str)>);

impl Platform for Apps {
    fn load(&self, host: &Host) -> u64 {
        for (source, ids) in &self.0 {
            host.load_builtin(source, ids).expect("the package loads");
        }
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-22T12:00:00Z".to_owned()
    }

    fn waker(&self) -> Waker {
        Arc::new(|| {})
    }
}

/// Ada's window of `app` over `workspace`, and its far end. A window of `embedder` shows no root
/// of its own: what its embedder opened stands at its detail level.
fn window(
    app: &str,
    packages: &[(&'static str, &'static str)],
    workspace: &Workspace,
    embedder: bool,
) -> (Instance, End) {
    let (app_end, window_end) = pair();
    let mut instance = Instance::new(
        Config {
            app: app.to_owned(),
            me: "ada".into(),
            mode: Mode::Live,
            device: None,
            store: workspace.store(),
            root: RootView::default(),
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
        },
        Rc::new(Apps(packages.to_vec())),
        Rc::new(tilolang_tilo::names::Installed),
        Box::new(app_end),
    );
    if embedder {
        instance.without_root();
    }
    instance.sync();
    (instance, window_end)
}

/// What the window was sent since the last call.
fn sent(window: &mut End) -> Vec<ToUi> {
    let mut messages = Vec::new();
    while let Ok(Some(batch)) = window.recv() {
        messages.extend(patch::decode_batch::<ToUi>(&batch).expect("the window is sent batches"));
    }
    messages
}

/// The tree the window shows at `level`, as text.
fn shown(window: &Instance, level: Level) -> String {
    format!(
        "{:?}",
        window
            .view(level)
            .expect("the level shows a view")
            .borrow()
            .tree()
    )
}

/// The id of the view `name` of `app`, as the package declares it.
fn view_id(host: &Host, app: &str, name: &str) -> String {
    host.package(app)
        .unwrap_or_else(|| panic!("{app} is loaded"))
        .meta
        .views
        .iter()
        .find(|view| view.name == name)
        .unwrap_or_else(|| panic!("{app} declares the view `{name}`"))
        .id
        .clone()
}

/// Ada's notebook, with a note called `title`.
fn notebook_with(replica: &Replica, title: &str) -> (ResourceRef, ResourceRef) {
    let notebook = replica.root(NOTES_APP);
    let note = target(&call(
        replica,
        "ada",
        NOTES_APP,
        "new_note",
        vec![Value::resource(&notebook)],
    ));
    call(
        replica,
        "ada",
        NOTES_APP,
        "retitle",
        vec![Value::resource(&note), Value::text(title)],
    );
    (notebook, note)
}

/// Open `target` in `view` at the window's detail level, as a link or a search hit does.
fn open(window: &mut Instance, target: &ResourceRef, view: Option<&str>) {
    if let Some(view) = view {
        window.open_with(
            &patch::ResourceRef {
                id: target.id_string(),
                ty: target.ty.clone(),
            },
            view,
        );
    } else {
        window.open_requests().borrow_mut().push(OpenEffect::Open {
            anchor: target.id_string(),
        });
    }
    window.update();
    window.sync();
}

/// Ada's window of Mail over a mailbox with a reply being written: the account and the draft the
/// app's own scenario stages, and the view its composer is.
struct Reply {
    window: Instance,
    far: End,
    host: Host,
    mail: ResourceRef,
    draft: ResourceRef,
    meta: Meta,
    editor: String,
}

fn mail_with_a_draft(workspace: &Workspace, rootless: bool) -> Reply {
    let (mut window, far) = window(MAIL_APP, &[MAIL], workspace, rootless);
    let host = window.host().expect("the window opened");
    let mail = host.root_of(MAIL_APP).expect("the mailbox");
    let meta = host.package(MAIL_APP).expect("mail is loaded").meta;
    let editor = view_id(&host, MAIL_APP, "editor");
    let scenario = meta
        .scenarios
        .iter()
        .find(|scenario| scenario.name == "a reply being written")
        .expect("mail stages a reply being written")
        .action
        .clone();
    host.runtime()
        .invoke_action(
            &Actor::from(MemberId::person("ada")),
            (scenario.as_str(), vec![]),
        )
        .expect("the scenario runs");
    window.sync();
    let Value::List(drafts) = host.runtime().read(&mail, "drafts").expect("the drafts") else {
        panic!("the mailbox lists its drafts");
    };
    let draft = target(drafts.first().expect("the reply was written"));
    Reply {
        window,
        far,
        host,
        mail,
        draft,
        meta,
        editor,
    }
}

/// Grace discards the reply's draft, as another surface of the mailbox does.
fn discard_elsewhere(reply: &Reply) {
    reply
        .host
        .runtime()
        .invoke_action(
            &Actor::from(MemberId::person("grace")),
            (
                action_id(&reply.meta, "discard").as_str(),
                vec![Value::resource(&reply.mail), Value::resource(&reply.draft)],
            ),
        )
        .expect("the draft is discarded");
}

#[test]
fn a_draft_discarded_by_another_caller_closes_its_composer() {
    let workspace = Workspace::new();
    let mut reply = mail_with_a_draft(&workspace, false);
    open(&mut reply.window, &reply.draft, Some(&reply.editor));
    let _ = sent(&mut reply.far);
    assert_eq!(
        reply.window.detail_target().map(ResourceRef::id_string),
        Some(reply.draft.id_string()),
        "the composer shows the draft"
    );

    discard_elsewhere(&reply);
    reply.window.store_moved();
    reply.window.sync();

    assert!(
        reply.window.detail_target().is_none(),
        "the composer closed with its draft"
    );
    let after = sent(&mut reply.far);
    assert!(
        after.iter().any(|message| matches!(message, ToUi::Close)),
        "the window goes back where it came from: {after:?}"
    );
}

#[test]
fn a_composer_with_nowhere_to_go_back_to_says_the_draft_is_gone() {
    let workspace = Workspace::new();
    let mut reply = mail_with_a_draft(&workspace, true);
    open(&mut reply.window, &reply.draft, Some(&reply.editor));
    let _ = sent(&mut reply.far);
    assert_eq!(
        reply.window.detail_target().map(ResourceRef::id_string),
        Some(reply.draft.id_string()),
        "the composer shows the draft"
    );

    discard_elsewhere(&reply);
    reply.window.store_moved();
    reply.window.sync();

    let after = sent(&mut reply.far);
    assert!(
        after.iter().any(|message| matches!(
            message,
            ToUi::Open {
                level: Level::Detail,
                tree,
                ..
            } if format!("{tree:?}").contains("tilolang-gone")
        )),
        "the page says in the system's words that it is gone: {after:?}"
    );
}

#[test]
fn a_note_deleted_by_another_device_closes_its_page() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[DISCUSSION, NOTES]);
    ada.commit();
    let (notebook, note) = notebook_with(&ada, "Q3 plan");
    // A note the notebook keeps, so the window has a list with nothing open after the removal.
    let later = target(&call(
        &ada,
        "ada",
        NOTES_APP,
        "new_note",
        vec![Value::resource(&notebook)],
    ));
    call(
        &ada,
        "ada",
        NOTES_APP,
        "retitle",
        vec![Value::resource(&later), Value::text("Later")],
    );
    ada.commit();

    let (mut window, mut far) = window(NOTES_APP, &[DISCUSSION, NOTES], &ada.doc, false);
    // The note opens in the notebook's tree, beside it, as a press on its row does.
    open(&mut window, &note, None);
    let _ = sent(&mut far);
    assert!(
        !shown(&window, Level::Root).contains("tilolang-empty"),
        "the notebook holds its notes: {}",
        shown(&window, Level::Root)
    );
    assert!(
        !shown(&window, Level::Root).contains("tilolang-nothing-open"),
        "the note's page stands beside the tree: {}",
        shown(&window, Level::Root)
    );

    // Another device deletes the note, and the change arrives here over sync.
    let other = ada.fork("bo", &[DISCUSSION, NOTES]);
    call(
        &other,
        "bo",
        NOTES_APP,
        "delete",
        vec![Value::resource(&notebook), Value::resource(&note)],
    );
    other.commit();
    other.send_to(&ada);
    window.store_moved();
    window.sync();

    let after = sent(&mut far);
    assert!(
        shown(&window, Level::Root).contains("tilolang-nothing-open"),
        "the note's page closed: {} / {after:?}",
        shown(&window, Level::Root)
    );
}

#[test]
fn a_removal_of_another_record_leaves_the_page_alone() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[DISCUSSION, NOTES]);
    ada.commit();
    let (notebook, kept) = notebook_with(&ada, "Q3 plan");
    let other = target(&call(
        &ada,
        "ada",
        NOTES_APP,
        "new_note",
        vec![Value::resource(&notebook)],
    ));
    call(
        &ada,
        "ada",
        NOTES_APP,
        "retitle",
        vec![Value::resource(&other), Value::text("Old notes")],
    );
    ada.commit();

    let (mut window, mut far) = window(NOTES_APP, &[DISCUSSION, NOTES], &ada.doc, false);
    open(&mut window, &kept, None);
    let _ = sent(&mut far);

    let elsewhere = ada.fork("bo", &[DISCUSSION, NOTES]);
    call(
        &elsewhere,
        "bo",
        NOTES_APP,
        "delete",
        vec![Value::resource(&notebook), Value::resource(&other)],
    );
    elsewhere.commit();
    elsewhere.send_to(&ada);
    window.store_moved();
    window.sync();

    let after = sent(&mut far);
    assert!(
        !shown(&window, Level::Root).contains("tilolang-empty"),
        "the notebook holds its notes: {} / {after:?}",
        shown(&window, Level::Root)
    );
    assert!(
        !shown(&window, Level::Root).contains("tilolang-nothing-open"),
        "the note open stays open: {} / {after:?}",
        shown(&window, Level::Root)
    );
    assert!(
        !after.iter().any(|message| matches!(message, ToUi::Close)),
        "no page closes: {after:?}"
    );
}
