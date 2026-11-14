//! Several sessions of one agent member in one tab.
//!
//! Every session of one MCP client acts as the same agent member: a program
//! on the device that runs the page and one that came in from another of the
//! person's devices are both `agent:claude-code`, and the engine keeps what
//! they do under that one name. Where each works from is what tells them
//! apart, and the record says it beside the name. So the window sets an
//! action down as the invoker of the invocation that ran it, whoever else
//! was admitted under the name meanwhile.

use super::many_actors::{devices_in_history, ended, entries, reviewed_field, set_by, start};
use super::*;

/// The agent member every session of the client acts as.
const MEMBER: &str = "agent:claude-code";

/// A session of the member on the device that runs the page, by the name the
/// host that admitted it knows it by.
fn here(name: &str) -> ActionInvoker {
    agent_for_ada(MEMBER).named(Some(name.to_owned()))
}

/// A session of the same member that came in from Ada's laptop.
fn on_laptop(name: &str) -> ActionInvoker {
    here(name).from_device(Some(laptop()))
}

/// The member with the laptop beside it, as an entry names them.
fn from_laptop() -> (String, Option<String>, Option<String>) {
    (
        MEMBER.to_owned(),
        Some(laptop().id),
        Some("Studio laptop".to_owned()),
    )
}

/// The member with no device beside it: a session on the device that runs
/// the page.
fn from_here() -> (String, Option<String>, Option<String>) {
    (MEMBER.to_owned(), None, None)
}

/// The session each change an agent authored in the record's history is by:
/// the name its host knows it by.
fn sessions_in_history(record: &PersonRecord<PageRecord>) -> Vec<Option<String>> {
    record
        .record()
        .read(|doc| {
            let mut doc = doc.clone();
            history::versions(&mut doc)
                .into_iter()
                .filter_map(|version| version.context?.invoker)
                .filter(|invoker| invoker.acting.is_agent())
                .map(|invoker| invoker.name)
                .collect()
        })
        .expect("the record is open")
}

/// Which of `listed` say that where their actor worked from is not known.
fn unknown(listed: &Value) -> Vec<bool> {
    listed
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry.get("device_unknown") == Some(&json!(true)))
        .collect()
}

/// Two sessions of the member answer one form at the same moment: `first`'s
/// fill holds the tab, and `second`'s is admitted while it runs and waits
/// behind it. The engine sets both down under the member's name, the only
/// one it knows either by.
fn two_sessions_fill_at_once(
    app: &mut App,
    engine: &TestEngine,
    first: &ActionInvoker,
    second: &ActionInvoker,
) {
    engine_sets_down(engine, &field_filled(1, MEMBER, "Given names"));
    engine_sets_down(engine, &field_filled(2, MEMBER, "Passport number"));
    let holding = start(app, first, &fill("d1e2", "Maria Elena")).expect("it holds the tab");
    let waiting = start(app, second, &fill("d1e4", "P7K290XB4")).expect("it waits its turn");
    app.settle();
    ended(app, holding).expect("the fill that held the tab answers");
    ended(app, waiting).expect("the fill that waited answers");
    app.settle();
}

/// Who set each of the form's two fields, as a review of the page's fields
/// answers the person at the window.
fn who_set_the_fields(app: &mut App, engine: &TestEngine) -> Vec<Value> {
    engine.set_page_data(
        "fields",
        json!({"fields": [
            reviewed_field("Given names", &set_by(MEMBER, 1)),
            reviewed_field("Passport number", &set_by(MEMBER, 2)),
        ]}),
    );
    let fields =
        act(app, None, &BrowserEffect::ReadPageFields { frame: None }).expect("the fields answer");
    fields["data"]["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|field| field["set"].clone())
        .collect()
}

/// A session of Claude Code on Ada's laptop and one on the device that runs
/// the page answer one form at the same moment, both as the one agent
/// member. The laptop's fill holds the tab and the other is admitted while
/// it runs. Each fill is set down as the session that did it: the laptop's
/// names the laptop beside the member and the other names no device, in the
/// record, in a reviewer's read of it, in who set each field and in the
/// record's history.
#[test]
fn a_session_from_another_device_is_told_from_one_here_under_the_same_name() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    let laptop = laptop();
    two_sessions_fill_at_once(
        &mut app,
        &engine,
        &on_laptop("Claude Code"),
        &here("Claude Code #2"),
    );

    let expected = [from_laptop(), from_here()];
    let record = record(&mut app);
    let tab = only_tab(&record);
    let kept = Value::Array(entries(&record, &tab));
    assert_eq!(named_in(&kept), expected, "{kept}");
    assert_eq!(unknown(&kept), [false, false], "{kept}");
    let read = review(&mut app, None);
    assert_eq!(named_in(&read["data"]["entries"]), expected, "{read}");
    assert_eq!(
        who_set_the_fields(&mut app, &engine),
        [
            json!({"how": "action", "by": MEMBER, "seq": 1,
                "device": laptop.id, "device_name": "Studio laptop"}),
            json!({"how": "action", "by": MEMBER, "seq": 2}),
        ]
    );
    assert_eq!(
        devices_in_history(&record),
        [Some(laptop), None],
        "each change is authored by the invoker its invocation was admitted for"
    );
    assert_eq!(
        sessions_in_history(&record),
        [
            Some("Claude Code".to_owned()),
            Some("Claude Code #2".to_owned())
        ]
    );
}

