//! What a person is told when someone mentions them: one notice per place, once.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc as _};

use super::*;
use crate::platform::records::{Changes, Kept};
use ui::mentions::{MARK, reading_nothing};
use ui::resource::{DocumentId, ResourceType};

const ADA: &str = "u-ada";
const BEA: &str = "u-bea";

/// A member's id as the apps keep it: a mention's mark, or an item's author.
fn member_value(person: &str) -> String {
    serde_json::to_string(&ui::MemberId::person(person)).expect("a member id")
}
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

    fn write_through(store: &Store, app: &str, route: &str, doc: &mut AutoCommit) {
        let key = crate::platform::records::storage_key(app, WORKSPACE, route).expect("a route");
        Self::write_doc(store, &key, doc);
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
            records: Some(
                Rc::new(Kept::over(self.store.clone(), self.changes.clone())) as Rc<dyn Records>,
            ),
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
    notebook_at(&DocumentId::parse(NOTE).unwrap(), text, mention, by)
}

/// [`notebook`], kept as the record `id`.
fn notebook_at(
    id: &DocumentId,
    text: &str,
    mention: Option<(usize, usize, &str)>,
    by: &str,
) -> AutoCommit {
    let mut doc = AutoCommit::new();
    ui::resource::init(
        &mut doc,
        id,
        &ResourceType::new("notes.notebook"),
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
            Mark::new(MARK.into(), member_value(person), start, end),
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

tilo_test_env::scenario! {
    fn a_mention_is_one_notice_for_the_person_it_names_and_opens_its_place;

    phase a_mention_in_a_record_is_one_notice_for_the_reader_it_names;
    {
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
                resource: "notes.notebook".into(),
                anchor: "notes.notebook/pages/p1".into(),
            }
        );
        assert_eq!(Place::from_activation(&place.activation()), Some(place));
    }

    phase a_mention_in_a_file_opened_as_a_record_opens_that_record;
    // A document a file opened in is named by the file's path, and kept under its storage key: its
    // write is noted by the record's route, and the notice opens that record.
    {
        let file = DocumentId::for_file("/run/tilo/documents/Budget for Friday.txt");
        let route = file.route().to_owned();
        let written = notebook_at(&file, "Ask @Bea about the date", Some((4, 8, BEA)), ADA);
        let device = Device::holding(&[(NOTES, &route, &written)]);
        let shown = Rc::new(Shown::default());
        device.role(&shown).step();

        let up = shown.up.borrow();
        assert_eq!(up.len(), 1, "{up:?}");
        let place = up.values().next().unwrap().place.clone();
        assert_eq!(
            place.expect("it opens what it names").record,
            route,
            "the record by its route, not where it is kept"
        );
    }

    phase a_mention_in_an_apps_own_record_is_told;
    // An app's own record is the workspace's document, which also keeps its index, and travels
    // under the empty route: a mention in it is read there, as any other record's is.
    {
        let device = Device::empty();
        let shown = Rc::new(Shown::default());
        let mentions = device.role(&shown);
        let mut written = notebook("Ask @Bea about the date", Some((4, 8, BEA)), ADA);
        Device::write_doc(
            &device.store,
            &StorageKey::workspace_index(NOTES, WORKSPACE),
            &mut written,
        );
        mentions.step();
        let up = shown.up.borrow();
        assert_eq!(up.len(), 1, "{up:?}");
        assert_eq!(
            up.values().next().unwrap().title,
            "Ada mentioned you in ‘Launch plan’"
        );
    }

    phase nobody_else_in_the_workspace_is_told;
    {
        let written = notebook("Ask @Bea about the date", Some((4, 8, BEA)), ADA);
        for other in [CAI, ADA] {
            let device = Device::holding(&[(NOTES, NOTE, &written)]);
            let theirs = Rc::new(Shown::default());
            device.role_as(other, &theirs).step();
            assert!(theirs.up.borrow().is_empty(), "{other}");
        }
    }

    phase choosing_a_notice_gives_the_place_the_host_opens;
    {
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

    phase a_message_in_a_chat_part_opens_the_message_in_the_chat;
    {
        chat_part_mention(&|part, span| {
            let value = part.put_object(span, "value", ObjType::Map).unwrap();
            part.put(&value, "$variant", "str").unwrap();
            part.put(&value, "$field/str/0", member_value(BEA)).unwrap();
        });
    }

    phase a_mark_value_of_the_older_flat_layout_still_names_its_person;
    {
        chat_part_mention(&|part, span| {
            let value = part.put_object(span, "value", ObjType::Map).unwrap();
            part.put(&value, "$variant", "str").unwrap();
            part.put(&value, "0", member_value(BEA)).unwrap();
        });
    }
}

tilo_test_env::scenario! {
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "fixture arithmetic in test bodies, which the lint skips in a plain test"
    )]
    fn a_mention_is_told_once_and_withdrawn_with_its_mark;

    phase reopening_or_reloading_tells_nothing_again;
    {
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

    phase a_mention_edited_away_is_withdrawn;
    {
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

    phase a_mention_the_workspace_held_before_this_device_followed_it_is_not_news;
    {
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

    phase changes_arriving_in_a_burst_are_looked_at_together;
    {
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

    phase a_records_tags_are_known_by_its_app_and_route;
    {
        let mut held = Held::default();
        let written = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
        let mut me = BTreeSet::new();
        me.insert(ui::MemberId::person(BEA));
        let told = held.look(
            &Look {
                app: NOTES,
                record: NOTE,
                place: NOTE,
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
}

/// Chat's surface as this test names it: its own record's kind first, then its parts'.
const CHAT_SURFACE: &[u8] = b"format = 2\n\n[[resources]]\nkind = \"chat.workspace\"\nlabel = \"Chat\"\nversion = 1\n\n[[resources]]\nkind = \"chat.messages\"\nlabel = \"Messages\"\nversion = 1\n";

/// Post message `id` by Ada in #general into `part`, a paragraph whose mark span names Bea, its
/// value written by `write_value`.
fn post(
    part: &mut AutoCommit,
    id: &str,
    text: &str,
    write_value: &dyn Fn(&mut AutoCommit, &ObjId),
) {
    let messages = match part.get(ROOT, "messages").unwrap() {
        Some((_, messages)) => messages,
        None => part.put_object(ROOT, "messages", ObjType::Map).unwrap(),
    };
    let entry = part.put_object(&messages, id, ObjType::Map).unwrap();
    let message = part.put_object(&entry, "v", ObjType::Map).unwrap();
    part.put(&message, "author", member_value(ADA)).unwrap();
    part.put(&message, "channel", "c_general").unwrap();
    let body = part.put_object(&message, "body", ObjType::List).unwrap();
    let block = part.insert_object(&body, 0, ObjType::Map).unwrap();
    part.put(&block, "text", text).unwrap();
    let marks = part.put_object(&block, "marks", ObjType::List).unwrap();
    let span = part.insert_object(&marks, 0, ObjType::Map).unwrap();
    part.put(&span, "start", 0_i64).unwrap();
    part.put(&span, "end", 4_i64).unwrap();
    part.put(&span, "name", MARK).unwrap();
    write_value(part, &span);
    part.commit();
}

/// Chat as it keeps a workspace: its own record, which names no kind, with its conversations, and a
/// part of the chain of messages derived from it. Each message that mentions Bea is one notice,
/// opening that message in the chat's own record as a search hit on it does.
fn chat_part_mention(write_value: &dyn Fn(&mut AutoCommit, &ObjId)) {
    ui::app_surface::register_built_in(CHAT, CHAT_SURFACE);
    let mut chat = AutoCommit::new();
    let channels = chat.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let entry = chat
        .put_object(&channels, "c_general", ObjType::Map)
        .unwrap();
    let channel = chat.put_object(&entry, "v", ObjType::Map).unwrap();
    chat.put(&channel, "name", "general").unwrap();
    chat.commit();

    let part_id = DocumentId::derived("messages", &DocumentId::workspace_default()).part(0);
    let part_route = part_id.as_str().to_owned();
    let mut part = AutoCommit::new();
    ui::resource::init(
        &mut part,
        &part_id,
        &ResourceType::new("chat.messages"),
        "workspace",
    );
    post(&mut part, "m1", "@Bea can you look?", write_value);

    let device = Device::holding(&[(CHAT, "", &chat), (CHAT, &part_route, &part)]);
    let shown = Rc::new(Shown::default());
    let mentions = device.role(&shown);
    mentions.step();
    let opens = |id: &str| Place {
        app: CHAT.into(),
        record: String::new(),
        resource: "chat.workspace".into(),
        anchor: format!("chat.workspace/messages/{id}"),
    };
    {
        let up = shown.up.borrow();
        assert_eq!(up.len(), 1, "{up:?}");
        let notice = up.values().next().unwrap();
        assert_eq!(notice.title, "Ada mentioned you in #general");
        assert_eq!(notice.body, "@Bea can you look?");
        assert_eq!(notice.place, Some(opens("m1")));
    }

    // A second message in the same part is news of its own: its place is its message.
    post(&mut part, "m2", "@Bea and this one", write_value);
    device.writes(CHAT, &part_route, &mut part);
    look_again(&mentions);
    let up = shown.up.borrow();
    let mut places: Vec<Place> = up
        .values()
        .filter_map(|notice| notice.place.clone())
        .collect();
    places.sort();
    assert_eq!(places, vec![opens("m1"), opens("m2")], "{up:?}");
}

/// A device's records as a page reads them: what changed of a record since some heads, apart from
/// the record, and every whole read counted.
struct Counted {
    linked: Kept,
    store: Store,
    whole: std::cell::Cell<usize>,
}

impl Records for Counted {
    fn take_changed(&self) -> Vec<Changed> {
        self.linked.take_changed()
    }

    fn document(&self, app: &str, workspace: &str, record: &str) -> Option<AutoCommit> {
        self.whole.set(self.whole.get().saturating_add(1));
        self.linked.document(app, workspace, record)
    }

    fn since(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
        heads: &[automerge::ChangeHash],
    ) -> Since {
        // The host's own copy: what the page's replica answers from, never the reader's.
        let Some(mut doc) =
            crate::platform::records::document(self.store.as_ref(), app, workspace, record)
        else {
            return Since::Whole;
        };
        if heads
            .iter()
            .any(|head| doc.get_change_by_hash(head).is_none())
        {
            return Since::Whole;
        }
        let now = doc.get_heads();
        if now == heads {
            return Since::Unchanged;
        }
        let bytes = doc
            .get_changes(heads)
            .iter()
            .flat_map(|change| change.raw_bytes().to_vec())
            .collect();
        Since::Changes { heads: now, bytes }
    }

    fn heads(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
    ) -> Option<Vec<automerge::ChangeHash>> {
        crate::platform::records::document(self.store.as_ref(), app, workspace, record)
            .map(|mut doc| doc.get_heads())
    }
}

/// A tracker of `issues` issues, each with a thread of comments as text, as the workspace held it
/// before this device followed it; issue 7's thread mentions Bea.
fn tracker(issues: usize) -> AutoCommit {
    let mut doc = AutoCommit::new();
    ui::resource::init(
        &mut doc,
        &DocumentId::parse(TRACKER).unwrap(),
        &ResourceType::new("tasks.tracker"),
        "workspace",
    );
    let all = doc.put_object(ROOT, "issues", ObjType::Map).unwrap();
    for at in 0..issues {
        let entry = doc
            .put_object(&all, format!("i{at}"), ObjType::Map)
            .unwrap();
        doc.put(&entry, "k", format!("{at:06}")).unwrap();
        let issue = doc.put_object(&entry, "v", ObjType::Map).unwrap();
        doc.put(&issue, "title", format!("Issue {at}")).unwrap();
        let thread = doc.put_object(&issue, "thread", ObjType::Text).unwrap();
        doc.splice_text(&thread, 0, 0, "@Bea first look\n").unwrap();
        if at == 7 {
            doc.mark(
                &thread,
                Mark::new(MARK.into(), member_value(BEA), 0, 4),
                ExpandMark::None,
            )
            .unwrap();
        }
    }
    doc.commit();
    doc
}

const TRACKER: &str = "tab:00000000000000b2";
const TASKS: &str = "so.tilo.tasks";

/// The thread of issue `at` in `doc`.
fn thread(doc: &AutoCommit, at: usize) -> ObjId {
    let (_, all) = doc.get(ROOT, "issues").unwrap().unwrap();
    let (_, entry) = doc.get(&all, format!("i{at}")).unwrap().unwrap();
    let (_, issue) = doc.get(&entry, "v").unwrap().unwrap();
    doc.get(&issue, "thread").unwrap().unwrap().1
}

/// A comment added to issue `at`'s thread, mentioning `person` when given, written as Tasks
/// writes it and noted as it lands.
fn comment(device: &Device, doc: &mut AutoCommit, at: usize, person: Option<&str>) {
    let thread = thread(doc, at);
    let end = doc.text(&thread).unwrap().chars().count();
    doc.splice_text(&thread, end, 0, "@Bea see this\n").unwrap();
    if let Some(person) = person {
        doc.mark(
            &thread,
            Mark::new(
                MARK.into(),
                member_value(person),
                end,
                end.saturating_add(4),
            ),
            ExpandMark::None,
        )
        .unwrap();
    }
    doc.commit();
    device.writes(TASKS, TRACKER, doc);
}

/// A record the size of a company's tracker is followed for the person's mentions at the cost of
/// what changes in it: what the workspace held before is taken as seen without reading it, a
/// change that names nobody here reads nothing of the record, and only the change that mentions
/// the person reads it whole, telling that mention and none from before.
#[test]
fn a_large_record_is_read_whole_only_for_the_change_that_mentions_the_person() {
    let issues = 2_500;
    let device = Device::empty();
    let mut doc = tracker(issues);
    device.already_held(TASKS, TRACKER, &mut doc);
    let shown = Rc::new(Shown::default());
    let records = Rc::new(Counted {
        linked: Kept::over(device.store.clone(), device.changes.clone()),
        store: device.store.clone(),
        whole: std::cell::Cell::new(0),
    });
    let mentions = Mentions::over(Given {
        person: vec![BEA.to_owned()],
        store: Some(device.store.clone()),
        records: Some(Rc::clone(&records) as Rc<dyn Records>),
        notices: Some(Rc::clone(&shown) as Rc<dyn Notices>),
        name: Rc::new(name),
        wake: None,
    });
    // Each look a second after the one before, past the pause the role keeps between looks.
    let clock = std::cell::Cell::new(ui::time::now());
    let look = |mentions: &Mentions| {
        clock.set(
            clock
                .get()
                .checked_add(ui::time::Duration::from_secs(1))
                .unwrap(),
        );
        mentions.step_at(clock.get());
    };
    look(&mentions);
    assert_eq!(
        records.whole.get(),
        0,
        "what the workspace held is not read"
    );
    assert!(shown.up.borrow().is_empty(), "and none of it is news");

    // A busy afternoon in the tracker, mentioning everyone but Bea.
    let began = std::time::Instant::now();
    for at in 0..50 {
        comment(&device, &mut doc, at * 37 % issues, Some(CAI));
        look(&mentions);
    }
    assert_eq!(
        records.whole.get(),
        0,
        "a change that names nobody here reads nothing of the record"
    );
    assert!(shown.up.borrow().is_empty());
    eprintln!(
        "50 changes of a {issues}-issue tracker followed in {:?}",
        began.elapsed()
    );

    // Bea's own window registers itself and signs what it writes with her id: no mention of her.
    let (_, all) = doc.get(ROOT, "issues").unwrap().unwrap();
    let (_, entry) = doc.get(&all, "i3").unwrap().unwrap();
    let (_, issue) = doc.get(&entry, "v").unwrap().unwrap();
    doc.put(&issue, "author", member_value(BEA)).unwrap();
    let windows = doc.put_object(ROOT, "windows", ObjType::Map).unwrap();
    doc.put(&windows, BEA, BEA).unwrap();
    doc.commit();
    device.writes(TASKS, TRACKER, &mut doc);
    look(&mentions);
    assert_eq!(
        records.whole.get(),
        0,
        "the person's id where no mention keeps it reads nothing of the record"
    );

    // Ada mentions Bea on issue 9: that alone is news, not issue 7's mention from before.
    comment(&device, &mut doc, 9, Some(BEA));
    look(&mentions);
    assert_eq!(
        records.whole.get(),
        1,
        "the change that mentions Bea reads the record"
    );
    let up = shown.up.borrow();
    assert_eq!(up.len(), 1, "{up:?}");
    let place = up
        .values()
        .next()
        .unwrap()
        .place
        .clone()
        .expect("it opens what it names");
    assert_eq!(place.anchor, "tasks.tracker/issues/i9", "{up:?}");
    drop(up);

    // After a restart the record is followed from where it was looked at last.
    let again = Mentions::over(Given {
        person: vec![BEA.to_owned()],
        store: Some(device.store.clone()),
        records: Some(Rc::clone(&records) as Rc<dyn Records>),
        notices: Some(Rc::clone(&shown) as Rc<dyn Notices>),
        name: Rc::new(name),
        wake: None,
    });
    comment(&device, &mut doc, 11, None);
    look(&again);
    assert_eq!(
        records.whole.get(),
        1,
        "nothing is read again after a restart"
    );
}
