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
    ActionAddress, ActionInvoker, ActorKind, InteractionActor, InvocationUpdate, InvokerDevice,
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

/// Ada's laptop, another of her devices, as the agent door of the device
/// that runs the page verified it for a connection that came from there.
fn laptop() -> InvokerDevice {
    InvokerDevice::certified([0x5c; 32], "Studio laptop")
}

/// An agent the action door admitted that works from `device`, another of
/// Ada's devices: what the host says of an agent whose connection its agent
/// door verified to come from there.
fn agent_on(principal: &str, device: &InvokerDevice) -> ActionInvoker {
    agent(principal).from_device(Some(device.clone()))
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

/// An agent fills the field `name`, as the engine sets it down at `seq`
/// under the agent's name: the engine knows an actor by its name alone.
fn field_filled(seq: u64, actor: &str, name: &str) -> Value {
    json!({"seq": seq, "time_ms": 1_700_000_000_000_u64 + seq, "actor": actor,
        "action": "fill", "ok": true, "message": format!("Filled textbox \"{name}\""),
        "url": PAGE, "target": {"role": "textbox", "name": name}, "text": name})
}

/// The actor and the device each entry names, with the device's name.
fn named_in(entries: &Value) -> Vec<(String, Option<String>, Option<String>)> {
    let said = |entry: &Value, key: &str| entry.get(key).and_then(Value::as_str).map(str::to_owned);
    entries
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| {
            (
                said(entry, "actor").unwrap_or_default(),
                said(entry, "device"),
                said(entry, "device_name"),
            )
        })
        .collect()
}

