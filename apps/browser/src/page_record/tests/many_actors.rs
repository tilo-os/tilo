//! A tab worked in by more actors than its window remembers the admission
//! of ([`MAX_ACTORS`]): what the record, a reviewer's reads and the review
//! screen then say of the actor admitted longest ago.
//!
//! The window knows whom the action system admitted under a name, and from
//! which device, only for as long as it remembers that admission. Two things
//! must hold past the limit. An actor whose page action still runs or waits
//! its turn is not forgotten for the sake of the ones that came after it. And
//! where the window has forgotten, what the engine then lists under that
//! name says so, instead of reading as an actor on the device that runs the
//! page.

use ui::tilo_actions::InvocationId;

use super::*;
use crate::page_record::MAX_ACTORS;

/// Start the page action `effect` for `invoker`, as the host does for an
/// agent it admitted, and do not wait for it: its invocation, or what the
/// tab turned it away with at once.
fn start(
    app: &mut App,
    invoker: &ActionInvoker,
    effect: &BrowserEffect,
) -> Result<InvocationId, String> {
    let instance = app
        .harness()
        .component_instance_id()
        .ok_or("Browser has no component")?;
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: instance,
        address: effect.address_on("browser.page"),
    };
    let (invocation, _) = app
        .harness()
        .start_bound_job_for(Some(invoker.clone()), &address, &effect.arguments(), true)
        .map_err(|error| error.to_string())?;
    match ended_as(app, invocation) {
        Some(Err(refusal)) => Err(refusal),
        _ => Ok(invocation),
    }
}

/// How the job `invocation` ended, if it has.
fn ended_as(app: &mut App, invocation: InvocationId) -> Option<Result<(), String>> {
    app.harness()
        .job_updates(invocation)
        .iter()
        .find_map(|update| match update {
            InvocationUpdate::Succeeded { .. } => Some(Ok(())),
            InvocationUpdate::Failed { error, .. } => {
                Some(Err(format!("{}: {}", error.code, error.message)))
            }
            _ => None,
        })
}

/// Wait for the job `invocation` to end, and say how it did.
fn ended(app: &mut App, invocation: InvocationId) -> Result<(), String> {
    app.harness()
        .wait_until(ui::time::Duration::from_secs(30), |harness| {
            harness
                .job_updates(invocation)
                .iter()
                .any(InvocationUpdate::is_terminal)
        });
    ended_as(app, invocation).unwrap_or_else(|| Err(NO_ANSWER.to_owned()))
}

/// The `n`th of a crowd of agents on the device that runs the page.
fn crowd(n: usize) -> ActionInvoker {
    agent(&format!("agent:crowd-{n}"))
}

/// What `tab`'s record holds, as a reader of it is answered.
fn entries(record: &PersonRecord<PageRecord>, tab: &str) -> Vec<Value> {
    record.with(|state| state.page(tab, 0).entries)
}

/// The device each change an agent authored in the record's history names.
fn devices_in_history(record: &PersonRecord<PageRecord>) -> Vec<Option<InvokerDevice>> {
    record
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
        .expect("the record is open")
}

/// Who set a field, as the engine's review of the page says it: by name and
/// place alone.
fn set_by(actor: &str, seq: u64) -> Value {
    json!({"how": "action", "by": actor, "seq": seq})
}

/// A field of the page's form as the engine's review lists it.
fn reviewed_field(name: &str, set: &Value) -> Value {
    json!({"element": "d1e2", "role": "textbox", "name": name, "value": name,
        "form": "form1", "question": name, "set": set})
}

