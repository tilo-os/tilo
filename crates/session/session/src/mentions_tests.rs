//! What a person is told when someone mentions them: one notice per place, once.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc as _};

use super::*;
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

/// The records a host reads: the documents it holds and what changed.
#[derive(Default)]
struct Recorder {
    changed: RefCell<(bool, Vec<Changed>)>,
    docs: RefCell<BTreeMap<(String, String), AutoCommit>>,
    kinds: RefCell<BTreeMap<String, String>>,
}

impl Recorder {
    fn holding(docs: &[(&str, &str, &AutoCommit)]) -> Rc<Self> {
        let held = Rc::new(Self::default());
        for (app, route, doc) in docs {
            held.docs
                .borrow_mut()
                .insert((app.to_string(), route.to_string()), AutoCommit::clone(doc));
        }
        held
    }

    /// A change reached `record` of `app`, as a host that had not replayed the workspace reports
    /// it.
    fn reached(&self, app: &str, record: &str) {
        self.changed.borrow_mut().1.push(Changed {
            workspace: WORKSPACE.to_owned(),
            app: app.to_owned(),
            record: record.to_owned(),
        });
    }

    /// A change reached `record` of `app` in what the workspace replayed: what it already held.
    fn replayed(&self, app: &str, record: &str) {
        self.changed.borrow_mut().0 = true;
        self.reached(app, record);
    }
}

impl Records for Recorder {
    fn take_changed(&self) -> (bool, Vec<Changed>) {
        std::mem::take(&mut *self.changed.borrow_mut())
    }

    fn document(&self, app: &str, workspace: &str, record: &str) -> Option<AutoCommit> {
        assert_eq!(workspace, WORKSPACE);
        self.docs
            .borrow()
            .get(&(app.to_owned(), record.to_owned()))
            .map(AutoCommit::clone)
    }

    fn kind(&self, app: &str, _workspace: &str) -> String {
        self.kinds.borrow().get(app).cloned().unwrap_or_default()
    }
}

/// The person's notifications, as a host shows them.
#[derive(Default)]
struct Shown {
    up: RefCell<BTreeMap<String, Notice>>,
    chosen: RefCell<Vec<(String, Place)>>,
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

    fn take_chosen(&self) -> Vec<(String, Place)> {
        std::mem::take(&mut *self.chosen.borrow_mut())
    }
}

fn role(records: &Rc<Recorder>, shown: &Rc<Shown>, store: Option<Store>) -> Mentions {
    Mentions::over(Given {
        person: vec![BEA.to_owned()],
        store,
        records: Some(Rc::clone(records) as Rc<dyn Records>),
        notices: Some(Rc::clone(shown) as Rc<dyn Notices>),
        name: Rc::new(name),
        wake: None,
    })
}

/// The first look of a session, and the next one after the pause between looks.
fn later(now: ui::time::Instant) -> ui::time::Instant {
    now + ui::time::Duration::from_millis(LOOK_EVERY_MS)
}

/// A device's store to keep what was told in.
fn kept_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(app_state::FsStore::new(dir.path())) as Store;
    (dir, store)
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

