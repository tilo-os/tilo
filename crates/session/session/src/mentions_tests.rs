//! What a person is told when someone mentions them: one notice per place, once.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc as _};

use super::*;
use crate::platform::records::Changes;
use ui::mentions::{MARK, reading_nothing};
use ui::record::{RecordId, RecordKind};

const ADA: &str = "u-ada";
const BEA: &str = "u-bea";
const CAI: &str = "u-cai";
const NOTE: &str = "tab:00000000000000a1";
const NOTES: &str = "so.tilo.notes";
const CHAT: &str = "so.tilo.chat";
const WORKSPACE: &str = "6d1c1f36-0000-4000-8000-000000000001";

fn name(id: &str) -> String {
    match id {
        ADA => "Ada".into(),
        BEA => "Bea".into(),
        _ => id.into(),
    }
}

/// A device's records: the store they are kept in, the one stream their writes land in, and the
/// reader the role looks at them through — what a session hands its mentions.
struct Device {
    dir: tempfile::TempDir,
    store: Store,
    changes: Changes,
}

impl Device {
    /// A device holding `docs` as `app`'s records of [`WORKSPACE`], each written as its app writes
    /// it: what the person is told of is what this device's store holds.
    fn holding(docs: &[(&str, &str, &AutoCommit)]) -> Self {
        let held = Self::empty();
        for (app, route, doc) in docs {
            held.writes(app, route, &mut AutoCommit::clone(doc));
        }
        held
    }

    /// A device whose store holds nothing yet.
    fn empty() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let changes = Changes::default();
        let store = crate::platform::records::noting(
            std::sync::Arc::new(app_state::FsStore::new(dir.path())) as Store,
            changes.clone(),
        );
        Self {
            dir,
            store,
            changes,
        }
    }

    /// Write `doc` as `app`'s record `route` of the workspace: the write its app makes, noted in
    /// the stream as it lands.
    fn writes(&self, app: &str, route: &str, doc: &mut AutoCommit) {
        Self::write_through(&self.store, app, route, doc);
    }

    /// The workspace held `doc` before this device followed it: written where the store keeps it,
    /// and noted as what the device already had rather than as news.
    fn already_held(&self, app: &str, route: &str, doc: &mut AutoCommit) {
        let plain: Store = std::sync::Arc::new(app_state::FsStore::new(self.dir.path()));
        Self::write_through(&plain, app, route, doc);
        self.changes.note(WORKSPACE, app, route, true);
    }

    /// `app`'s index names `route` as a record of `kind`, as its app writes it.
    fn kind(&self, app: &str, route: &str, kind: &str) {
        let mut doc = AutoCommit::new();
        let named = ui::record::index::reference(
            &mut doc,
            &RecordId::parse(route).expect("a record id"),
            &RecordKind::new(kind),
        );
        assert!(named, "{route} is named in {app}'s index");
        doc.commit();
        Self::write_doc(
            &self.store,
            &StorageKey::workspace_index(app, WORKSPACE),
            &mut doc,
        );
    }

    fn write_through(store: &Store, app: &str, route: &str, doc: &mut AutoCommit) {
        Self::write_doc(store, &StorageKey::record(app, WORKSPACE, route), doc);
    }

    /// Write `doc` at `key` through `store`, as a writer of it does.
    fn write_doc(store: &Store, key: &StorageKey, doc: &mut AutoCommit) {
        let lease = store.lease(key).expect("the key is this test's");
        let bytes = doc.save();
        let actor: Vec<u8> = doc.get_actor().to_bytes().to_vec();
        store
            .write(key, WriteOp::Replace(bytes), Some(&actor))
            .expect("the document is written");
        store.flush(Some(key)).expect("the document is kept");
        drop(lease);
    }

    /// The role over these records, telling the person `shown`, who is Bea.
    fn role(&self, shown: &Rc<Shown>) -> Mentions {
        self.role_as(BEA, shown)
    }

    /// The role over these records for `person`, telling them through `shown`.
    fn role_as(&self, person: &str, shown: &Rc<Shown>) -> Mentions {
        Mentions::over(Given {
            person: vec![person.to_owned()],
            store: Some(self.store.clone()),
            records: Some(Rc::new(crate::platform::records::Linked::over(
                self.store.clone(),
                self.changes.clone(),
            )) as Rc<dyn Records>),
            notices: Some(Rc::clone(shown) as Rc<dyn Notices>),
            name: Rc::new(name),
            wake: None,
        })
    }
}