/// Agents on three machines work in one tab: two on Ada's laptop, one on a
/// device of hers that names itself nothing, one on the device that runs
/// the page. The record names, beside each actor, the device the door
/// verified it came from, and none for the one here, whatever the engine
/// lists. An actor's name stays the principal the door admitted, so two
/// agents of one device are two actors and one agent is the same actor
/// across its actions. The record's read, the review of the page's fields
/// and the record's history say the same.
#[test]
fn an_agent_from_another_device_is_set_down_with_the_device_it_was_admitted_from() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    let laptop = laptop();
    let unnamed = InvokerDevice::certified([0x40; 32], "");
    let first = agent_on("agent:task-a1", &laptop);
    let second = agent_on("agent:task-a2", &laptop);
    let third = agent_on("agent:task-d1", &unnamed);
    let here = agent("agent:task-b1");
    // The engine is told a device by nobody, and one it lists is nothing a
    // door verified: here it says the agent on this device is on the laptop.
    let mut forged = field_filled(4, "agent:task-b1", "Arrival");
    forged["device"] = laptop.id.clone().into();
    forged["device_name"] = "Studio laptop".into();
    for (by, listed) in [
        (&first, field_filled(1, "agent:task-a1", "Given names")),
        (&second, field_filled(2, "agent:task-a2", "Family name")),
        (&third, field_filled(3, "agent:task-d1", "Passport number")),
        (&here, forged),
        (&first, field_filled(5, "agent:task-a1", "Nationality")),
    ] {
        engine_sets_down(&engine, &listed);
        act(&mut app, Some(by), &fill("d1e2", "an answer")).expect("the fill answers");
    }
    app.settle();

    let on_laptop = |actor: &str| {
        (
            actor.to_owned(),
            Some(laptop.id.clone()),
            Some("Studio laptop".to_owned()),
        )
    };
    let expected = [
        on_laptop("agent:task-a1"),
        on_laptop("agent:task-a2"),
        ("agent:task-d1".to_owned(), Some(unnamed.id.clone()), None),
        ("agent:task-b1".to_owned(), None, None),
        on_laptop("agent:task-a1"),
    ];
    let record = record(&mut app);
    let tab = only_tab(&record);
    let kept = Value::Array(record.with(|state| state.page(&tab, 0).entries));
    assert_eq!(named_in(&kept), expected, "{kept}");

    // A reviewer's read of the record answers the same.
    let read = review(&mut app, None);
    assert_eq!(named_in(&read["data"]["entries"]), expected, "{read}");

    // The review of the page's fields says who set each as the engine says
    // it, by name and place, and the window names the device beside it.
    let set = |actor: &str, seq: u64| json!({"how": "action", "by": actor, "seq": seq});
    let field = |name: &str, set: Value| {
        json!({"element": "d1e2", "role": "textbox", "name": name, "value": name,
            "form": "form1", "question": name, "set": set})
    };
    engine.set_page_data(
        "fields",
        json!({"fields": [
            field("Given names", set("agent:task-a1", 1)),
            field("Passport number", set("agent:task-d1", 3)),
            field("Arrival", set("agent:task-b1", 4)),
            field("Notes", json!({"how": "input"})),
        ]}),
    );
    let fields = act(
        &mut app,
        None,
        &BrowserEffect::ReadPageFields { frame: None },
    )
    .expect("the fields answer");
    let who: Vec<Value> = fields["data"]["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|field| field["set"].clone())
        .collect();
    assert_eq!(
        who,
        [
            json!({"how": "action", "by": "agent:task-a1", "seq": 1,
                "device": laptop.id, "device_name": "Studio laptop"}),
            json!({"how": "action", "by": "agent:task-d1", "seq": 3, "device": unnamed.id}),
            json!({"how": "action", "by": "agent:task-b1", "seq": 4}),
            json!({"how": "input"}),
        ]
    );

    // Each action is its agent's change of the record, and the change says
    // the device too.
    let from: Vec<Option<InvokerDevice>> = record
        .record()
        .read(|doc| {
            let mut doc = doc.clone();
            history::versions(&mut doc)
                .into_iter()
                .filter_map(|version| version.context?.invoker)
                .filter(|invoker| invoker.kind == ActorKind::Agent)
                .map(|invoker| invoker.device)
                .collect()
        })
        .expect("the record is open");
    assert_eq!(
        from,
        [
            Some(laptop.clone()),
            Some(laptop.clone()),
            Some(unnamed),
            None,
            Some(laptop),
        ]
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

/// What a person types straight into the page is no action, and no answer
/// carries it: the engine says it set an entry down, and the window writes
/// it into the tab's record in its place, as the person's change and under
/// no agent's name. One the engine set down after actions the window has
/// not read yet waits for them and follows them in order; one told twice is
/// written once. Of what was typed into a secret field the record keeps
/// that it was set, whatever the entry carries.
#[test]
fn what_a_person_types_is_set_down_in_its_place_as_the_persons() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    two_agents_answer_the_form(&mut app, &engine);
    let record = record(&mut app);
    let tab = only_tab(&record);
    assert_eq!(held(&record, &tab).len(), 3);

    // The person types an address. Nothing asked for it and nothing answers
    // with it: the engine tells of it.
    let typed = json!({"seq": 4, "time_ms": 1_700_000_000_004_u64, "actor": "", "how": "input",
        "ok": true, "message": "Direct input into textbox \"Passport number\"", "url": PAGE,
        "target": {"role": "textbox", "name": "Passport number"}, "text": "P1234567"});
    let page_asks = |engine: &TestEngine| {
        engine
            .commands()
            .iter()
            .filter(|command| matches!(command, WebViewCommand::Page { .. }))
            .count()
    };
    let asked = page_asks(&engine);
    engine.emit_page_record(4, typed.clone());
    app.settle();
    assert_eq!(
        record.with(|state| state.page(&tab, 3).entries).as_slice(),
        std::slice::from_ref(&typed),
        "the entry is kept as the engine set it down, with no action of anyone's"
    );
    assert_eq!(
        page_asks(&engine),
        asked,
        "what follows the record as the window has it is written as told, without a read"
    );

    // The engine sets down an agent's action and then more typing, and tells
    // of the typing before the window has read the action: the typing waits
    // for the action before it, which the window reads from the engine.
    let next = json!({"seq": 5, "time_ms": 1_700_000_000_005_u64, "actor": "agent:ben",
        "action": "click", "ok": true, "message": "Clicked button \"Next\"", "url": PAGE,
        "target": {"role": "button", "name": "Next"}});
    let more = json!({"seq": 6, "time_ms": 1_700_000_000_006_u64, "actor": "", "how": "input",
        "ok": true, "message": "Direct input into checkbox \"Accept terms\"", "url": PAGE,
        "target": {"role": "checkbox", "name": "Accept terms"}, "text": "checked"});
    engine.push_page_reply(Ok(listing(&[next.clone(), more.clone()], &json!({}))));
    engine.emit_page_record(6, more.clone());
    for _ in 0..3 {
        app.settle();
    }
    // Told again, it is the entry the record already holds.
    engine.emit_page_record(6, more.clone());
    engine.emit_page_record(4, typed);
    for _ in 0..3 {
        app.settle();
    }
    assert_eq!(
        held(&record, &tab)[3..],
        [
            (4, String::new(), String::new()),
            (5, "agent:ben".to_owned(), "click".to_owned()),
            (6, String::new(), String::new()),
        ],
        "each in its place, once"
    );
    assert_eq!(
        record.with(|state| state.page(&tab, 4).entries),
        [next, more]
    );
    let authors: Vec<String> = steps(&record, &tab)
        .into_iter()
        .map(|(author, ..)| author)
        .collect();
    let person = ada();
    assert_eq!(
        authors[3..],
        [person.as_str(), "agent:ben", person.as_str()],
        "what was typed is the person's change, among the agents' own"
    );

    // Typed into a secret field: the engine says so and sends no text. An
    // engine that sent it all the same would not have it kept.
    engine.emit_page_record(
        7,
        json!({"seq": 7, "time_ms": 1_700_000_000_007_u64, "actor": "", "how": "input",
            "ok": true, "secret": true, "text": "hunter2-typed", "values": ["hunter2-typed"],
            "message": "Direct input into textbox \"Password\"", "url": PAGE,
            "target": {"role": "textbox", "name": "Password"}}),
    );
    app.settle();
    assert_eq!(
        record.with(|state| state.page(&tab, 6).entries),
        [
            json!({"seq": 7, "time_ms": 1_700_000_000_007_u64, "actor": "", "how": "input",
            "ok": true, "secret": true, "message": "Direct input into textbox \"Password\"",
            "url": PAGE, "target": {"role": "textbox", "name": "Password"}})
        ],
        "the record says the field was set, and holds none of it"
    );
    // A reader of the record is told the same, in the order it happened.
    let read = review(&mut app, Some(3));
    let entries = read["data"]["entries"].as_array().expect("entries");
    assert_eq!(
        entries
            .iter()
            .map(|entry| (
                entry["seq"].as_u64().unwrap_or(0),
                entry["how"].as_str().unwrap_or_default(),
                entry["secret"].as_bool().unwrap_or(false)
            ))
            .collect::<Vec<_>>(),
        [
            (4, "input", false),
            (5, "", false),
            (6, "input", false),
            (7, "input", true)
        ]
    );
    for kept in [read.to_string(), format!("{:?}", record.get())] {
        assert!(!kept.contains("hunter2"), "{kept}");
    }

    // Nobody but the window sets typing down: an agent that writes some as
    // the person's is refused, and the record is as it was.
    let forged = json!({"seq": 8, "actor": "", "how": "input", "ok": true,
        "target": {"role": "textbox", "name": "Passport number"}, "text": "X0000000"});
    let refused = set_down(&record, Some(&agent("agent:mallory")), &tab, forged).unwrap_err();
    assert!(refused.contains("the actor who did it"), "{refused}");
    assert_eq!(held(&record, &tab).len(), 7);
}

/// The engine holds the request `id` the page sends on its own, and sets
/// that down at `seq` of its record: no actor's action reached it.
fn engine_holds(engine: &TestEngine, seq: u64, id: u32) -> Value {
    let mut request = ui::webview::HeldRequest::new(id, ui::webview::HeldKind::Request);
    request.method = "POST".into();
    request.origin = "https://orders.test".into();
    engine.hold(request);
    let entry = json!({"seq": seq, "time_ms": 1_700_000_000_000_u64 + seq, "actor": "",
        "how": "hold", "action": "request", "ok": true, "message": "POST https://orders.test",
        "url": PAGE, "to_url": "https://orders.test"});
    engine.emit_page_record(seq, entry.clone());
    entry
}

/// The engine ends the hold `id` as the window answered it and sets that
/// down at `seq`, under the name the window's answer carries: the engine
/// knows of no other.
fn engine_ends_hold(engine: &TestEngine, seq: u64, id: u32) -> Value {
    let (allow, by) = engine
        .commands()
        .into_iter()
        .find_map(|command| match command {
            WebViewCommand::AnswerHeldRequest {
                id: answered,
                allow,
                by,
            } if answered == id => Some((allow, by)),
            _ => None,
        })
        .expect("the window answered the hold");
    let ended = if allow {
        "allow"
    } else if by.expired() {
        "drop"
    } else {
        "keep"
    };
    let entry = json!({"seq": seq, "time_ms": 1_700_000_000_000_u64 + seq,
        "actor": by.actor().map_or("", PageActor::as_str), "how": "hold", "action": ended,
        "ok": true, "message": "POST https://orders.test", "url": PAGE,
        "to_url": "https://orders.test"});
    engine.emit_page_record(seq, entry.clone());
    engine.push_event(ui::webview::WebViewEvent::HeldRequestClosed { id });
    entry
}

/// What the window held is in the tab's record: that something the page was
/// about to send waited for the person, and how that ended, each in its
/// place among the actions. Who answered is the name the window's own answer
/// carried to the engine: an agent's answer is that agent's change, authored
/// by the invoker the action door admitted it for, and the person's is the
/// person's. Nobody at the door sets such an entry down.
#[test]
fn what_the_window_held_is_set_down_with_who_answered() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    two_agents_answer_the_form(&mut app, &engine);
    let record = record(&mut app);
    let tab = only_tab(&record);
    assert_eq!(held(&record, &tab).len(), 3);

    // The page posts on its own and an agent keeps the page as it is.
    let first = engine_holds(&engine, 4, 1);
    app.settle();
    let rev = agent_for_ada("agent:rev");
    act(
        &mut app,
        Some(&rev),
        &BrowserEffect::KeepHeldRequest { id: "1".into() },
    )
    .expect("the agent's answer is taken");
    let kept = engine_ends_hold(&engine, 5, 1);
    app.settle();
    // It posts again, and the person at the window lets it leave.
    let second = engine_holds(&engine, 6, 2);
    app.settle();
    act(
        &mut app,
        None,
        &BrowserEffect::SendHeldRequest { id: "2".into() },
    )
    .expect("the person's answer is taken");
    let left = engine_ends_hold(&engine, 7, 2);
    app.settle();

    assert_eq!(
        (kept["action"].as_str(), kept["actor"].as_str()),
        (Some("keep"), Some("agent:rev")),
        "the window told the engine who kept the page"
    );
    assert_eq!(
        (left["action"].as_str(), left["actor"].as_str()),
        (Some("allow"), Some("")),
        "and that the person let the second leave"
    );
    assert_eq!(
        record.with(|state| state.page(&tab, 3).entries),
        [first, kept, second, left],
        "each is kept as the engine set it down, in its place"
    );
    let authors: Vec<String> = steps(&record, &tab)
        .into_iter()
        .map(|(author, ..)| author)
        .collect();
    let person = ada();
    assert_eq!(
        authors[3..],
        [
            person.as_str(),
            "agent:rev",
            person.as_str(),
            person.as_str()
        ],
        "the agent's answer is the agent's change, and the rest the person's"
    );
    let answered_for: Vec<Option<String>> = record
        .record()
        .read(|doc| {
            let mut doc = doc.clone();
            history::versions(&mut doc)
                .into_iter()
                .filter_map(|version| version.context.as_ref()?.invoker.clone())
                .filter(|invoker| invoker.actor == "agent:rev")
                .map(|invoker| invoker.on_behalf_of)
                .collect()
        })
        .expect("the record is open");
    assert_eq!(
        answered_for,
        [Some(ada())],
        "authored by the invoker the action door admitted the answer for"
    );
    // A reader of the record is told the same.
    let read = review(&mut app, Some(3));
    let entries = read["data"]["entries"].as_array().expect("entries");
    assert_eq!(
        entries
            .iter()
            .map(|entry| (
                entry["how"].as_str().unwrap_or_default(),
                entry["action"].as_str().unwrap_or_default(),
                entry["actor"].as_str().unwrap_or_default(),
                entry["message"].as_str().unwrap_or_default(),
            ))
            .collect::<Vec<_>>(),
        [
            ("hold", "request", "", "POST https://orders.test"),
            ("hold", "keep", "agent:rev", "POST https://orders.test"),
            ("hold", "request", "", "POST https://orders.test"),
            ("hold", "allow", "", "POST https://orders.test"),
        ]
    );

    // Nobody but the window sets a hold's answer down: an agent that writes
    // the person's allow is refused, and the record is as it was.
    let forged = json!({"seq": 8, "actor": "", "how": "hold", "action": "allow", "ok": true,
        "message": "POST https://orders.test", "to_url": "https://orders.test"});
    let refused = set_down(&record, Some(&agent("agent:mallory")), &tab, forged).unwrap_err();
    assert!(refused.contains("the actor who did it"), "{refused}");
    assert_eq!(held(&record, &tab).len(), 7);
}

