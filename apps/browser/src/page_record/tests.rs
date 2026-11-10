//! The tab's record as a resource, through Browser's own window: agents the
//! action door admitted act on the page, the engine sets down what they did,
//! and the record is read where Browser keeps it — by a reviewer through the
//! page's own action, by History, by another device, and after the engine
//! that set it down is gone. Only the window adds to it: nobody at the door
//! does.
//!
//! The engine is the deterministic one. It answers what a test queues: an
//! operating action's answer with its place in the engine's record, then the
//! engine's listing of that action when the window asks for it.

use std::sync::Arc;

use serde_json::{Value, json};
use ui::framework::testing::TestApp;
use ui::tilo_actions::{
    ActionAddress, ActionInvoker, ActorKind, InteractionActor, InvocationUpdate,
};
use ui::tilo_resource::history;
use ui::tilo_resource::{Effect as _, Invoke};
use ui::webview::test_engine::TestEngine;
use ui::webview::{PageNavigation, WebViewCommand, WebViewEngine as _};

use super::*;
use crate::effects::BrowserEffect;
use crate::resource::BrowserShell;

type App = TestApp<BrowserShell>;

/// The workspace Browser is open in: one with an id, so its records have a
/// route.
const WORKSPACE: &str = "00000000-0000-4000-8000-0000000000b7";
const PAGE: &str = "https://shop.test/form";
/// The record as the window publishes it.
const RESOURCE: &str = "person:browser.page-record";

/// Ada, by her root public key.
fn ada() -> String {
    "ad".repeat(32)
}

/// Ada's Browser on one of her devices, showing the page, with `engine` as
/// the engine of its tab.
fn open(engine: &Arc<TestEngine>) -> App {
    crate::browser_widget::test_window_engine::install(engine.clone());
    let mut app = App::for_person_in(&ada(), WORKSPACE).expect("Browser opens");
    app.settle();
    engine.send(WebViewCommand::Navigate(PAGE.into()));
    app.settle();
    app
}

/// An agent the action door admitted, by its principal.
fn agent(principal: &str) -> ActionInvoker {
    ActionInvoker::admitted(&InteractionActor::new(principal, ActorKind::Agent))
}

/// An agent the action door admitted to act for Ada, by its principal: what
/// the door of a record of Ada's own takes actions from, beside Ada.
fn agent_for_ada(principal: &str) -> ActionInvoker {
    ActionInvoker::admitted(&InteractionActor::new(principal, ActorKind::Agent).acting_for(ada()))
}

/// What a call through the door comes to when nothing answers it in time.
const NO_ANSWER: &str = "it did not answer";

/// Invoke the action at `address` with `arguments` through the window's
/// door for `invoker`, as the host does for an agent it admitted (nobody:
/// the person at the window), and wait for what it answers with.
fn invoke(
    app: &mut App,
    invoker: Option<&ActionInvoker>,
    address: ActionAddress,
    arguments: &serde_json::Map<String, Value>,
) -> Result<Option<Value>, String> {
    let instance = app
        .harness()
        .component_instance_id()
        .ok_or("Browser has no component")?;
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: instance,
        address,
    };
    let (invocation, started) = app
        .harness()
        .start_bound_job_for(invoker.cloned(), &address, arguments, true)
        .map_err(|error| error.to_string())?;
    if let Some(output) = started.completed() {
        return Ok(output.value);
    }
    app.harness()
        .wait_until(ui::time::Duration::from_secs(30), |harness| {
            harness
                .job_updates(invocation)
                .iter()
                .any(InvocationUpdate::is_terminal)
        });
    for update in app.harness().job_updates(invocation) {
        match update {
            InvocationUpdate::Succeeded { output, .. } => return Ok(output.value.clone()),
            InvocationUpdate::Failed { error, .. } => {
                return Err(format!("{}: {}", error.code, error.message));
            }
            _ => {}
        }
    }
    Err(NO_ANSWER.to_owned())
}

/// Run the page action `effect` for `invoker` as the host does for an agent
/// it admitted (nobody: the person at the window), and wait for its answer.
fn act(
    app: &mut App,
    invoker: Option<&ActionInvoker>,
    effect: &BrowserEffect,
) -> Result<Value, String> {
    invoke(
        app,
        invoker,
        effect.address_on("browser.page"),
        &effect.arguments(),
    )?
    .ok_or_else(|| "it answered nothing".to_owned())
}

/// An engine answer that says `message` and nothing else.
fn answer(message: &str) -> PageReply {
    PageReply {
        url: PAGE.into(),
        message: message.into(),
        outline: None,
        data: None,
        image: None,
        navigation: PageNavigation::None,
        loading: false,
        record: PageRecordNote::default(),
    }
}