/// The person's notifications, as a host shows them.
#[derive(Default)]
struct Shown {
    up: RefCell<BTreeMap<String, Notice>>,
    chosen: RefCell<Vec<Place>>,
}

impl Notices for Shown {
    fn show(&self, notice: &Notice) {
        self.up
            .borrow_mut()
            .insert(notice.tag.clone(), notice.clone());
    }

    fn withdraw(&self, tag: &str) {
        self.up.borrow_mut().remove(tag);
    }

    fn take_chosen(&self) -> Vec<Place> {
        std::mem::take(&mut *self.chosen.borrow_mut())
    }
}

/// Look again, past the pause the role keeps between two looks.
fn look_again(mentions: &Mentions) {
    let after = ui::time::now().checked_add(ui::time::Duration::from_secs(1));
    mentions.step_at(after.expect("a second after now"));
}

/// A notebook as `tilo_resource::Collection` keeps it: one page, its title and a paragraph whose
/// text marks `mention` (as `tilo-doc` does), written by `by`.
fn notebook(text: &str, mention: Option<(usize, usize, &str)>, by: &str) -> AutoCommit {
    let mut doc = AutoCommit::new();
    ui::record::init(
        &mut doc,
        &RecordId::parse(NOTE).unwrap(),
        &RecordKind::new("notes.notebook"),
        "workspace",
    );
    let pages = doc.put_object(ROOT, "pages", ObjType::Map).unwrap();
    let entry = doc.put_object(&pages, "p1", ObjType::Map).unwrap();
    doc.put(&entry, "k", "a").unwrap();
    let page = doc.put_object(&entry, "v", ObjType::Map).unwrap();
    doc.put(&page, "title", "Launch plan").unwrap();
    let body = doc.put_object(&page, "body", ObjType::Map).unwrap();
    let blocks = doc.put_object(&body, "blocks", ObjType::Map).unwrap();
    let block = doc.put_object(&blocks, "b7", ObjType::Text).unwrap();
    doc.splice_text(&block, 0, 0, text).unwrap();
    let length = text.chars().count();
    doc.mark(
        &block,
        Mark::new("author".into(), by, 0, length),
        ExpandMark::None,
    )
    .unwrap();
    if let Some((start, end, person)) = mention {
        doc.mark(
            &block,
            Mark::new(MARK.into(), person, start, end),
            ExpandMark::None,
        )
        .unwrap();
    }
    doc.commit();
    doc
}

/// The block `b7` of the notebook `doc` holds the mention.
fn notebook_block(doc: &AutoCommit) -> ObjId {
    doc.get(ROOT, "pages")
        .ok()
        .flatten()
        .and_then(|(_, pages)| doc.get(&pages, "p1").ok().flatten())
        .and_then(|(_, entry)| doc.get(&entry, "v").ok().flatten())
        .and_then(|(_, page)| doc.get(&page, "body").ok().flatten())
        .and_then(|(_, body)| doc.get(&body, "blocks").ok().flatten())
        .and_then(|(_, blocks)| doc.get(&blocks, "b7").ok().flatten())
        .map(|(_, block)| block)
        .unwrap()
}

/// The device holding a notebook that mentions Bea, the notices it told, and the role.
fn beas_notebook() -> (Device, Rc<Shown>, Mentions) {
    let written = notebook("Ask @Bea about the date", Some((4, 8, BEA)), ADA);
    let device = Device::holding(&[(NOTES, NOTE, &written)]);
    let shown = Rc::new(Shown::default());
    let mentions = device.role(&shown);
    mentions.step();
    (device, shown, mentions)
}