/// What was written into a secret field is in the page and nowhere else,
/// whatever wrote it: the record keeps of such an entry that the field was
/// set, and no text, chosen value or answer it carries. A key pressed there
/// is kept by what the engine said of it, which names no key.
#[test]
fn an_entry_of_a_secret_field_says_it_was_set_and_keeps_none_of_its_text() {
    let (_app, record) = quiet_window();
    let by = agent("agent:ada");
    let password = json!({"role": "textbox", "name": "Password"});
    set_down(
        &record,
        Some(&by),
        "tab-a",
        json!({"seq": 1, "actor": "agent:ada", "action": "fill", "ok": true, "secret": true,
            "message": "Filled textbox \"Password\"", "url": PAGE, "target": password,
            "text": "s3cret-text", "values": ["s3cret-value"],
            "answers": [{"name": "Password", "value": "s3cret-answer"}]}),
    )
    .unwrap();
    set_down(
        &record,
        Some(&by),
        "tab-a",
        json!({"seq": 2, "actor": "agent:ada", "action": "press", "ok": true, "secret": true,
            "message": "Pressed a key in a secret field", "url": PAGE,
            "target": password}),
    )
    .unwrap();
    // A flag that is no boolean marks nothing.
    set_down(
        &record,
        Some(&by),
        "tab-a",
        json!({"seq": 3, "actor": "agent:ada", "action": "fill", "ok": true, "secret": "no",
            "message": "Filled textbox \"Email\"", "url": PAGE,
            "target": {"role": "textbox", "name": "Email"}, "text": "ada@example.org"}),
    )
    .unwrap();
    let state = record.get();
    assert_eq!(
        state.page("tab-a", 0).entries,
        [
            json!({"seq": 1, "actor": "agent:ada", "action": "fill", "ok": true, "secret": true,
                "message": "Filled textbox \"Password\"", "url": PAGE, "target": password}),
            json!({"seq": 2, "actor": "agent:ada", "action": "press", "ok": true, "secret": true,
                "message": "Pressed a key in a secret field", "url": PAGE,
                "target": {"role": "textbox", "name": "Password"}}),
            json!({"seq": 3, "actor": "agent:ada", "action": "fill", "ok": true,
                "message": "Filled textbox \"Email\"", "url": PAGE,
                "target": {"role": "textbox", "name": "Email"}, "text": "ada@example.org"}),
        ]
    );
    assert!(!format!("{state:?}").contains("s3cret"));
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

/// An action names the device the door set on the invoker it is written
/// as, and no other: not one for an agent on this device, not another
/// device or another name of it for an agent from the laptop, not none for
/// that agent, and none for what the person did.
#[test]
fn an_action_names_only_the_device_its_actor_was_admitted_from() {
    let laptop = laptop();
    let on = |device: &str, name: &str| {
        let mut entry = clicked(1);
        entry["device"] = device.into();
        if !name.is_empty() {
            entry["device_name"] = name.into();
        }
        entry
    };
    let from_laptop = on(&laptop.id, "Studio laptop");
    let here = agent("agent:ada");
    let there = agent_on("agent:ada", &laptop);
    let own = json!({"seq": 1, "actor": "", "action": "click", "ok": true,
        "device": laptop.id, "device_name": "Studio laptop"});

    let (_app, record) = quiet_window();
    let forged = [
        (Some(&here), from_laptop.clone()),
        (Some(&there), on(&"66".repeat(32), "Studio laptop")),
        (Some(&there), on(&laptop.id, "Build box")),
        (Some(&there), on(&laptop.id, "")),
        (Some(&there), clicked(1)),
        (None, own),
    ];
    for (by, entry) in forged {
        let refused = set_down(&record, by, "tab-a", entry.clone()).unwrap_err();
        assert!(
            refused.contains("the device its actor was admitted from"),
            "{entry}: {refused}"
        );
    }
    assert!(record.with(|state| state.actions.is_empty()));

    set_down(&record, Some(&there), "tab-a", from_laptop.clone()).unwrap();
    set_down(&record, Some(&here), "tab-b", clicked(1)).unwrap();
    assert_eq!(
        record.with(|state| state.page("tab-a", 0).entries),
        [from_laptop]
    );
    assert_eq!(
        record.with(|state| state.page("tab-b", 0).entries),
        [clicked(1)]
    );
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

/// The form the page at [`PAGE`] asks, as the engine reads it.
const ORDER: &str = "0bde4001";

fn order_form() -> Value {
    json!({
        "form": "form1", "label": "Order", "method": "POST", "action": "/order",
        "signature": ORDER, "submit": ["Send order"],
        "fields": [{"name": "Email", "kind": "text", "required": true, "value": "",
            "element": "d1e2"}],
    })
}

/// The address of the page's published action `action`, as the host takes it
/// from what the window publishes.
fn page_action(app: &mut App, action: &str) -> ActionAddress {
    fn find<'a>(
        resources: &'a [ui::tilo_actions::ResourceSpec],
        id: &str,
    ) -> Option<&'a ui::tilo_actions::ResourceSpec> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource)
            } else {
                find(&resource.children, id)
            }
        })
    }
    let published = app.harness().resources();
    find(&published, "browser.page")
        .and_then(|page| page.action_address(action))
        .unwrap_or_else(|| panic!("the page publishes no {action}"))
}