#[test]
fn a_mention_in_a_record_is_one_notice_for_the_reader_it_names() {
    let written = notebook("Ask @Bea about the date", Some((4, 8, BEA)), ADA);
    let records = Recorder::holding(&[(NOTES, NOTE, &written)]);
    let shown = Rc::new(Shown::default());
    let (_dir, store) = kept_store();
    let mentions = role(&records, &shown, Some(store));
    records.reached(NOTES, NOTE);
    mentions.step();

    let up = shown.up.borrow();
    assert_eq!(up.len(), 1, "{up:?}");
    let notice = up.values().next().unwrap();
    assert_eq!(notice.title, "Ada mentioned you in ‘Launch plan’");
    assert_eq!(notice.body, "Ask @Bea about the date");
    assert_eq!(notice.app, "Notes");
    assert_eq!(
        notice.place,
        Place {
            app: NOTES.into(),
            record: NOTE.into(),
            resource: "notes.notebook/pages/p1".into(),
        }
    );
    assert_eq!(
        Place::from_activation(&notice.place.activation()),
        Some(notice.place.clone())
    );
    drop(up);

    // Someone else in the workspace, and the author, are told nothing.
    for other in [CAI, ADA] {
        let theirs = Rc::new(Shown::default());
        let (_dir, store) = kept_store();
        let mentions = Mentions::over(Given {
            person: vec![other.to_owned()],
            store: Some(store),
            records: Some(Rc::clone(&records) as Rc<dyn Records>),
            notices: Some(Rc::clone(&theirs) as Rc<dyn Notices>),
            name: Rc::new(name),
            wake: None,
        });
        records.reached(NOTES, NOTE);
        mentions.step();
        assert!(theirs.up.borrow().is_empty(), "{other}");
    }
}

#[test]
fn reopening_or_reloading_tells_nothing_again() {
    let written = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    let records = Recorder::holding(&[(NOTES, NOTE, &written)]);
    let shown = Rc::new(Shown::default());
    let (_dir, store) = kept_store();
    let mentions = role(&records, &shown, Some(store.clone()));
    records.reached(NOTES, NOTE);
    mentions.step();
    assert_eq!(shown.up.borrow().len(), 1);

    // The record changed again, elsewhere.
    records.reached(NOTES, NOTE);
    mentions.step();
    assert_eq!(shown.up.borrow().len(), 1, "nothing is told twice");

    // The device restarted: what it told is kept in its store.
    let after = Rc::new(Shown::default());
    let restarted = Mentions::over(Given {
        person: vec![BEA.to_owned()],
        store: Some(store),
        records: Some(Rc::clone(&records) as Rc<dyn Records>),
        notices: Some(Rc::clone(&after) as Rc<dyn Notices>),
        name: Rc::new(name),
        wake: None,
    });
    records.reached(NOTES, NOTE);
    restarted.step();
    assert!(
        after.up.borrow().is_empty(),
        "the device that restarted tells nothing again"
    );
}

#[test]
fn a_mention_edited_away_is_withdrawn() {
    let mut written = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    let records = Recorder::holding(&[(NOTES, NOTE, &written)]);
    let shown = Rc::new(Shown::default());
    let (_dir, store) = kept_store();
    let mentions = role(&records, &shown, Some(store));
    records.reached(NOTES, NOTE);
    mentions.step();
    assert_eq!(shown.up.borrow().len(), 1);

    let block = notebook_block(&written);
    written
        .unmark(&block, MARK, 0, 8, ExpandMark::None)
        .unwrap();
    written.commit();
    records
        .docs
        .borrow_mut()
        .insert((NOTES.to_owned(), NOTE.to_owned()), written);
    records.reached(NOTES, NOTE);
    mentions.step();
    assert!(
        shown.up.borrow().is_empty(),
        "the notice went with the mention"
    );
}

