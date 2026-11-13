#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
//! A conversation's record, opened beside the window's own: one record of
//! the two people it is between in the workspace, written by either person's
//! devices, each person's read mark theirs, merged without an owner in
//! between.

use tilo_app::prelude::*;

/// What the two people said, and how far each of them read.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Talk {
    pub said: Collection<Said>,
    /// Each person's read mark, by their participant id.
    pub read: std::collections::BTreeMap<String, i64>,
}

/// One thing one of them said.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Said {
    pub author: String,
    pub text: String,
}

#[resource(
    kind = "test.direct",
    app = "so.tilo.test-direct",
    catalogue = "tests/locales"
)]
impl Talk {
    /// Say something.
    #[action]
    pub fn say(&mut self, cx: &mut ActionCx, text: String) {
        self.said.push(
            cx.id(),
            Said {
                author: cx.author().unwrap_or_default().to_owned(),
                text,
            },
        );
    }

    /// Mark how far the person has read.
    #[action(housekeeping)]
    pub fn mark_read(&mut self, cx: &mut ActionCx, seen: i64) {
        self.read
            .insert(cx.author().unwrap_or_default().to_owned(), seen);
    }
}

struct TalkApp;

impl ResourceApp for TalkApp {
    type Resource = Talk;
    const ID: &'static str = "so.tilo.test-direct";
    const TITLE: &'static str = "Direct conversation";

    fn view(_cx: &mut ViewCx<Talk>) -> Element {
        Box::new(Text::new("a conversation between two people"))
    }
}

/// The app that keeps the conversations' records.
const KEEPER: &str = "so.tilo.test-direct";
/// The workspace the conversations' records are in.
const WORKSPACE: &str = "00000000-0000-4000-8000-00000000000b";
/// One conversation's record, as the workspace names it.
const WITH_BOB: &str =
    "direct/direct_0000000000000000000000000000000000000000000000000000000000000000";
/// Another conversation's record.
const WITH_BO: &str =
    "direct/direct_1111111111111111111111111111111111111111111111111111111111111111";

fn ada() -> String {
    "ad".repeat(32)
}

fn bob() -> String {
    "b0".repeat(32)
}

fn said(record: &WorkspaceRecord<Talk>) -> Vec<String> {
    let mut texts: Vec<String> = record
        .get()
        .said
        .values()
        .map(|said| said.text.clone())
        .collect();
    texts.sort();
    texts
}

/// Two devices of two people, all in the one workspace.
fn devices() -> (TestApp<TalkApp>, TestApp<TalkApp>, TestApp<TalkApp>) {
    (
        TestApp::<TalkApp>::for_person_in(ada().as_str(), WORKSPACE).expect("Ada's laptop"),
        TestApp::<TalkApp>::for_person_in(ada().as_str(), WORKSPACE).expect("Ada's phone"),
        TestApp::<TalkApp>::for_person_in(bob().as_str(), WORKSPACE).expect("Bob's phone"),
    )
}

#[test]
fn the_conversations_record_is_one_record_on_all_their_devices() {
    let (mut laptop, mut phone, _) = devices();
    let on_laptop = laptop
        .shared_record::<Talk>(KEEPER, WITH_BOB)
        .expect("Ada's laptop opens the conversation's record");
    let on_phone = phone
        .shared_record::<Talk>(KEEPER, WITH_BOB)
        .expect("Ada's phone opens the conversation's record");
    assert_eq!(
        on_laptop.id(),
        on_phone.id(),
        "one record per conversation, wherever it is opened"
    );
    let route = on_laptop.record().route().expect("a route");
    assert_eq!(route.app_id, TalkApp::ID);
    assert_eq!(
        route.cluster_id.to_string(),
        WORKSPACE,
        "the record lives in the workspace the conversation is in"
    );
    assert_eq!(
        route.record.as_deref(),
        Some(WITH_BOB),
        "the conversation's record sits beside the workspace's own"
    );
}

#[test]
fn either_person_writes_and_the_other_reads_it_after_they_meet() {
    let (mut laptop, _, mut bobs) = devices();
    let on_laptop = laptop
        .shared_record::<Talk>(KEEPER, WITH_BOB)
        .expect("Ada's laptop opens the conversation's record");
    let on_bobs = bobs
        .shared_record::<Talk>(KEEPER, WITH_BOB)
        .expect("Bob's phone opens the conversation's record");
    on_laptop.invoke(&Talk::SAY.with("hi from Ada")).unwrap();
    on_bobs.invoke(&Talk::SAY.with("hi from Bob")).unwrap();
    let her = on_laptop.record().document().expect("Ada's record");
    on_bobs.record().merge_peer_document(her).unwrap();
    let his = on_bobs.record().document().expect("Bob's record");
    on_laptop.record().merge_peer_document(his).unwrap();
    assert_eq!(
        said(&on_laptop),
        ["hi from Ada", "hi from Bob"],
        "both people's words are in the one conversation"
    );
    assert_eq!(said(&on_laptop), said(&on_bobs));
    assert_eq!(
        on_laptop.record().document().unwrap(),
        on_bobs.record().document().unwrap(),
        "each device holds the same document"
    );
}

#[test]
fn each_persons_read_mark_is_theirs_and_their_devices_agree() {
    let (mut laptop, mut phone, mut bobs) = devices();
    let on_laptop = laptop
        .shared_record::<Talk>(KEEPER, WITH_BOB)
        .expect("Ada's laptop opens the conversation's record");
    let on_phone = phone
        .shared_record::<Talk>(KEEPER, WITH_BOB)
        .expect("Ada's phone opens the conversation's record");
    let on_bobs = bobs
        .shared_record::<Talk>(KEEPER, WITH_BOB)
        .expect("Bob's phone opens the conversation's record");
    on_bobs.invoke(&Talk::SAY.with("hello")).unwrap();
    on_laptop.invoke(&Talk::MARK_READ.with(1_000_i64)).unwrap();
    let hers = on_laptop.record().document().expect("Ada's record");
    on_phone.record().merge_peer_document(hers.clone()).unwrap();
    on_bobs.record().merge_peer_document(hers).unwrap();
    assert_eq!(
        on_phone.get().read.get(ada().as_str()).copied(),
        Some(1_000),
        "Ada's other device shows what she read"
    );
    assert_eq!(
        on_bobs.get().read.get(ada().as_str()).copied(),
        Some(1_000),
        "the other person reads her mark, not a mark of their own"
    );
    assert!(
        !on_bobs.get().read.contains_key(bob().as_str()),
        "Bob has read nothing yet"
    );
}

#[test]
fn another_conversation_is_another_record() {
    let (mut laptop, _, _) = devices();
    let with_bob = laptop
        .shared_record::<Talk>(KEEPER, WITH_BOB)
        .expect("Ada opens the conversation's record");
    let with_bo = laptop
        .shared_record::<Talk>(KEEPER, WITH_BO)
        .expect("Ada opens another conversation's record");
    assert_ne!(with_bob.id(), with_bo.id());
    with_bob.invoke(&Talk::SAY.with("only Bob")).unwrap();
    assert!(
        with_bo.get().said.is_empty(),
        "one conversation's words are not in another's record"
    );
}