/// A form's own action is its invoker's, as a page action is. A named agent
/// answers the page's form through the form's action and then sends it:
/// what the engine sets down under the agent's name, the answers and the
/// submission the window held, is the agent's change in the record, authored
/// by the invoker the action door admitted it as, and a reviewer reads both
/// under that agent.
#[test]
fn a_forms_own_action_is_set_down_as_the_agent_that_ran_it() {
    let engine = TestEngine::new();
    engine.set_page_data("forms", json!({ "forms": [order_form()] }));
    let mut app = open(&engine);
    engine.push_event(ui::webview::WebViewEvent::LoadFinished);
    for _ in 0..3 {
        app.settle();
    }
    let intake = agent_for_ada("agent:intake");

    // The agent answers the form through the form's own action.
    let answered = json!({"seq": 1, "time_ms": 1_700_000_000_001_u64, "actor": "agent:intake",
        "action": "fill_form", "ok": true, "message": "Answered 1 question", "url": PAGE,
        "answers": [{"name": "Email", "value": "ada@example.org"}]});
    engine.set_page_data(
        "ledger",
        json!({"entries": [answered], "next": 1, "more": false, "dropped": 0, "waiting": 0}),
    );
    engine.push_page_reply(Ok(PageReply {
        record: PageRecordNote {
            seq: Some(1),
            ..PageRecordNote::default()
        },
        data: Some(json!({
            "form": "form1", "signature": ORDER,
            "results": [{"name": "Email", "ok": true}],
            "set": [{"name": "Email", "value": "ada@example.org"}],
            "submitted": false, "now_asks": [], "unanswered": [],
        })),
        ..answer("Answered 1 question")
    }));
    let address = page_action(&mut app, &format!("form-{ORDER}"));
    let arguments = json!({"email": "ada@example.org"});
    invoke(
        &mut app,
        Some(&intake),
        address,
        arguments.as_object().expect("arguments"),
    )
    .expect("the form's action answers");
    app.settle();

    // It sends the form, and the window holds the submission for the person.
    let mut submission = ui::webview::HeldRequest::new(1, ui::webview::HeldKind::Submit);
    submission.method = "POST".into();
    submission.origin = "https://shop.test".into();
    engine.hold(submission);
    let address = page_action(&mut app, &format!("send-form-{ORDER}"));
    let needs = invoke(&mut app, Some(&intake), address, &serde_json::Map::new())
        .expect_err("the submission waits for the person");
    assert!(
        needs.contains(ui::tilo_actions::remote::CONFIRMATION_REQUIRED_CODE),
        "{needs}"
    );
    // The engine sets the hold down under the actor whose action reached it.
    engine.emit_page_record(
        2,
        json!({"seq": 2, "time_ms": 1_700_000_000_002_u64, "actor": "agent:intake",
            "how": "hold", "action": "submit", "ok": true, "message": "POST https://shop.test",
            "url": PAGE, "to_url": "https://shop.test"}),
    );
    for _ in 0..3 {
        app.settle();
    }

    let record = record(&mut app);
    let tab = only_tab(&record);
    assert_eq!(
        held(&record, &tab),
        [
            (1, "agent:intake".to_owned(), "fill_form".to_owned()),
            (2, "agent:intake".to_owned(), "submit".to_owned()),
        ],
        "the form's answers and its held submission stand under the agent"
    );
    let authored_for: Vec<(ActorKind, Option<String>)> = record
        .record()
        .read(|doc| {
            let mut doc = doc.clone();
            history::versions(&mut doc)
                .into_iter()
                .filter_map(|version| version.context.as_ref()?.invoker.clone())
                .filter(|invoker| invoker.actor == "agent:intake")
                .map(|invoker| (invoker.kind, invoker.on_behalf_of))
                .collect()
        })
        .expect("the record is open");
    assert_eq!(
        authored_for,
        [
            (ActorKind::Agent, Some(ada())),
            (ActorKind::Agent, Some(ada()))
        ],
        "each is the agent's change, authored by the invoker the action door admitted"
    );
    // A reviewer reads both under that agent.
    let read = review(&mut app, None);
    let entries = read["data"]["entries"].as_array().expect("entries");
    assert_eq!(
        entries
            .iter()
            .map(|entry| (
                entry["actor"].as_str().unwrap_or_default(),
                entry["how"].as_str().unwrap_or_default(),
                entry["action"].as_str().unwrap_or_default(),
            ))
            .collect::<Vec<_>>(),
        [
            ("agent:intake", "", "fill_form"),
            ("agent:intake", "hold", "submit")
        ]
    );
}