#[test]
fn a_mention_in_a_record_is_one_notice_for_the_reader_it_names() {
    let (_held, shown, _mentions) = beas_notebook();
    let up = shown.up.borrow();
    assert_eq!(up.len(), 1, "{up:?}");
    let notice = up.values().next().unwrap();
    assert_eq!(notice.title, "Ada mentioned you in ‘Launch plan’");
    assert_eq!(notice.body, "Ask @Bea about the date");
    assert_eq!(notice.app, "Notes");
    let place = notice.place.clone().expect("it opens what it names");
    assert_eq!(
        place,
        Place {
            app: NOTES.into(),
            record: NOTE.into(),
            resource: "notes.notebook/pages/p1".into(),
        }
    );
    assert_eq!(Place::from_activation(&place.activation()), Some(place));
}

#[test]
fn nobody_else_in_the_workspace_is_told() {
    let written = notebook("Ask @Bea about the date", Some((4, 8, BEA)), ADA);
    for other in [CAI, ADA] {
        let device = Device::holding(&[(NOTES, NOTE, &written)]);
        let theirs = Rc::new(Shown::default());
        device.role_as(other, &theirs).step();
        assert!(theirs.up.borrow().is_empty(), "{other}");
    }
}

#[test]
fn reopening_or_reloading_tells_nothing_again() {
    let (device, shown, mentions) = beas_notebook();
    let mut written = notebook("Ask @Bea about the date", Some((4, 8, BEA)), ADA);
    device.writes(NOTES, NOTE, &mut written);
    look_again(&mentions);
    assert_eq!(shown.up.borrow().len(), 1, "nothing is told twice");

    // The device restarted: what it told is kept in its store.
    let after = Rc::new(Shown::default());
    let restarted = device.role(&after);
    device.writes(NOTES, NOTE, &mut written);
    restarted.step();
    assert!(
        after.up.borrow().is_empty(),
        "a restart tells nothing again"
    );
}

#[test]
fn a_mention_edited_away_is_withdrawn() {
    let mut written = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    let device = Device::holding(&[(NOTES, NOTE, &written)]);
    let shown = Rc::new(Shown::default());
    let mentions = device.role(&shown);
    mentions.step();
    assert_eq!(shown.up.borrow().len(), 1);

    let block = notebook_block(&written);
    written
        .unmark(&block, MARK, 0, 8, ExpandMark::None)
        .unwrap();
    written.commit();
    device.writes(NOTES, NOTE, &mut written);
    look_again(&mentions);
    assert!(
        shown.up.borrow().is_empty(),
        "the notice went with the mention"
    );
}

#[test]
fn a_mention_the_workspace_held_before_this_device_followed_it_is_not_news() {
    let mut written = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    let device = Device::empty();
    device.already_held(NOTES, NOTE, &mut written);
    let shown = Rc::new(Shown::default());
    let mentions = device.role(&shown);
    mentions.step();
    assert!(shown.up.borrow().is_empty(), "{:?}", shown.up.borrow());

    // The record changes again: the same mention is not told twice.
    let block = notebook_block(&written);
    written.splice_text(&block, 0, 0, "Please ").unwrap();
    written.commit();
    device.writes(NOTES, NOTE, &mut written);
    look_again(&mentions);
    assert!(shown.up.borrow().is_empty());
}

#[test]
fn choosing_a_notice_gives_the_place_the_host_opens() {
    let (_held, shown, mentions) = beas_notebook();
    let place = shown
        .up
        .borrow()
        .values()
        .next()
        .and_then(|notice| notice.place.clone())
        .unwrap();
    shown.chosen.borrow_mut().push(place.clone());
    mentions.step();
    assert_eq!(mentions.take_chosen(), vec![place]);
    assert!(mentions.take_chosen().is_empty(), "a place is opened once");
}

#[test]
fn changes_arriving_in_a_burst_are_looked_at_together() {
    let device = Device::empty();
    let shown = Rc::new(Shown::default());
    let mentions = device.role(&shown);
    let first = ui::time::now();
    let mut written = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    device.writes(NOTES, NOTE, &mut written);
    mentions.step_at(first);
    assert_eq!(shown.up.borrow().len(), 1);

    let block = notebook_block(&written);
    written.splice_text(&block, 0, 0, "Please ").unwrap();
    written.commit();
    device.writes(NOTES, NOTE, &mut written);
    mentions.step_at(first + ui::time::Duration::from_millis(10));
    mentions.step_at(first + ui::time::Duration::from_millis(LOOK_EVERY_MS));
    assert_eq!(shown.up.borrow().len(), 1, "one record, one notice");
}

