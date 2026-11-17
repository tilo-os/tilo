//! A record every app of a workspace opens beside its own: the workspace's
//! emoji, kept by Chat and read and added to from any app's window, the
//! same record on each, decided by the workspace's grants on Chat's resource.

use tilo_app::prelude::*;
use tilo_app::tilo_workspace_emoji::{self as emoji, WorkspaceEmoji, workspace_emoji as verbs};

/// The workspace both apps' windows are in.
const WORKSPACE: &str = "6f0c3a52-8e1b-4d6a-9c2f-3b7e5d1a4c90";

/// A note in a shared workspace.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    pub text: String,
}

#[resource(
    kind = "test.note",
    app = "so.tilo.test-jottings",
    catalogue = "tests/locales"
)]
impl Note {
    /// Write the note.
    #[action]
    pub fn write(&mut self, text: String) {
        self.text = text;
    }
}

/// The app that keeps the workspace's emoji.
struct ChatApp;

impl ResourceApp for ChatApp {
    type Resource = Note;
    const ID: &'static str = emoji::OWNER;
    const TITLE: &'static str = "Chat";

    fn view(_cx: &mut ViewCx<Note>) -> Element {
        Box::new(Column::new())
    }
}

/// Another app of the workspace, drawing the workspace's emoji by name on
/// [`LINES`] lines, as every line a note lays out reads them.
struct NotesApp;

thread_local! {
    static LINES: std::cell::Cell<usize> = const { std::cell::Cell::new(1) };
}

impl ResourceApp for NotesApp {
    type Resource = Note;
    const ID: &'static str = "so.tilo.test-jottings";
    const TITLE: &'static str = "Notes";

    fn view(_cx: &mut ViewCx<Note>) -> Element {
        let shown = || -> Element {
            Box::new(Text::dynamic(|| {
                let names: Vec<String> = tilo_app::workspace_emoji::emoji()
                    .iter()
                    .map(|emoji| format!(":{}:", emoji.name))
                    .collect();
                if names.is_empty() {
                    "no emoji".to_owned()
                } else {
                    names.join(" ")
                }
            }))
        };
        let lines = LINES.with(std::cell::Cell::get);
        Box::new((0..lines).fold(Column::new(), |column, _| column.push(shown())))
    }
}

/// A picture as the record keeps one: a small PNG.
fn picture() -> String {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&[0; 64]);
    emoji::EmojiImage(png).to_base64()
}

fn add(name: &str) -> verbs::AddEmoji {
    verbs::AddEmoji {
        name: name.into(),
        image: picture(),
    }
}