/// What the engine answers a read of its record with when it lists
/// `entries`.
fn listing(entries: &[Value], live: &Value) -> PageReply {
    let mut data = json!({
        "entries": entries,
        "next": entries.last().map_or(0, |entry| entry["seq"].as_u64().unwrap_or(0)),
        "more": false, "dropped": 0, "waiting": 0,
    });
    if let (Some(data), Some(live)) = (data.as_object_mut(), live.as_object()) {
        data.extend(live.clone());
    }
    PageReply {
        data: Some(data),
        ..answer("listed")
    }
}

/// The engine ends the next operating action as `entry`: the action's answer
/// carries its place in the engine's record, and the engine lists the entry
/// when it is asked what it set down.
fn engine_sets_down(engine: &TestEngine, entry: &Value) {
    engine.push_page_reply(Ok(PageReply {
        record: PageRecordNote {
            seq: entry["seq"].as_u64(),
            ..PageRecordNote::default()
        },
        ..answer(entry["message"].as_str().unwrap_or("Done"))
    }));
    engine.push_page_reply(Ok(listing(std::slice::from_ref(entry), &json!({}))));
}

fn fill(element: &str, text: &str) -> BrowserEffect {
    BrowserEffect::FillPage {
        element: element.into(),
        text: text.into(),
        frame: None,
    }
}

fn click(element: &str) -> BrowserEffect {
    BrowserEffect::ClickPage {
        element: element.into(),
        frame: None,
    }
}

/// Ada's agent fills the email, as the engine sets it down at `seq`.
fn email_filled(seq: u64) -> Value {
    json!({"seq": seq, "time_ms": 1_700_000_000_000_u64 + seq, "actor": "agent:ada",
        "action": "fill", "ok": true, "message": "Filled textbox \"Email\"", "url": PAGE,
        "navigation": "possible", "target": {"role": "textbox", "name": "Email"},
        "text": "ada@example.org"})
}

/// Ben's agent chooses a country, as the engine sets it down at `seq`.
fn country_chosen(seq: u64) -> Value {
    json!({"seq": seq, "time_ms": 1_700_000_000_000_u64 + seq, "actor": "agent:ben",
        "action": "select", "ok": true, "message": "Chose Belgium", "url": PAGE,
        "target": {"role": "combobox", "name": "Country", "nth": 2}, "values": ["Belgium"]})
}

/// Ada's agent sends the form, which leaves the page, as the engine sets it
/// down at `seq`.
fn form_sent(seq: u64) -> Value {
    json!({"seq": seq, "time_ms": 1_700_000_000_000_u64 + seq, "actor": "agent:ada",
        "action": "click", "ok": true, "message": "Clicked button \"Send\"", "url": PAGE,
        "to_url": "https://shop.test/sent", "navigation": "started",
        "target": {"role": "button", "name": "Send"}})
}

/// Ada's and Ben's agents fill, choose and send, each action set down by
/// the engine as it ends.
fn two_agents_answer_the_form(app: &mut App, engine: &TestEngine) {
    let (ada, ben) = (agent("agent:ada"), agent("agent:ben"));
    engine_sets_down(engine, &email_filled(1));
    act(app, Some(&ada), &fill("d1e2", "ada@example.org")).expect("the fill answers");
    engine_sets_down(engine, &country_chosen(2));
    let chosen = BrowserEffect::SelectPage {
        element: "d1e4".into(),
        values: vec!["Belgium".into()],
        frame: None,
    };
    act(app, Some(&ben), &chosen).expect("the choice answers");
    engine_sets_down(engine, &form_sent(3));
    act(app, Some(&ada), &click("d1e9")).expect("the click answers");
    app.settle();
}

/// Browser's record of page actions, as its window holds it.
fn record(app: &mut App) -> PersonRecord<PageRecord> {
    app.person::<PageRecord>()
        .expect("Browser keeps a record of page actions")
}

/// The one tab the record holds.
fn only_tab(record: &PersonRecord<PageRecord>) -> String {
    let tabs: Vec<String> = record.with(|state| state.tabs.keys().cloned().collect());
    assert_eq!(tabs.len(), 1, "one tab acted: {tabs:?}");
    tabs.into_iter().next().unwrap_or_default()
}