/// A sign-in's password never reaches the record either. The window sends it
/// as a secret, so it is in no state of the record, no version of its
/// history, no answer of a read and no byte of the document: not from the
/// answer itself, which says only who was answered, and not when an engine
/// lists the same text as something written afterwards.
#[test]
fn a_sign_ins_password_is_never_written() {
    const PASSWORD: &str = "correct-horse-battery-staple-11";
    const ASKER: &str = "https://shop.test";
    let engine = TestEngine::new();
    let mut app = open(&engine);
    let ada = agent("agent:ada");
    engine.push_event(ui::webview::WebViewEvent::HttpAuth(
        ui::webview::HttpAuthChallenge {
            id: 4,
            asker: ASKER.into(),
            proxy: false,
            realm: "Members".into(),
            scheme: "basic".into(),
            retry: false,
        },
    ));
    app.settle();
    assert!(
        published(&app.harness().resources(), crate::page_sign_in::ROW),
        "the sign-in is the window's row"
    );

    // Ada's agent answers through the row, as the host does for an agent it
    // admitted.
    let sign_in = BrowserEffect::AnswerPageSignIn {
        sign_in: 4,
        username: "ada".into(),
        password: PASSWORD.into(),
    };
    let said = invoke(
        &mut app,
        Some(&ada),
        sign_in.address_on(crate::page_sign_in::ROW),
        &sign_in.arguments(),
    )
    .expect("the sign-in answers")
    .expect("and says what it did")
    .to_string();
    assert!(said.contains(ASKER), "it names who was answered: {said}");
    assert!(
        !said.contains(PASSWORD) && !said.contains("ada"),
        "and never with what: {said}"
    );
    let sent: Vec<_> = engine
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            WebViewCommand::AnswerHttpAuth {
                id,
                asker,
                credentials,
            } => Some((id, asker, credentials)),
            _ => None,
        })
        .collect();
    assert_eq!(
        sent,
        [(
            4,
            ASKER.to_owned(),
            Some(ui::webview::HttpAuthCredentials::new("ada", PASSWORD))
        )],
        "the credentials reach the engine, for the asker that was shown"
    );
    assert!(
        !format!("{:?}", engine.commands()).contains(PASSWORD),
        "and print as nothing"
    );
    app.settle();
    assert!(
        !published(&app.harness().resources(), crate::page_sign_in::ROW),
        "answered, the row goes"
    );

    // An engine that lists the same text as written, as chosen and as a
    // form's answer: the window sent it as a secret, so it writes none.
    engine_sets_down(
        &engine,
        &json!({"seq": 1, "time_ms": 1_700_000_000_001_u64, "actor": "agent:ada",
            "action": "fill", "ok": true, "message": "Filled textbox \"Search\"", "url": PAGE,
            "target": {"role": "textbox", "name": "Search"},
            "text": PASSWORD, "values": [PASSWORD, "Members"],
            "answers": [{"name": "Search", "value": PASSWORD},
                {"name": "Area", "value": "Members area"}]}),
    );
    act(&mut app, Some(&ada), &fill("d1e2", "members")).expect("the fill answers");
    app.settle();

    let record = record(&mut app);
    let tab = only_tab(&record);
    assert_eq!(held(&record, &tab).len(), 1, "the fill is set down");
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
    assert!(!read.contains(PASSWORD), "a read answers it: {read}");
    assert!(!state.contains(PASSWORD), "the record holds it: {state}");
    for version in &versions {
        assert!(!version.contains(PASSWORD), "history holds it: {version}");
    }
    assert!(!contains(&bytes, PASSWORD), "the document's bytes hold it");
    // The same search finds what was written beside it.
    for written in ["Members area", "Search"] {
        assert!(read.contains(written), "{written}: {read}");
        assert!(contains(&bytes, written), "{written} is in the document");
    }
}