fn names(record: &WorkspaceResource<WorkspaceEmoji>) -> Vec<String> {
    record
        .get()
        .names()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_app_of_the_workspace_reads_and_adds_to_the_one_record_its_owner_keeps() {
    let mut chat = TestApp::<ChatApp>::in_workspace(WORKSPACE).unwrap();
    let mut notes = TestApp::<NotesApp>::in_workspace(WORKSPACE).unwrap();
    let kept = chat
        .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
        .unwrap();
    let in_notes = notes
        .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
        .unwrap();
    let route = kept.record().route().unwrap();
    assert_eq!(route, in_notes.record().route().unwrap(), "one record");
    assert_eq!(route.app_id, emoji::OWNER, "on its owner's route");

    // Notes adds an emoji; Chat's copy takes it in, and Notes' window draws
    // it for its name.
    assert!(in_notes.may("add-emoji"));
    in_notes
        .invoke(&WorkspaceEmoji::ADD_EMOJI.with(add("party")))
        .unwrap();
    kept.record()
        .merge_peer_document(in_notes.record().document().unwrap())
        .unwrap();
    assert_eq!(names(&kept), ["party"]);
    notes.harness().relayout();
    let drawn = notes.harness().inspection_json().unwrap();
    assert!(drawn.contains(":party:"), "{drawn}");

    // Both apps' edits converge on the one record.
    kept.invoke(&WorkspaceEmoji::ADD_EMOJI.with(add("ship_it")))
        .unwrap();
    in_notes
        .record()
        .merge_peer_document(kept.record().document().unwrap())
        .unwrap();
    // The window follows what arrived in its refresh: its lines draw it.
    notes.harness().woken();
    let drawn = notes.harness().inspection_json().unwrap();
    assert!(drawn.contains(":party: :ship_it:"), "{drawn}");
    assert_eq!(names(&in_notes), ["party", "ship_it"]);

    // The person takes back their own.
    in_notes
        .invoke(&WorkspaceEmoji::REMOVE_EMOJI.with(verbs::RemoveEmoji {
            name: "party".into(),
        }))
        .unwrap();
    assert_eq!(names(&in_notes), ["ship_it"]);
}

#[test]
fn the_workspaces_grants_on_the_owners_resource_decide_from_any_app() {
    let mut notes = TestApp::<NotesApp>::with_role(tilo_app::Role::Viewer).unwrap();
    let record = notes
        .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
        .unwrap();
    assert!(!record.may("add-emoji"), "a viewer adds none");
    assert!(
        record
            .invoke(&WorkspaceEmoji::ADD_EMOJI.with(add("party")))
            .is_err()
    );
    assert!(names(&record).is_empty());

    // An editor adds and removes their own, but not someone else's.
    let mut notes = TestApp::<NotesApp>::with_role(tilo_app::Role::Editor).unwrap();
    let record = notes
        .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
        .unwrap();
    assert!(record.may("add-emoji") && record.may("remove-emoji"));
    assert!(
        !record.may("remove-any-emoji"),
        "removing another's is moderation"
    );

    // No app keeps it but its owner.
    assert!(
        notes
            .shared_workspace::<WorkspaceEmoji>(NotesApp::ID)
            .is_none()
    );
}

/// The first emoji added in a fresh workspace, from a refresh that runs while
/// something reads the record's store (as a browser page's frame can):
/// the action waits for the store instead of failing, and is applied, in
/// the order asked for, once the store is free.
#[test]
fn an_action_asked_for_while_the_store_is_in_use_waits_for_it() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let mut chat = TestApp::<ChatApp>::in_workspace(WORKSPACE).unwrap();
    let kept = chat
        .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
        .unwrap();
    assert!(names(&kept).is_empty(), "a fresh workspace has none yet");
    let settled = Rc::new(RefCell::new(Vec::new()));
    kept.record()
        .read(|_| {
            // Asked for at once, the store in use refuses it, saying why.
            let refused = kept
                .invoke(&WorkspaceEmoji::ADD_EMOJI.with(add("party")))
                .unwrap_err();
            assert!(refused.to_string().contains("in use"), "{refused}");
            for name in ["party", "ship_it"] {
                let settled = Rc::clone(&settled);
                kept.invoke_then(&WorkspaceEmoji::ADD_EMOJI.with(add(name)), move |added| {
                    settled.borrow_mut().push((name, added.is_ok()));
                });
            }
            // Refreshes while the store stays in use keep them waiting.
            let _ = kept.revision();
            assert!(settled.borrow().is_empty());
        })
        .unwrap();
    // The window's next refresh applies them, in order.
    assert_eq!(names(&kept), ["party", "ship_it"]);
    assert_eq!(*settled.borrow(), [("party", true), ("ship_it", true)]);

    // One the record refuses settles as refused, without waiting.
    kept.invoke_then(&WorkspaceEmoji::ADD_EMOJI.with(add("party")), {
        let settled = Rc::clone(&settled);
        move |added| settled.borrow_mut().push(("again", added.is_ok()))
    });
    assert_eq!(settled.borrow().last(), Some(&("again", false)));
}

/// The record is synced in the window's refresh, once a frame: the lines read what it followed.
#[test]
fn a_frame_syncs_the_workspaces_emoji_once_however_many_lines_draw_them() {
    let syncs_in_a_frame_of = |lines: usize| -> usize {
        LINES.with(|cell| cell.set(lines));
        let mut notes = TestApp::<NotesApp>::in_workspace(WORKSPACE).unwrap();
        let record = notes
            .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
            .unwrap();
        record
            .invoke(&WorkspaceEmoji::ADD_EMOJI.with(add("party")))
            .unwrap();
        notes.harness().relayout();
        let drawn = notes.harness().inspection_json().unwrap();
        assert_eq!(drawn.matches(":party:").count(), lines, "{drawn}");
        let ((), samples) =
            tilo_app::tilo_resource::profiling::observe(|| notes.harness().relayout());
        samples
            .iter()
            .filter(|sample| sample.phase == "framework.live_sync")
            .map(|sample| sample.calls)
            .sum()
    };
    let (few, many) = (syncs_in_a_frame_of(10), syncs_in_a_frame_of(200));
    assert_eq!(few, many, "syncs grow with the lines drawn");
    assert!(many <= 2, "{many} syncs in one frame");
}