/// An agent on Ada's laptop has a fill waiting its turn in a busy tab when
/// as many other agents as the window remembers ask for the tab too. Most
/// are turned away at once, told what holds the tab and to ask again; all of
/// them were admitted. The waiting agent's fill then runs, and it is set
/// down as the agent the door admitted, from the laptop: the window does not
/// forget an actor whose action is still to come for the sake of the ones
/// that came after it.
#[test]
fn an_actor_whose_action_waits_its_turn_is_not_forgotten_for_a_crowd() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    let laptop = laptop();
    let far = agent_on("agent:task-x", &laptop);
    let near = agent("agent:task-0");
    engine_sets_down(&engine, &field_filled(1, "agent:task-x", "Given names"));
    act(&mut app, Some(&far), &fill("d1e2", "Maria Elena")).expect("the first fill answers");
    app.settle();

    // The engine answers each fill in the order the tab runs them: the one
    // that holds the tab, then the one that waits behind it.
    engine_sets_down(&engine, &field_filled(2, "agent:task-0", "Family name"));
    engine_sets_down(&engine, &field_filled(3, "agent:task-x", "Nationality"));
    let holding = start(&mut app, &near, &fill("d1e3", "Lovelace")).expect("it holds the tab");
    let waiting = start(&mut app, &far, &fill("d1e4", "British")).expect("it waits its turn");
    let mut behind = Vec::new();
    let mut turned_away = Vec::new();
    for n in 0..MAX_ACTORS {
        match start(&mut app, &crowd(n), &click("d1e9")) {
            Ok(invocation) => behind.push(invocation),
            Err(refusal) => turned_away.push(refusal),
        }
    }
    assert!(
        !behind.is_empty() && !turned_away.is_empty(),
        "some of the crowd wait and the rest are turned away: {} wait, {} turned away",
        behind.len(),
        turned_away.len()
    );
    for refusal in &turned_away {
        assert!(
            refusal.contains("page actions waiting")
                && refusal.contains("fill by \"agent:task-0\"")
                && refusal.contains("ask again once they have answered"),
            "one turned away is told what holds the tab and what to do: {refusal}"
        );
    }

    app.settle();
    ended(&mut app, holding).expect("the fill that held the tab answers");
    ended(&mut app, waiting).expect("the fill that waited answers");
    for invocation in behind {
        ended(&mut app, invocation).expect("each of the crowd that waited has its turn");
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
        on_laptop("agent:task-x"),
        ("agent:task-0".to_owned(), None, None),
        on_laptop("agent:task-x"),
    ];
    let record = record(&mut app);
    let tab = only_tab(&record);
    let kept = entries(&record, &tab);
    assert_eq!(named_in(&Value::Array(kept.clone())), expected, "{kept:?}");
    assert!(
        kept.iter()
            .all(|entry| entry.get("device_unknown").is_none()),
        "the window knew every actor it set down: {kept:?}"
    );
    let read = review(&mut app, None);
    assert_eq!(named_in(&read["data"]["entries"]), expected, "{read}");
    assert_eq!(
        devices_in_history(&record),
        [Some(laptop.clone()), None, Some(laptop.clone())],
        "each change is authored by the invoker the door admitted"
    );

    engine.set_page_data(
        "fields",
        json!({"fields": [reviewed_field("Nationality", &set_by("agent:task-x", 3))]}),
    );
    let fields = act(
        &mut app,
        None,
        &BrowserEffect::ReadPageFields { frame: None },
    )
    .expect("the fields answer");
    assert_eq!(
        fields["data"]["fields"][0]["set"],
        json!({"how": "action", "by": "agent:task-x", "seq": 3,
            "device": laptop.id, "device_name": "Studio laptop"})
    );
}

/// The engine answers an agent's fill and says where it set it down, and
/// before the window has read that entry from the engine's record as many
/// other actors as it remembers read the tab's record: agents turned away
/// from a busy tab ask again the moment an action answers. The entry is
/// still set down as the agent the door admitted, from its laptop. The
/// window owes the record that entry, and does not forget its actor before
/// it is written.
#[test]
fn an_actor_is_not_forgotten_before_its_answered_action_is_set_down() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    let laptop = laptop();
    let far = agent_on("agent:task-x", &laptop);
    let filled = field_filled(1, "agent:task-x", "Given names");
    // The fill's answer names its place in the engine's record. What the
    // engine lists there is read a moment later.
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
    for n in 0..MAX_ACTORS {
        act(
            &mut app,
            Some(&crowd(n)),
            &BrowserEffect::ReadPageRecord { since: None },
        )
        .expect("the record answers each of the crowd");
    }

    // The engine lists the fill from now on, and the next read writes it.
    engine.set_page_data(
        "ledger",
        json!({"entries": [filled], "next": 1, "more": false, "dropped": 0, "waiting": 0}),
    );
    let read = review(&mut app, None);
    assert_eq!(
        named_in(&read["data"]["entries"]),
        [(
            "agent:task-x".to_owned(),
            Some(laptop.id.clone()),
            Some("Studio laptop".to_owned())
        )],
        "{read}"
    );
    assert!(
        read["data"]["entries"][0].get("device_unknown").is_none(),
        "{read}"
    );
    assert_eq!(devices_in_history(&record), [Some(laptop)]);
}