/// A chat part whose message mentions Bea, its mark's value written by `write_value` into the
/// span: the record names no identity of its own, as a chat's part does not.
fn chat_part_mention(write_value: &dyn Fn(&mut AutoCommit, &ObjId)) {
    let mut chat = AutoCommit::new();
    let channels = chat.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let entry = chat
        .put_object(&channels, "c_general", ObjType::Map)
        .unwrap();
    let channel = chat.put_object(&entry, "v", ObjType::Map).unwrap();
    chat.put(&channel, "name", "general").unwrap();

    let mut part = AutoCommit::new();
    // The chat's own record, and the part of it a page of messages is kept in.
    let chat_route = "tab:00000000000000c1".to_owned();
    let part_route = RecordId::parse(&chat_route)
        .expect("a record id")
        .part(0)
        .as_str()
        .to_owned();
    let messages = part.put_object(ROOT, "messages", ObjType::Map).unwrap();
    let entry = part.put_object(&messages, "m1", ObjType::Map).unwrap();
    let message = part.put_object(&entry, "v", ObjType::Map).unwrap();
    part.put(&message, "author", ADA).unwrap();
    part.put(&message, "channel", "c_general").unwrap();
    let body = part.put_object(&message, "body", ObjType::List).unwrap();
    let block = part.insert_object(&body, 0, ObjType::Map).unwrap();
    part.put(&block, "text", "@Bea can you look?").unwrap();
    let marks = part.put_object(&block, "marks", ObjType::List).unwrap();
    let span = part.insert_object(&marks, 0, ObjType::Map).unwrap();
    part.put(&span, "start", 0_i64).unwrap();
    part.put(&span, "end", 4_i64).unwrap();
    part.put(&span, "name", MARK).unwrap();
    write_value(&mut part, &span);

    let device = Device::holding(&[
        (CHAT, &part_route, &part),
        (CHAT, &chat_route, &chat),
        (CHAT, &part_route, &part),
    ]);
    // The kind a part does not name itself: its app's index names the records it keeps.
    device.kind(CHAT, &chat_route, "chat.workspace");
    let shown = Rc::new(Shown::default());
    device.role(&shown).step();

    let up = shown.up.borrow();
    assert_eq!(up.len(), 1, "{up:?}");
    let notice = up.values().next().unwrap();
    assert_eq!(notice.title, "Ada mentioned you in #general");
    assert_eq!(notice.body, "@Bea can you look?");
    assert_eq!(
        notice.place.clone().expect("it opens what it names"),
        Place {
            app: CHAT.into(),
            record: chat_route,
            resource: "chat.workspace/messages/m1".into(),
        }
    );
}

#[test]
fn a_message_in_a_chat_part_opens_the_message_in_the_chat() {
    chat_part_mention(&|part, span| {
        let value = part.put_object(span, "value", ObjType::Map).unwrap();
        part.put(&value, "$variant", "str").unwrap();
        part.put(&value, "$field/str/0", BEA).unwrap();
    });
}

#[test]
fn a_mark_value_of_the_older_flat_layout_still_names_its_person() {
    chat_part_mention(&|part, span| {
        let value = part.put_object(span, "value", ObjType::Map).unwrap();
        part.put(&value, "$variant", "str").unwrap();
        part.put(&value, "0", BEA).unwrap();
    });
}

#[test]
fn a_records_tags_are_known_by_its_app_and_route() {
    let mut held = Held::default();
    let written = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    let mut me = BTreeSet::new();
    me.insert(BEA.to_owned());
    let told = held.look(
        &Look {
            app: NOTES,
            record: NOTE,
            kind: "notes.notebook",
            doc: &written,
            tree: None,
            quiet: false,
            read: &reading_nothing,
        },
        &me,
        &name,
    );
    assert_eq!(told.show.len(), 1);
    assert!(held.knows(NOTES, NOTE));
}