/// The other way round: the fill of the session on this device holds the
/// tab, and the laptop's is admitted while it runs. What the session here
/// did names no device, though a session on the laptop was admitted under
/// its name after it; what the laptop's did names the laptop.
#[test]
fn a_session_here_is_not_given_the_device_of_one_admitted_after_it() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    let laptop = laptop();
    two_sessions_fill_at_once(
        &mut app,
        &engine,
        &here("Claude Code"),
        &on_laptop("Claude Code #2"),
    );

    let expected = [from_here(), from_laptop()];
    let record = record(&mut app);
    let tab = only_tab(&record);
    let kept = Value::Array(entries(&record, &tab));
    assert_eq!(named_in(&kept), expected, "{kept}");
    assert_eq!(unknown(&kept), [false, false], "{kept}");
    let read = review(&mut app, None);
    assert_eq!(named_in(&read["data"]["entries"]), expected, "{read}");
    assert_eq!(
        who_set_the_fields(&mut app, &engine),
        [
            json!({"how": "action", "by": MEMBER, "seq": 1}),
            json!({"how": "action", "by": MEMBER, "seq": 2,
                "device": laptop.id, "device_name": "Studio laptop"}),
        ]
    );
    assert_eq!(devices_in_history(&record), [None, Some(laptop)]);
}

/// Two programs of one login are two sessions of the member on one device:
/// neither names a device, and each fill is still the change of the session
/// that did it, by the name its host knows it by.
#[test]
fn two_sessions_on_one_device_each_author_what_they_did() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    two_sessions_fill_at_once(
        &mut app,
        &engine,
        &here("Claude Code"),
        &here("Claude Code #2"),
    );

    let record = record(&mut app);
    let tab = only_tab(&record);
    let kept = Value::Array(entries(&record, &tab));
    assert_eq!(named_in(&kept), [from_here(), from_here()], "{kept}");
    assert_eq!(unknown(&kept), [false, false], "{kept}");
    assert_eq!(
        sessions_in_history(&record),
        [
            Some("Claude Code".to_owned()),
            Some("Claude Code #2".to_owned())
        ]
    );
}

/// The engine answers the fill of the session on the laptop and says where
/// it set it down. Before the window has read that entry a session of the
/// member on this device asks who set the field, and then reads the tab's
/// record; the engine lists the fill and, after it, a request the window
/// held under the member's name, which no answer placed.
///
/// The fill is the laptop session's, by the answer that placed it: who set
/// the field names the laptop before the entry is written, and so does the
/// entry. The held request may be either session's, and they work from
/// different places: it says that where its actor worked from is not known,
/// and names no device.
#[test]
fn what_no_answer_places_under_a_name_two_places_share_says_its_device_is_not_known() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    let laptop = laptop();
    let far = on_laptop("Claude Code");
    let near = here("Claude Code #2");
    engine.push_page_reply(Ok(PageReply {
        record: PageRecordNote {
            seq: Some(1),
            ..PageRecordNote::default()
        },
        ..answer("Filled textbox \"Given names\"")
    }));
    act(&mut app, Some(&far), &fill("d1e2", "Maria Elena")).expect("the fill answers");
    app.settle();
    let record = record(&mut app);
    assert!(
        record.with(|state| state.tabs.keys().next().is_none()),
        "the fill is not in the tab's record yet"
    );

    engine.set_page_data(
        "fields",
        json!({"fields": [reviewed_field("Given names", &set_by(MEMBER, 1))]}),
    );
    let fields = act(
        &mut app,
        Some(&near),
        &BrowserEffect::ReadPageFields { frame: None },
    )
    .expect("the fields answer");
    assert_eq!(
        fields["data"]["fields"][0]["set"],
        json!({"how": "action", "by": MEMBER, "seq": 1,
            "device": laptop.id, "device_name": "Studio laptop"}),
        "{fields}"
    );

    let held = json!({"seq": 2, "time_ms": 1_700_000_000_002_u64, "actor": MEMBER,
        "how": "hold", "action": "request", "ok": true,
        "message": "POST https://orders.test", "url": PAGE,
        "to_url": "https://orders.test"});
    engine.set_page_data(
        "ledger",
        json!({"entries": [field_filled(1, MEMBER, "Given names"), held],
            "next": 2, "more": false, "dropped": 0, "waiting": 0}),
    );
    let read = act(
        &mut app,
        Some(&near),
        &BrowserEffect::ReadPageRecord { since: None },
    )
    .expect("the record answers");
    let listed = &read["data"]["entries"];
    assert_eq!(named_in(listed), [from_laptop(), from_here()], "{read}");
    assert_eq!(unknown(listed), [false, true], "{read}");
    assert_eq!(devices_in_history(&record), [Some(laptop), None]);
}