/// The page posts on its own, an agent on Ada's laptop keeps the page as it
/// is, and before the engine says how the hold ended as many other actors as
/// the window remembers read the tab's record: the window no longer holds
/// whom the door admitted under the agent's name. Returns the laptop and the
/// agent.
fn an_actor_is_forgotten_before_its_answer_is_set_down(
    app: &mut App,
    engine: &TestEngine,
) -> (InvokerDevice, ActionInvoker) {
    let laptop = laptop();
    let far = agent_for_ada("agent:task-x").from_device(Some(laptop.clone()));
    engine_sets_down(engine, &field_filled(1, "agent:task-x", "Given names"));
    act(app, Some(&far), &fill("d1e2", "Maria Elena")).expect("the fill answers");
    app.settle();
    engine_holds(engine, 2, 1);
    app.settle();
    act(
        app,
        Some(&far),
        &BrowserEffect::KeepHeldRequest { id: "1".into() },
    )
    .expect("the agent's answer is taken");
    for n in 0..MAX_ACTORS {
        act(
            app,
            Some(&crowd(n)),
            &BrowserEffect::ReadPageRecord { since: None },
        )
        .expect("the record answers each of the crowd");
    }
    let kept = engine_ends_hold(engine, 3, 1);
    assert_eq!(
        (kept["action"].as_str(), kept["actor"].as_str()),
        (Some("keep"), Some("agent:task-x")),
        "the window told the engine who kept the page"
    );
    app.settle();
    (laptop, far)
}