/// What `tab`'s record holds: each action's place, actor and action.
fn held(record: &PersonRecord<PageRecord>, tab: &str) -> Vec<(u64, String, String)> {
    record.with(|state| {
        state
            .page(tab, 0)
            .entries
            .iter()
            .map(|entry| {
                (
                    entry["seq"].as_u64().unwrap_or(0),
                    entry["actor"].as_str().unwrap_or_default().to_owned(),
                    entry["action"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect()
    })
}

/// One step of the record's history: who made the change, how many actions
/// of `tab` the record held after it, and the newest of them.
fn steps(record: &PersonRecord<PageRecord>, tab: &str) -> Vec<(String, usize, String)> {
    record
        .record()
        .read(|doc| {
            let mut doc = doc.clone();
            history::versions(&mut doc)
                .into_iter()
                .filter_map(|version| {
                    let author = version.context.as_ref()?.invoker.as_ref()?.actor.clone();
                    let then = history::read_at::<PageRecord>(&mut doc, &[version.hash])
                        .expect("a past version of the record reads");
                    let page = then.page(tab, 0);
                    let newest = page
                        .entries
                        .last()
                        .and_then(|entry| entry["message"].as_str())
                        .unwrap_or_default()
                        .to_owned();
                    Some((author, page.entries.len(), newest))
                })
                .collect()
        })
        .expect("the record is open")
}

/// Read the tab's record through the page's own action, as a reviewer does.
fn review(app: &mut App, since: Option<u64>) -> Value {
    let reviewer = agent("agent:rev");
    act(
        app,
        Some(&reviewer),
        &BrowserEffect::ReadPageRecord { since },
    )
    .expect("the record answers")
}

fn published(resources: &[ui::tilo_actions::ResourceSpec], id: &str) -> bool {
    resources
        .iter()
        .any(|resource| resource.id.as_str() == id || published(&resource.children, id))
}

/// Two named agents act in one tab: each action is a change of Browser's
/// record as it ends, in the order they ran, under the name the engine keeps
/// the actor by and authored by the invoker the action door admitted. What
/// was typed straight into the page, and what the person at the window did,
/// is the person's.
#[test]
fn what_two_agents_did_is_the_record_in_order_under_their_names() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    two_agents_answer_the_form(&mut app, &engine);
    let record = record(&mut app);
    let tab = only_tab(&record);
    assert_eq!(
        held(&record, &tab),
        [
            (1, "agent:ada".to_owned(), "fill".to_owned()),
            (2, "agent:ben".to_owned(), "select".to_owned()),
            (3, "agent:ada".to_owned(), "click".to_owned()),
        ]
    );
    // What the engine set down of each is kept as it listed it.
    let kept = record.with(|state| state.page(&tab, 0).entries);
    assert_eq!(kept, [email_filled(1), country_chosen(2), form_sent(3)]);
    // Each is one change of the record, by the agent that acted.
    assert_eq!(
        steps(&record, &tab),
        [
            (
                "agent:ada".to_owned(),
                1,
                "Filled textbox \"Email\"".to_owned()
            ),
            ("agent:ben".to_owned(), 2, "Chose Belgium".to_owned()),
            (
                "agent:ada".to_owned(),
                3,
                "Clicked button \"Send\"".to_owned()
            ),
        ]
    );

    // The person at the window acts under no name, and what they typed
    // straight into the page the engine tells apart as input: both are set
    // down as theirs. The engine lists the typing with the click after it.
    let typed = json!({"seq": 4, "time_ms": 1_700_000_000_004_u64, "actor": "", "how": "input",
        "action": "input", "ok": true, "url": PAGE,
        "target": {"role": "textbox", "name": "Passport number"}, "text": "P1234567"});
    let own = json!({"seq": 5, "time_ms": 1_700_000_000_005_u64, "actor": "", "action": "click",
        "ok": true, "message": "Clicked button \"Back\"", "url": PAGE,
        "target": {"role": "button", "name": "Back"}});
    engine.push_page_reply(Ok(PageReply {
        record: PageRecordNote {
            seq: Some(5),
            ..PageRecordNote::default()
        },
        ..answer("Clicked button \"Back\"")
    }));
    engine.push_page_reply(Ok(listing(&[typed, own], &json!({}))));
    act(&mut app, None, &click("d1e1")).expect("the person's click answers");
    app.settle();
    assert_eq!(
        held(&record, &tab)[3..],
        [
            (4, String::new(), "input".to_owned()),
            (5, String::new(), "click".to_owned()),
        ]
    );
    let authors: Vec<String> = steps(&record, &tab)
        .into_iter()
        .map(|(author, ..)| author)
        .collect();
    let person = ada();
    assert_eq!(
        authors,
        [
            "agent:ada",
            "agent:ben",
            "agent:ada",
            person.as_str(),
            person.as_str()
        ],
        "the typing and the person's own click are the person's changes"
    );

    // The record is a resource of the window like Browser's others.
    assert!(
        published(&app.harness().resources(), RESOURCE),
        "{RESOURCE} is published"
    );
}

/// A reviewer reads the record through the page's action: answered from the
/// resource, read on by place, with what only the engine knows at that
/// moment added from the engine.
#[test]
fn the_record_is_read_on_by_place_with_what_the_engine_knows_now() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    two_agents_answer_the_form(&mut app, &engine);

    // The engine lists nothing new, and says what runs and what waits.
    engine.push_page_reply(Ok(listing(
        &[],
        &json!({"waiting": 2, "running": {"seq": 4, "actor": "agent:ben", "action": "click"}}),
    )));
    let all = review(&mut app, None);
    assert_eq!(all["message"], "3 actions");
    assert_eq!(
        all["data"]["entries"],
        json!([email_filled(1), country_chosen(2), form_sent(3)])
    );
    assert_eq!(all["data"]["next"], 3);
    assert_eq!(all["data"]["more"], false);
    assert_eq!(all["data"]["dropped"], 0);
    assert_eq!(all["data"]["waiting"], 2);
    assert_eq!(
        all["data"]["running"],
        json!({"seq": 4, "actor": "agent:ben", "action": "click"})
    );
    assert_eq!(all["provenance"], "web-content");

    // Read on from a place: only what came after it.
    let later = review(&mut app, Some(2));
    assert_eq!(later["message"], "1 action");
    assert_eq!(later["data"]["entries"], json!([form_sent(3)]));
    assert_eq!(later["data"]["next"], 3);
    assert!(later["data"].get("running").is_none(), "nothing runs now");
    let none = review(&mut app, Some(3));
    assert_eq!(none["data"]["entries"], json!([]));
    assert_eq!(none["data"]["next"], 3);

    // An action the engine set down that no answer told the window of (its
    // caller had given up) is written when the record is next read.
    let late = json!({"seq": 4, "time_ms": 1_700_000_000_004_u64, "actor": "agent:ben",
        "action": "press", "ok": false, "code": "timeout", "message": "The page did not answer",
        "url": "https://shop.test/sent"});
    engine.push_page_reply(Ok(listing(std::slice::from_ref(&late), &json!({}))));
    let caught_up = review(&mut app, Some(3));
    assert_eq!(caught_up["data"]["entries"], json!([late]));
    assert_eq!(caught_up["data"]["next"], 4);
    let record = record(&mut app);
    let tab = only_tab(&record);
    assert_eq!(held(&record, &tab).len(), 4);
}

/// The engine is gone and a new one runs the tab: the record is still read,
/// from the resource, and goes on from the place it had reached although
/// the new engine counts its own record from the start.
#[test]
fn the_record_is_still_read_after_the_engine_started_again() {
    let first = TestEngine::new();
    let mut app = open(&first);
    two_agents_answer_the_form(&mut app, &first);
    let tab = only_tab(&record(&mut app));

    // The window closes and opens again: its page runs in a new engine,
    // whose own record is empty.
    let second = TestEngine::new();
    crate::browser_widget::test_window_engine::install(second.clone());
    let mut app = app.reopen().expect("Browser opens again");
    app.settle();
    let read = review(&mut app, None);
    assert_eq!(
        read["data"]["entries"],
        json!([email_filled(1), country_chosen(2), form_sent(3)]),
        "what the first engine set down is read after it is gone"
    );
    assert_eq!(read["data"]["next"], 3);

    // The new engine sets its first action down at its own place 1. In the
    // tab's record it stands after the three before it, and that is the
    // place its answer names.
    let ben = agent("agent:ben");
    let again = json!({"seq": 1, "time_ms": 1_700_000_000_100_u64, "actor": "agent:ben",
        "action": "click", "ok": true, "message": "Clicked link \"Orders\"",
        "url": "https://shop.test/sent", "target": {"role": "link", "name": "Orders"}});
    engine_sets_down(&second, &again);
    let clicked = act(&mut app, Some(&ben), &click("d2e3")).expect("the click answers");
    assert_eq!(clicked["seq"], 4, "its place in the tab's record");
    app.settle();
    let record = record(&mut app);
    assert_eq!(
        only_tab(&record),
        tab,
        "the reopened window is the same tab"
    );
    assert_eq!(
        held(&record, &tab),
        [
            (1, "agent:ada".to_owned(), "fill".to_owned()),
            (2, "agent:ben".to_owned(), "select".to_owned()),
            (3, "agent:ada".to_owned(), "click".to_owned()),
            (4, "agent:ben".to_owned(), "click".to_owned()),
        ]
    );
    let later = review(&mut app, Some(3));
    assert_eq!(later["data"]["entries"][0]["seq"], 4);
    assert_eq!(
        later["data"]["entries"][0]["message"],
        "Clicked link \"Orders\""
    );
    assert_eq!(later["data"]["next"], 4);

    // An engine that does not answer leaves the record as readable as it
    // was, and the answer says what it could not tell.
    second.push_page_reply(Err(PageFailure::unavailable()));
    let without = review(&mut app, None);
    assert_eq!(without["data"]["entries"].as_array().map(Vec::len), Some(4));
    assert_eq!(without["data"]["waiting"], 0);
    assert!(
        without["message"].as_str().is_some_and(
            |message| message.starts_with("4 actions; the page's engine did not answer")
        ),
        "{without}"
    );
}

/// History over the record steps through the tab action by action: each
/// version of the record holds one action more, and names who did it.
#[test]
fn history_steps_through_the_record_one_action_at_a_time() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    two_agents_answer_the_form(&mut app, &engine);
    let record = record(&mut app);
    let tab = only_tab(&record);
    let seen = steps(&record, &tab);
    assert_eq!(
        seen.iter().map(|(_, held, _)| *held).collect::<Vec<_>>(),
        [1, 2, 3],
        "each step adds one action and takes none away"
    );
    assert_eq!(
        seen.iter()
            .map(|(_, _, newest)| newest.as_str())
            .collect::<Vec<_>>(),
        [
            "Filled textbox \"Email\"",
            "Chose Belgium",
            "Clicked button \"Send\""
        ]
    );
    // A step is the action's own change: the history entry names the action
    // of the record that made it and who invoked it.
    let entries: Vec<ui::tilo_actions::ActionContext> = record
        .record()
        .read(|doc| {
            history::versions(&mut doc.clone())
                .into_iter()
                .filter_map(|version| version.context)
                .filter(|context| context.invoker.is_some())
                .collect()
        })
        .expect("the record is open");
    assert_eq!(entries.len(), 3);
    for entry in &entries {
        assert_eq!(
            entry.invoker.as_ref().map(|invoker| invoker.kind),
            Some(ActorKind::Agent),
            "{entry:?}"
        );
    }
}

/// Another device of the person's that holds the workspace reads the same
/// record: same route, and after the workspace's sync the same actions by
/// the same authors.
#[test]
fn another_device_of_the_workspace_reads_the_same_record() {
    let engine = TestEngine::new();
    let mut laptop = open(&engine);
    two_agents_answer_the_form(&mut laptop, &engine);
    let mut phone = open(&TestEngine::new());
    let (on_laptop, on_phone) = (record(&mut laptop), record(&mut phone));
    assert!(on_laptop.syncs(), "the record has a route in the workspace");
    let route = on_laptop.record().route().expect("a route");
    assert_eq!(Some(&route), on_phone.record().route().as_ref());
    assert_eq!(route.cluster_id.to_string(), WORKSPACE);

    let tab = only_tab(&on_laptop);
    assert!(held(&on_phone, &tab).is_empty(), "not synced yet");
    let sent = on_laptop.record().document().expect("the record's bytes");
    on_phone
        .record()
        .merge_peer_document(sent)
        .expect("the phone takes the record in");
    assert_eq!(held(&on_phone, &tab), held(&on_laptop, &tab));
    assert_eq!(
        on_phone.with(|state| state.page(&tab, 0)),
        on_laptop.with(|state| state.page(&tab, 0))
    );
    assert_eq!(steps(&on_phone, &tab), steps(&on_laptop, &tab));
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// A secret never reaches the record: not when the engine leaves it out, as
/// it does, and not when an engine lists it anyway. It is in no state of the
/// record, no version of its history, no answer of a read and no byte of the
/// document.
#[test]
fn a_secret_filled_into_the_page_is_never_written() {
    const PASSWORD: &str = "correct-horse-battery-staple-9";
    const CARD: &str = "4929-1234-5678-9012";
    let engine = TestEngine::new();
    let mut app = open(&engine);
    let ada = agent("agent:ada");
    let secret = |text: &str| BrowserEffect::FillPageSecret {
        element: "d1e5".into(),
        text: text.into(),
        frame: None,
    };

    // The engine sets a secret fill down without its text.
    engine_sets_down(
        &engine,
        &json!({"seq": 1, "time_ms": 1_700_000_000_001_u64, "actor": "agent:ada",
            "action": "fill", "ok": true, "message": "Filled textbox \"Password\"", "url": PAGE,
            "target": {"role": "textbox", "name": "Password"}}),
    );
    act(&mut app, Some(&ada), &secret(PASSWORD)).expect("the secret fill answers");
    // What is not a secret is written, so its absence below means something.
    engine_sets_down(&engine, &email_filled(2));
    act(&mut app, Some(&ada), &fill("d1e2", "ada@example.org")).expect("the fill answers");
    // An engine that listed a secret all the same, as the text written, as a
    // chosen value and as a form's answer: the window sent it as a secret,
    // so it writes none of them.
    engine_sets_down(
        &engine,
        &json!({"seq": 3, "time_ms": 1_700_000_000_003_u64, "actor": "agent:ada",
            "action": "fill", "ok": true, "message": "Filled textbox \"Card number\"",
            "url": PAGE, "target": {"role": "textbox", "name": "Card number"},
            "text": CARD, "values": [CARD, "Visa"],
            "answers": [{"name": "Card number", "value": CARD},
                {"name": "Name on card", "value": "Ada Lovelace"}]}),
    );
    act(&mut app, Some(&ada), &secret(CARD)).expect("the secret fill answers");
    app.settle();

    let record = record(&mut app);
    let tab = only_tab(&record);
    assert_eq!(held(&record, &tab).len(), 3, "all three are set down");
    let read = review(&mut app, None).to_string();
    let state = format!("{:?}", record.get());
    let (bytes, versions) = record
        .record()
        .read(|doc| {
            let mut doc = doc.clone();
            let versions: Vec<String> = history::versions(&mut doc)
                .into_iter()
                .map(|version| {
                    let then = history::read_at::<PageRecord>(&mut doc, &[version.hash])
                        .expect("a past version of the record reads");
                    format!("{then:?} {:?} {:?}", version.context, version.message)
                })
                .collect();
            (doc.save_nocompress(), versions)
        })
        .expect("the record is open");
    for secret in [PASSWORD, CARD] {
        assert!(!read.contains(secret), "a read answers {secret}: {read}");
        assert!(
            !state.contains(secret),
            "the record holds {secret}: {state}"
        );
        for version in &versions {
            assert!(
                !version.contains(secret),
                "history holds {secret}: {version}"
            );
        }
        assert!(
            !contains(&bytes, secret),
            "the document's bytes hold {secret}"
        );
    }
    // The same search finds what was written, and what the engine listed
    // beside the secret is kept.
    for written in ["ada@example.org", "Ada Lovelace", "Visa", "Card number"] {
        assert!(read.contains(written), "{written}: {read}");
        assert!(state.contains(written), "{written}: {state}");
        assert!(contains(&bytes, written), "{written} is in the document");
    }
    let card = record.with(|state| state.page(&tab, 2).entries);
    assert_eq!(
        card,
        [
            json!({"seq": 3, "time_ms": 1_700_000_000_003_u64, "actor": "agent:ada",
            "action": "fill", "ok": true, "message": "Filled textbox \"Card number\"",
            "url": PAGE, "target": {"role": "textbox", "name": "Card number"},
            "values": ["Visa"],
            "answers": [{"name": "Name on card", "value": "Ada Lovelace"}]})
        ]
    );
}

/// Browser's record of page actions in a window whose page nobody acted
/// on, with the window that keeps it.
fn quiet_window() -> (App, PersonRecord<PageRecord>) {
    let mut app = open(&TestEngine::new());
    let record = record(&mut app);
    (app, record)
}

/// The window's own writing of `entry` into `tab`'s record.
fn setting_down(tab: &str, entry: Value) -> Invoke<PageRecord> {
    PageRecord::SET_DOWN.with(page_actions::SetDown {
        tab: tab.into(),
        device: "laptop".into(),
        entry,
    })
}

/// Set `entry` down in `tab` as a window does, authored by `by`: the invoker
/// the door admitted the action for (nobody: the person at the window).
fn set_down(
    record: &PersonRecord<PageRecord>,
    by: Option<&ActionInvoker>,
    tab: &str,
    entry: Value,
) -> Result<(), String> {
    let invoke = setting_down(tab, entry);
    match by {
        Some(by) => record.invoke_as(by, &invoke),
        None => record.invoke(&invoke),
    }
    .map_err(|error| error.to_string())
}

fn clicked(seq: u64) -> Value {
    json!({"seq": seq, "time_ms": 1_700_000_000_000_u64 + seq, "actor": "agent:ada",
        "action": "click", "ok": true, "message": format!("Clicked button \"{seq}\""),
        "url": PAGE, "target": {"role": "button", "name": seq.to_string()}})
}

/// The record keeps of an entry only what a record entry is, each text
/// within its bound, and neither text the page refused nor anything the
/// entry carries beside.
#[test]
fn an_entry_is_kept_by_what_the_record_keeps_of_one() {
    let (_app, record) = quiet_window();
    let by = agent("agent:ada");
    let long = "é".repeat(400);
    set_down(
        &record,
        Some(&by),
        "tab-a",
        json!({"seq": 1.0, "time_ms": 1_700_000_000_001.0_f64, "actor": "agent:ada",
            "action": "fill", "ok": true, "message": long, "url": PAGE,
            "target": {"role": "textbox", "name": "Notes", "nth": 2.0}, "text": long,
            "password": "never-kept", "cookies": {"session": "never-kept"}}),
    )
    .unwrap();
    // Text the page refused is not what was written.
    set_down(
        &record,
        Some(&by),
        "tab-a",
        json!({"seq": 2, "actor": "agent:ada", "action": "fill", "ok": false,
            "code": "not_editable", "message": "The field takes no text", "url": PAGE,
            "text": "refused text", "values": ["refused value"],
            "answers": [{"name": "Refused", "value": "refused answer"}]}),
    )
    .unwrap();
    let state = record.get();
    let kept = state.page("tab-a", 0).entries;
    let clipped = "é".repeat(100);
    assert_eq!(
        kept,
        [
            json!({"seq": 1, "time_ms": 1_700_000_000_001_u64, "actor": "agent:ada",
                "action": "fill", "ok": true, "message": clipped, "url": PAGE,
                "target": {"role": "textbox", "name": "Notes", "nth": 2}, "text": clipped}),
            json!({"seq": 2, "actor": "agent:ada", "action": "fill", "ok": false,
                "code": "not_editable", "message": "The field takes no text", "url": PAGE}),
        ]
    );
    assert!(!format!("{state:?}").contains("never-kept"));
    assert!(!format!("{state:?}").contains("refused"));

    // A place is set down once, a tab has an id, and an entry says what was
    // done.
    for (tab, entry) in [
        ("tab-a", clicked(2)),
        ("tab-a", clicked(1)),
        ("", clicked(3)),
        ("tab/a", clicked(3)),
        ("tab-a", json!({"seq": 3, "ok": true})),
        ("tab-a", json!({"seq": 0, "action": "click", "ok": true})),
        ("tab-a", json!("click")),
    ] {
        assert!(
            set_down(&record, Some(&by), tab, entry.clone()).is_err(),
            "{tab} {entry}"
        );
    }
    assert_eq!(record.get().page("tab-a", 0).entries.len(), 2);
}

/// The window writes an action as whoever did it and as nobody else: an
/// entry under another actor's name is refused, and so is what a person
/// typed or did at the window written as an agent, or an agent's action
/// written as the person.
#[test]
fn an_action_is_written_only_as_the_actor_who_did_it() {
    let typed = json!({"seq": 1, "actor": "", "how": "input", "action": "input", "ok": true,
        "target": {"role": "textbox", "name": "Passport number"}, "text": "P1234567"});
    let own = json!({"seq": 1, "actor": "", "action": "click", "ok": true});
    let long = format!("agent:{}", "x".repeat(120));
    let by_long = |seq: u64| {
        let mut entry = clicked(seq);
        entry["actor"] = PageActor::named(&long)
            .expect("a long principal names an actor")
            .as_str()
            .into();
        entry
    };

    let (_app, record) = quiet_window();
    let mallory = agent("agent:mallory");
    for forged in [clicked(1), typed.clone(), own.clone(), by_long(1)] {
        let refused = set_down(&record, Some(&mallory), "tab-a", forged.clone()).unwrap_err();
        assert!(
            refused.contains("the actor who did it"),
            "{forged}: {refused}"
        );
    }
    assert!(record.with(|state| state.actions.is_empty()));

    // The agent that acted is who its action is written as, under the name
    // the engine keeps it by: its principal, shortened when the engine takes
    // no longer one.
    set_down(&record, Some(&agent("agent:ada")), "tab-a", clicked(1)).unwrap();
    set_down(&record, Some(&agent(&long)), "tab-b", by_long(1)).unwrap();

    // What a person typed and what they did at the window is written as
    // theirs, and no agent's action is.
    set_down(&record, None, "tab-c", typed).unwrap();
    set_down(&record, None, "tab-d", own).unwrap();
    assert!(set_down(&record, None, "tab-a", clicked(2)).is_err());
    assert_eq!(record.with(|state| state.actions.len()), 4);
}

/// The record is bounded as the engine bounds its own: a tab keeps its
/// newest actions, the oldest leave the state counted as dropped and stay
/// in the record's history; a read answers a bounded part and says where to
/// read on.
#[test]
fn a_tab_keeps_its_newest_actions_and_older_ones_stay_in_history() {
    let (_app, record) = quiet_window();
    let by = agent("agent:ada");
    let over = 3;
    let total = MAX_TAB_ACTIONS as u64 + over;
    let mut first_full = Vec::new();
    for seq in 1..=total {
        set_down(&record, Some(&by), "tab-a", clicked(seq)).unwrap();
        if seq == 1 {
            first_full = record
                .record()
                .read(|doc| doc.clone().get_heads())
                .expect("the record is open");
        }
    }
    let state = record.get();
    assert_eq!(state.actions.len(), MAX_TAB_ACTIONS);
    let tab = state.tabs.get("tab-a").unwrap();
    assert_eq!((tab.last, tab.dropped), (total, over));

    // Reads are bounded and read on by place, to the newest.
    let mut since = 0;
    let mut read = Vec::new();
    loop {
        let page = state.page("tab-a", since);
        assert!(page.entries.len() <= MAX_READ);
        assert_eq!(page.dropped, over);
        read.extend(
            page.entries
                .iter()
                .map(|entry| entry["seq"].as_u64().unwrap()),
        );
        since = page.next;
        if !page.more {
            break;
        }
    }
    assert_eq!(read, (over + 1..=total).collect::<Vec<_>>());

    // What left the state is still what History shows of that time.
    let then = record
        .record()
        .read(|doc| history::read_at::<PageRecord>(&mut doc.clone(), &first_full))
        .expect("the record is open")
        .expect("the record's first version reads");
    assert_eq!(then.page("tab-a", 0).entries, [clicked(1)]);
}

/// The record keeps the tabs that acted last: a new tab beyond the bound
/// takes the place of the one that acted longest ago.
#[test]
fn the_record_keeps_the_tabs_that_acted_last() {
    let (_app, record) = quiet_window();
    let by = agent("agent:ada");
    for tab in 0..=MAX_TABS as u64 {
        // Each tab acts a little later than the one before it.
        let mut entry = clicked(1);
        entry["time_ms"] = (1_700_000_000_000_u64 + tab).into();
        set_down(&record, Some(&by), &format!("tab-{tab}"), entry).unwrap();
    }
    let state = record.get();
    assert_eq!(state.tabs.len(), MAX_TABS);
    assert_eq!(state.actions.len(), MAX_TABS);
    assert!(state.tabs.get("tab-0").is_none(), "the oldest made room");
    assert!(state.page("tab-0", 0).entries.is_empty());
    assert_eq!(state.page("tab-1", 0).entries.len(), 1);
    assert_eq!(state.page(&format!("tab-{MAX_TABS}"), 0).entries.len(), 1);
    // One tab's places never read as another's.
    assert_eq!(state.page("tab-1", 0).entries.len(), 1);
    assert!(state.page("tab", 0).entries.is_empty());
}

/// The actions the window's door offers on the record.
fn offered_on_the_record(app: &mut App) -> Vec<String> {
    app.agent_surface()
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter(|row| row["resource"] == RESOURCE)
        .filter_map(|row| row["action"].as_str().map(str::to_owned))
        .collect()
}

/// Why the door refused a call: it must have refused it, and at once.
fn refusal(answer: Result<Option<Value>, String>) -> String {
    let refusal = answer.expect_err("the door takes it from nobody");
    assert_ne!(refusal, NO_ANSWER, "the door refuses at once");
    refusal
}

/// Only a window adds to the record, and only what its engine listed. An
/// agent the door admitted for the person cannot set down, under its own
/// name, an action it never did: not by the verb's name, which the door
/// does not know, not as a value of `apply`, not in a group. Nor can the
/// person. The record stays what the engine listed, a reviewer reads it as
/// before, and what the window writes next still lands as the change of the
/// agent that acted.
#[test]
fn nobody_at_the_door_adds_to_the_record() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    let (ada_agent, ben) = (agent_for_ada("agent:ada"), agent_for_ada("agent:ben"));
    engine_sets_down(&engine, &email_filled(1));
    act(&mut app, Some(&ada_agent), &fill("d1e2", "ada@example.org")).expect("the fill answers");
    app.settle();
    let record = record(&mut app);
    let tab = only_tab(&record);
    let listed = record.with(|state| state.page(&tab, 0).entries);
    assert_eq!(listed, [email_filled(1)]);

    // The record is at the door with what every record offers, and without
    // an action that sets one down.
    let offered = offered_on_the_record(&mut app);
    for open in ["apply", "group"] {
        assert!(
            offered.iter().any(|action| action == open),
            "{open} in {offered:?}"
        );
    }
    assert!(
        !offered.iter().any(|action| action == "set-down"),
        "{offered:?}"
    );

    // Ada's agent never sent the form and Ada typed nothing. Each claims it
    // did, with an entry the record keeps when the window writes it: under
    // the claimant's own name, at the tab's next place.
    let typed = json!({"seq": 2, "time_ms": 1_700_000_000_002_u64, "actor": "", "how": "input",
        "action": "input", "ok": true, "url": PAGE,
        "target": {"role": "textbox", "name": "Passport number"}, "text": "P1234567"});
    for (claimant, claimed) in [(Some(&ada_agent), form_sent(2)), (None, typed)] {
        let named = setting_down(&tab, claimed);
        let unknown = refusal(invoke(
            &mut app,
            claimant,
            named.address_on(RESOURCE),
            &named.arguments,
        ));
        assert!(unknown.contains("no action addressed by"), "{unknown}");

        let value = named.build(0).expect("Browser builds its own writing");
        let value = Invoke::<PageRecord>::apply(&value).expect("the writing as a value");
        refusal(invoke(
            &mut app,
            claimant,
            value.address_on(RESOURCE),
            &value.arguments,
        ));

        let grouped = Invoke::group([named]);
        refusal(invoke(
            &mut app,
            claimant,
            grouped.address_on(RESOURCE),
            &grouped.arguments,
        ));
    }
    app.settle();
    assert_eq!(
        record.with(|state| state.page(&tab, 0).entries),
        listed,
        "the record holds what the engine listed, and nothing claimed"
    );
    assert_eq!(
        steps(&record, &tab),
        [(
            "agent:ada".to_owned(),
            1,
            "Filled textbox \"Email\"".to_owned()
        )]
    );

    // A reviewer reads the record as before.
    engine.push_page_reply(Ok(listing(&[], &json!({}))));
    let read = review(&mut app, None);
    assert_eq!(read["message"], "1 action");
    assert_eq!(read["data"]["entries"], json!([email_filled(1)]));
    assert_eq!(read["data"]["next"], 1);

    // What the engine lists next the window still writes, as the change of
    // the agent that acted.
    engine_sets_down(&engine, &country_chosen(2));
    let chosen = BrowserEffect::SelectPage {
        element: "d1e4".into(),
        values: vec!["Belgium".into()],
        frame: None,
    };
    act(&mut app, Some(&ben), &chosen).expect("the choice answers");
    app.settle();
    assert_eq!(
        held(&record, &tab),
        [
            (1, "agent:ada".to_owned(), "fill".to_owned()),
            (2, "agent:ben".to_owned(), "select".to_owned()),
        ]
    );
    let authors: Vec<String> = steps(&record, &tab)
        .into_iter()
        .map(|(author, ..)| author)
        .collect();
    assert_eq!(authors, ["agent:ada", "agent:ben"]);
}
