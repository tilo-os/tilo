//! A record every app of a room opens beside its own: the workspace's
//! emoji, kept by Chat and read and added to from any app's window, the
//! same record on each, decided by the room's grants on Chat's resource.

use tilo_app::prelude::*;
use tilo_app::tilo_workspace_emoji::{self as emoji, WorkspaceEmoji, workspace_emoji as verbs};

/// The workspace's room, which both apps' windows are in.
const ROOM: &str = "6f0c3a52-8e1b-4d6a-9c2f-3b7e5d1a4c90";

/// A note in a shared room.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    pub text: String,
}

#[resource(kind = "test.note", label = "Note")]
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

/// Another app of the room, drawing the workspace's emoji by name.
struct NotesApp;

impl ResourceApp for NotesApp {
    type Resource = Note;
    const ID: &'static str = "so.tilo.test-jottings";
    const TITLE: &'static str = "Notes";

    fn view(_cx: &mut ViewCx<Note>) -> Element {
        let shown = Text::dynamic(|| {
            let names: Vec<String> = tilo_app::workspace_emoji::emoji()
                .iter()
                .map(|emoji| format!(":{}:", emoji.name))
                .collect();
            if names.is_empty() {
                "no emoji".to_owned()
            } else {
                names.join(" ")
            }
        });
        Box::new(Column::new().push(Box::new(shown)))
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

fn names(record: &WorkspaceRecord<WorkspaceEmoji>) -> Vec<String> {
    record
        .get()
        .names()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_app_of_the_room_reads_and_adds_to_the_one_record_its_owner_keeps() {
    let mut chat = TestApp::<ChatApp>::in_room(ROOM).unwrap();
    let mut notes = TestApp::<NotesApp>::in_room(ROOM).unwrap();
    let kept = chat.workspace::<WorkspaceEmoji>(emoji::OWNER).unwrap();
    let in_notes = notes.workspace::<WorkspaceEmoji>(emoji::OWNER).unwrap();
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
fn the_rooms_grants_on_the_owners_resource_decide_from_any_app() {
    let mut notes = TestApp::<NotesApp>::with_role(tilo_app::Role::Viewer).unwrap();
    let record = notes.workspace::<WorkspaceEmoji>(emoji::OWNER).unwrap();
    assert!(!record.may("add-emoji"), "a viewer adds none");
    assert!(
        record
            .invoke(&WorkspaceEmoji::ADD_EMOJI.with(add("party")))
            .is_err()
    );
    assert!(names(&record).is_empty());

    // An editor adds and removes their own, but not someone else's.
    let mut notes = TestApp::<NotesApp>::with_role(tilo_app::Role::Editor).unwrap();
    let record = notes.workspace::<WorkspaceEmoji>(emoji::OWNER).unwrap();
    assert!(record.may("add-emoji") && record.may("remove-emoji"));
    assert!(
        !record.may("remove-any-emoji"),
        "removing another's is moderation"
    );

    // No app keeps it but its owner.
    assert!(notes.workspace::<WorkspaceEmoji>(NotesApp::ID).is_none());
}