/// What the engine lists under a name the window has forgotten is set down
/// as that actor's, and says that where the actor worked from is not known:
/// it names no device, and does not read as an actor on the device that runs
/// the page. What the same actor did while the window knew it keeps its
/// device, in the record, in a reviewer's read of it and in who set a field.
/// The actor's next action is admitted again and is set down with its device.
#[test]
fn what_is_set_down_under_a_forgotten_name_says_its_device_is_not_known() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    let (laptop, far) = an_actor_is_forgotten_before_its_answer_is_set_down(&mut app, &engine);

    let on_laptop = (
        "agent:task-x".to_owned(),
        Some(laptop.id.clone()),
        Some("Studio laptop".to_owned()),
    );
    let nobody = (String::new(), None, None);
    let unplaced = ("agent:task-x".to_owned(), None, None);
    let record = record(&mut app);
    let tab = only_tab(&record);
    let kept = entries(&record, &tab);
    assert_eq!(
        named_in(&Value::Array(kept.clone())),
        [on_laptop.clone(), nobody.clone(), unplaced.clone()],
        "{kept:?}"
    );
    let unknown = |entries: &[Value]| -> Vec<bool> {
        entries
            .iter()
            .map(|entry| entry.get("device_unknown") == Some(&json!(true)))
            .collect()
    };
    assert_eq!(
        unknown(&kept),
        [false, false, true],
        "only what was set down after the window forgot says so: {kept:?}"
    );

    // A reviewer's read of the record answers the same.
    let read = review(&mut app, None);
    let listed = read["data"]["entries"].as_array().expect("entries");
    assert_eq!(
        named_in(&read["data"]["entries"]),
        [on_laptop.clone(), nobody, unplaced],
        "{read}"
    );
    assert_eq!(unknown(listed), [false, false, true], "{read}");

    // Who set a field: the record's own word where it keeps the action, and
    // not known where it keeps none and the window has forgotten the actor.
    engine.set_page_data(
        "fields",
        json!({"fields": [
            reviewed_field("Given names", &set_by("agent:task-x", 1)),
            reviewed_field("Family name", &set_by("agent:task-x", 9)),
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
            json!({"how": "action", "by": "agent:task-x", "seq": 1,
                "device": laptop.id, "device_name": "Studio laptop"}),
            json!({"how": "action", "by": "agent:task-x", "seq": 9, "device_unknown": true}),
        ]
    );

    // The record's history says no more than the record: the change that
    // set the answer down names the actor and no device.
    assert_eq!(devices_in_history(&record), [Some(laptop), None]);

    // Acting again admits the actor again.
    engine_sets_down(&engine, &field_filled(4, "agent:task-x", "Nationality"));
    act(&mut app, Some(&far), &fill("d1e4", "British")).expect("the fill answers");
    app.settle();
    let kept = entries(&record, &tab);
    assert_eq!(named_in(&Value::Array(kept.clone()))[3], on_laptop);
    assert_eq!(unknown(&kept), [false, false, true, false], "{kept:?}");
}

/// The person's review screen says the same in words: the answer the agent
/// set while the window knew it stands under the agent on its laptop, and
/// the step set down after the window forgot the agent says that its device
/// is not known, instead of showing the agent as one on this device.
#[test]
fn the_review_screen_says_a_forgotten_actors_device_is_not_known() {
    let engine = TestEngine::new();
    let mut app = open(&engine);
    an_actor_is_forgotten_before_its_answer_is_set_down(&mut app, &engine);

    // What the page holds and what its engine lists of the tab from now on.
    engine.set_page_data(
        "forms",
        json!({"forms": [{"form": "form1", "label": "Apply for a visitor visa",
            "method": "post", "action": "/apply", "signature": "5d0c7a1e",
            "submit": ["Send application"],
            "fields": [{"name": "Given names", "kind": "text", "element": "d1e2",
                "value": "Maria Elena"}]}]}),
    );
    engine.set_page_data(
        "fields",
        json!({"fields": [{"element": "d1e2", "role": "textbox", "name": "Given names",
            "value": "Maria Elena", "form": "form1", "question": "Given names",
            "set": set_by("agent:task-x", 1)}]}),
    );
    engine.set_page_data(
        "ledger",
        json!({"entries": [
            field_filled(1, "agent:task-x", "Given names"),
            {"seq": 2, "time_ms": 1_700_000_000_002_u64, "actor": "", "how": "hold",
                "action": "request", "ok": true, "message": "POST https://orders.test",
                "url": PAGE, "to_url": "https://orders.test"},
            {"seq": 3, "time_ms": 1_700_000_000_003_u64, "actor": "agent:task-x",
                "how": "hold", "action": "keep", "ok": true,
                "message": "POST https://orders.test", "url": PAGE,
                "to_url": "https://orders.test"},
        ], "next": 3, "more": false, "dropped": 0, "waiting": 0}),
    );
    act(&mut app, None, &BrowserEffect::ReviewPage).expect("the review opens");

    let says = |app: &mut App, words: &str| app.texts().iter().any(|text| text.contains(words));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !(says(&mut app, "Set by task-x on Studio laptop")
        && says(&mut app, "By task-x, device not known"))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the review names the laptop where the record keeps it and says where it is not \
             known; the window says {:?}",
            app.texts()
        );
        app.settle();
        app.harness()
            .wait_until(ui::time::Duration::from_millis(20), |_| false);
    }
    let texts = app.texts();
    let kept: Vec<&str> = texts
        .iter()
        .zip(texts.iter().skip(1))
        .filter(|(title, _)| *title == "Kept as it is")
        .map(|(_, said)| said.as_str())
        .collect();
    // The row ends with when it was set down, as this device shows times.
    assert!(
        matches!(
            kept.as_slice(),
            [said] if said.starts_with("POST https://orders.test, By task-x, device not known")
        ),
        "the step set down under the forgotten name: {kept:?}"
    );
    assert!(
        texts
            .iter()
            .any(|text| text.contains("By task-x on Studio laptop")),
        "the fill set down while the window knew the agent keeps its device: {texts:?}"
    );
}
