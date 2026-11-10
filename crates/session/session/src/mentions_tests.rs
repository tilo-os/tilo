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

/// The records a host reads: the documents it holds, the kinds their apps keep, and what changed.
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

    /// A change reached `record` of `app`; `replayed` for one the workspace already held when this
    /// device first followed it.
    fn reached(&self, app: &str, record: &str, replayed: bool) {
        self.changed.borrow_mut().0 |= replayed;
        self.changed.borrow_mut().1.push(Changed {
            workspace: WORKSPACE.to_owned(),
            app: app.to_owned(),
            record: record.to_owned(),
        });
    }

    fn holds(&self, app: &str, record: &str, doc: &AutoCommit) {
        self.docs
            .borrow_mut()
            .insert((app.to_owned(), record.to_owned()), AutoCommit::clone(doc));
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

fn role(records: &Rc<Recorder>, shown: &Rc<Shown>, store: Store) -> Mentions {
    Mentions::over(Given {
        person: vec![BEA.to_owned()],
        store: Some(store),
        records: Some(Rc::clone(records) as Rc<dyn Records>),
        notices: Some(Rc::clone(shown) as Rc<dyn Notices>),
        name: Rc::new(name),
        wake: None,
    })
}

/// Look again, past the pause the role keeps between two looks.
fn look_again(mentions: &Mentions) {
    mentions.step_at(ui::time::now() + ui::time::Duration::from_secs(1));
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

/// The role over a notebook that mentions Bea, and the notices it told.
fn beas_notebook() -> (Rc<Recorder>, Rc<Shown>, Mentions, Store, tempfile::TempDir) {
    let written = notebook("Ask @Bea about the date", Some((4, 8, BEA)), ADA);
    let records = Recorder::holding(&[(NOTES, NOTE, &written)]);
    let shown = Rc::new(Shown::default());
    let (dir, store) = kept_store();
    let mentions = role(&records, &shown, store.clone());
    records.reached(NOTES, NOTE, false);
    mentions.step();
    (records, shown, mentions, store, dir)
}

#[test]
fn a_mention_in_a_record_is_one_notice_for_the_reader_it_names() {
    let (_records, shown, _mentions, _store, _dir) = beas_notebook();
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
}

#[test]
fn nobody_else_in_the_workspace_is_told() {
    let written = notebook("Ask @Bea about the date", Some((4, 8, BEA)), ADA);
    let records = Recorder::holding(&[(NOTES, NOTE, &written)]);
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
        records.reached(NOTES, NOTE, false);
        mentions.step();
        assert!(theirs.up.borrow().is_empty(), "{other}");
    }
}

#[test]
fn reopening_or_reloading_tells_nothing_again() {
    let (records, shown, mentions, store, _dir) = beas_notebook();
    records.reached(NOTES, NOTE, false);
    look_again(&mentions);
    assert_eq!(shown.up.borrow().len(), 1, "nothing is told twice");

    // The device restarted: what it told is kept in its store.
    let after = Rc::new(Shown::default());
    let restarted = role(&records, &after, store);
    records.reached(NOTES, NOTE, false);
    restarted.step();
    assert!(
        after.up.borrow().is_empty(),
        "a restart tells nothing again"
    );
}

#[test]
fn a_mention_edited_away_is_withdrawn() {
    let mut written = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    let records = Recorder::holding(&[(NOTES, NOTE, &written)]);
    let shown = Rc::new(Shown::default());
    let (_dir, store) = kept_store();
    let mentions = role(&records, &shown, store);
    records.reached(NOTES, NOTE, false);
    mentions.step();
    assert_eq!(shown.up.borrow().len(), 1);

    let block = notebook_block(&written);
    written
        .unmark(&block, MARK, 0, 8, ExpandMark::None)
        .unwrap();
    written.commit();
    records.holds(NOTES, NOTE, &written);
    records.reached(NOTES, NOTE, false);
    look_again(&mentions);
    assert!(
        shown.up.borrow().is_empty(),
        "the notice went with the mention"
    );
}

#[test]
fn a_mention_the_workspace_held_before_this_device_followed_it_is_not_news() {
    let written = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    let records = Recorder::holding(&[(NOTES, NOTE, &written)]);
    let shown = Rc::new(Shown::default());
    let (_dir, store) = kept_store();
    let mentions = role(&records, &shown, store);
    records.reached(NOTES, NOTE, true);
    mentions.step();
    assert!(shown.up.borrow().is_empty(), "{:?}", shown.up.borrow());

    // The record changes again: the same mention is not told twice.
    let mut changed = written;
    let block = notebook_block(&changed);
    changed.splice_text(&block, 0, 0, "Please ").unwrap();
    changed.commit();
    records.holds(NOTES, NOTE, &changed);
    records.reached(NOTES, NOTE, false);
    look_again(&mentions);
    assert!(shown.up.borrow().is_empty());
}

#[test]
fn choosing_a_notice_gives_the_place_the_host_opens() {
    let (_records, shown, mentions, _store, _dir) = beas_notebook();
    let place = shown
        .up
        .borrow()
        .values()
        .next()
        .map(|notice| notice.place.clone())
        .unwrap();
    shown.chosen.borrow_mut().push(place.clone());
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
    let mentions = role(&records, &shown, store);
    let first = ui::time::now();
    records.reached(NOTES, NOTE, false);
    mentions.step_at(first);
    assert_eq!(shown.up.borrow().len(), 1);

    let mut changed = written;
    let block = notebook_block(&changed);
    changed.splice_text(&block, 0, 0, "Please ").unwrap();
    changed.commit();
    records.holds(NOTES, NOTE, &changed);
    records.reached(NOTES, NOTE, false);
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
    let mentions = role(&records, &shown, store);
    records.reached(CHAT, &part_route, false);
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