#[test]
fn a_mention_the_workspace_already_held_when_this_device_came_is_not_news() {
    let written = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    let records = Recorder::holding(&[(NOTES, NOTE, &written)]);
    let shown = Rc::new(Shown::default());
    let (_dir, store) = kept_store();
    let mentions = role(&records, &shown, Some(store));
    records.replayed(NOTES, NOTE);
    mentions.step();
    assert!(shown.up.borrow().is_empty(), "{:?}", shown.up.borrow());

    // What it already held is taken as seen: a later change of it is news only if it mentions
    // them anew.
    let mut changed = written.clone();
    let block = notebook_block(&changed);
    changed.put(&block, "title", "Launch plan v2").unwrap();
    changed.commit();
    records
        .docs
        .borrow_mut()
        .insert((NOTES.to_owned(), NOTE.to_owned()), changed);
    records.reached(NOTES, NOTE);
    mentions.step();
    assert!(
        shown.up.borrow().is_empty(),
        "the same mention is not told twice"
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

/// A chat part whose message mentions Bea, its mark's value written by `write_value` into the
/// span: the record names no identity of its own, as a chat's part does not.
fn chat_part_mention(write_value: &dyn Fn(&mut AutoCommit, &ObjId)) {
    // The chat record names its channels; its messages are in a part.
    let mut chat = AutoCommit::new();
    let channels = chat.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let entry = chat
        .put_object(&channels, "c_general", ObjType::Map)
        .unwrap();
    let channel = chat.put_object(&entry, "v", ObjType::Map).unwrap();
    chat.put(&channel, "name", "general").unwrap();

    let mut part = AutoCommit::new();
    let part_route = RecordId::package_primary().part(0).as_str().to_owned();
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

    let records = Recorder::holding(&[(CHAT, &part_route, &part), (CHAT, "", &chat)]);
    records
        .kinds
        .borrow_mut()
        .insert(CHAT.to_owned(), "chat.workspace".to_owned());
    let shown = Rc::new(Shown::default());
    let (_dir, store) = kept_store();
    let mentions = role(&records, &shown, Some(store));
    records.reached(CHAT, &part_route);
    mentions.step();

    let up = shown.up.borrow();
    assert_eq!(up.len(), 1, "{up:?}");
    let notice = up.values().next().unwrap();
    assert_eq!(notice.title, "Ada mentioned you in #general");
    assert_eq!(notice.body, "@Bea can you look?");
    assert_eq!(
        notice.place,
        Place {
            app: CHAT.into(),
            record: String::new(),
            resource: "chat.workspace/messages/m1".into(),
        }
    );
}

#[test]
fn choosing_a_notice_gives_the_place_the_host_opens() {
    let written = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    let records = Recorder::holding(&[(NOTES, NOTE, &written)]);
    let shown = Rc::new(Shown::default());
    let (_dir, store) = kept_store();
    let mentions = role(&records, &shown, Some(store));
    records.reached(NOTES, NOTE);
    mentions.step();
    let place = shown
        .up
        .borrow()
        .values()
        .next()
        .map(|notice| notice.place.clone())
        .unwrap();

    shown
        .chosen
        .borrow_mut()
        .push((format!("mention:{}", "tag"), place.clone()));
    mentions.step();
    assert_eq!(mentions.take_chosen(), vec![place]);
    assert!(mentions.take_chosen().is_empty(), "a place is opened once");
}

#[test]
fn changes_arriving_in_a_burst_are_looked_at_together() {
    let written = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    let records = Recorder::holding(&[(NOTES, NOTE, &written)]);
    let shown = Rc::new(Shown::default());
    let (_dir, store) = kept_store();
    let mentions = role(&records, &shown, Some(store));
    let first = ui::time::now();
    records.reached(NOTES, NOTE);
    mentions.step_at(first);
    assert_eq!(shown.up.borrow().len(), 1);

    // The second change lands before the pause is over: it is looked at once it is.
    let mut changed = written;
    let block = notebook_block(&changed);
    changed.put(&block, "title", "Launch plan v2").unwrap();
    changed.commit();
    records
        .docs
        .borrow_mut()
        .insert((NOTES.to_owned(), NOTE.to_owned()), changed);
    records.reached(NOTES, NOTE);
    mentions.step_at(first + ui::time::Duration::from_millis(10));
    mentions.step_at(later(first));
    assert_eq!(
        shown.up.borrow().len(),
        1,
        "one record, one notice, whatever the burst"
    );
}

/// The tags of what one record told, as the role's state holds them: the document a record's
/// mentions are known by.
#[test]
fn a_records_tags_are_known_by_its_app_and_route() {
    let mut held = Held::default();
    assert!(!held.knows(NOTES, NOTE));
    let written = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    let mut me = BTreeSet::new();
    me.insert(BEA.to_owned());
    let doc = AutoCommit::clone(&written);
    let told = held.look(
        &Look {
            app: NOTES,
            record: NOTE,
            kind: "notes.notebook",
            doc: &doc,
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
