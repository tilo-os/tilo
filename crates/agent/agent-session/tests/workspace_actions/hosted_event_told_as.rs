//! How the result of an event is told to its person, where the event says. An event a person's
//! assistant set up says whether what its work answers is told as a brief, which reaches its
//! person with a notice as a reminder does, or as news, which their own rule marks. An event
//! that says nothing is told as before. The word is the event's own: it is said only where an
//! event is set up at all, it makes a brief of nothing but that person's own event and tells
//! nobody else, and what may break do-not-disturb stays the person's own to say. All through the
//! server's own interfaces, with nothing of the person's on while the events fire.

use super::*;
use tilo_actions::remote::ActionCallError;
use tilo_actions::remote::person_telling::{self, Kind, Told, ToldList, WhyCode};
use tilo_workspace_service::agents::events::Authority;

/// What the assistant answers every turn an event starts here.
const SAID: &str = "At ten the lease is signed.";

/// What the server told outside Tilo once it told `count` things there, as its own teller tells
/// while no device of a person's is looking: whom, the thing's title and its kind, once for each
/// time it was told, in the order the things were told. Read from what the teller wrote on each
/// thing, so it is the host's one teller that is heard.
async fn told_outside(up: &Up, count: usize) -> Vec<([u8; 32], String, Kind)> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let told: Vec<([u8; 32], String, Kind)> = up
            .workspaces
            .people_told()
            .into_iter()
            .flat_map(|person| {
                up.workspaces
                    .told_after(&person, 0)
                    .told
                    .into_iter()
                    .flat_map(move |told| {
                        told.pushed
                            .iter()
                            .map(|_| (person, told.title.clone(), told.kind))
                            .collect::<Vec<_>>()
                    })
            })
            .collect();
        if told.len() >= count {
            return told;
        }
        assert!(
            Instant::now() < deadline,
            "the server told fewer than {count} things outside: {told:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// One action on a person's events as `caller` takes it, refused or not.
/// How `tried` was answered, as a test names it: done, or which refusal.
fn refusal(tried: &Result<serde_json::Value, ActionCallError>) -> &'static str {
    match tried {
        Ok(_) => "done",
        Err(ActionCallError::NotPermitted) => "not permitted",
        Err(ActionCallError::UnknownAction) => "unknown action",
        Err(ActionCallError::Refused { .. }) => "refused",
        Err(_) => "another error",
    }
}

fn tried(
    up: &Up,
    workspace: &str,
    caller: &Caller,
    action: &str,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value, ActionCallError> {
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: workspace_events::target(action),
        argument: InvokeArgument::Json(arguments.as_object().cloned().unwrap_or_default()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let unanswered = |why: String| ActionCallError::Unavailable { reason: why };
    let workspace = WorkspaceId::parse(workspace)
        .ok_or_else(|| unanswered(format!("{workspace} names no workspace")))?;
    let receipt = up
        .events
        .invoking(workspace, caller, Authority::Turn, request)
        .wait_timeout(std::time::Duration::from_secs(30))
        .map_err(|_| unanswered("the server did not answer".to_owned()))?
        .ok_or_else(|| unanswered("the server abandoned the answer".to_owned()))?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Rejected { error } => Err(error),
        other => Err(unanswered(format!(
            "{action} on the workspace's events ends at once: {other:?}"
        ))),
    }
}

/// The person's events, as a device of their own reads them from the server.
fn listed(up: &Up, workspace: &str, me: &Caller) -> serde_json::Value {
    up.take(
        workspace,
        me,
        workspace_events::LIST,
        &serde_json::json!({}),
    )["events"]
        .clone()
}

/// One action on what the person is told and on their own rule, as their own device takes it
/// from the server. The device is on for that action only.
async fn on_their_device(
    up: &Up,
    (browser, device): (&Device, &Device),
    workspace: &str,
    (resource, action): (&str, &str),
    arguments: serde_json::Value,
) -> serde_json::Value {
    let mut page = Browser::open_at(&up.hosted, browser, device, workspace).await;
    let argument = match arguments {
        serde_json::Value::Object(arguments) => InvokeArgument::Json(arguments),
        _ => InvokeArgument::None,
    };
    let answered = page
        .invoke_on(
            person_telling::PROVIDER,
            tilo_actions::api::ActionAddress::new(resource, action),
            argument,
        )
        .await
        .expect("the server answers the person's own device")
        .value
        .unwrap_or_default();
    page.peer.close();
    answered
}

/// What the person finds they were told, as their own device reads it once it is on again.
async fn told_to(up: &Up, devices: (&Device, &Device), workspace: &str) -> Vec<Told> {
    let listed = on_their_device(
        up,
        devices,
        workspace,
        (person_telling::TOLD, person_telling::LIST),
        serde_json::Value::Null,
    )
    .await;
    serde_json::from_value::<ToldList>(listed)
        .expect("a list of what a person was told")
        .told
}

/// The message the event its person calls `label` asked the assistant with, once the
/// conversation holds it.
async fn asked_as(up: &Up, workspace: &str, label: &str) -> Id {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let part = conversation_part(&up.workspaces, workspace).await;
        let asked = part.messages.iter().find(|(_, held)| {
            held.cause
                .as_ref()
                .is_some_and(|cause| cause.label == label)
        });
        if let Some((id, _)) = asked {
            return id.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the event {label:?} never asked the assistant"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The person's assistant, `theirs`, sets an event called `name` up for a moment from now, told
/// as `told_as` where it says so. The moment comes with nothing of the person's on, and their
/// assistant answers the turn it starts. The event as setting it up answered it.
async fn fired(
    up: &Up,
    workspace: &str,
    theirs: &Caller,
    name: &str,
    told_as: Option<&str>,
) -> serde_json::Value {
    let mut set_up = serde_json::json!({
        "name": name,
        "when": in_a_moment(1_200).0,
        "instruction": "Say what is on for the day.",
    });
    if let Some(told_as) = told_as {
        set_up["toldAs"] = serde_json::json!(told_as);
    }
    let event = up.take(workspace, theirs, workspace_events::SET_UP, &set_up);
    let ask = asked_as(up, workspace, name).await;
    let model = waits(
        vec![says(SAID)],
        usize::MAX,
        &Arc::new(Mutex::new(Vec::new())),
        &Arc::new(Hold::default()),
    );
    let agents = up.agents(workspace, Some(model)).await;
    let turns = agents.turns(workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn is owed: {turns:?}");
    agents.answer(workspace, &turns[0]).await.unwrap();
    let reply = ended(&up.workspaces, workspace, &ask).await;
    assert!(
        reply.text.contains(SAID),
        "the assistant answered {name:?}: {}",
        reply.text
    );
    event
}

/// What a thing told says of itself: its kind, the kind its cause names, whether it reaches
/// its person, and why it carries its mark.
fn said_of(told: &Told) -> (Kind, Option<&str>, bool, WhyCode) {
    (
        told.kind,
        told.cause.as_ref().map(|cause| cause.kind.as_str()),
        told.reaches,
        told.mark.why.code,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_event_told_as_a_brief_raises_a_briefs_notice_and_one_that_says_nothing_is_told_as_before()
 {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (up, workspace, page) = team_on(&root, (&device, &browser), state.path()).await;
    let ada = root.verifying_key().to_bytes();
    let me = themselves(&root, &browser, &workspace);
    let theirs = super::their_agent(&me, SCOUT).named("Scout");
    // Nothing of the person's is on from here until they come back.
    page.peer.close();

    // Their assistant sets the same event up three times: told as a brief, told as news, and
    // saying nothing. Each fires, and the assistant answers each with the same words.
    let brief = fired(
        &up,
        &workspace,
        &theirs,
        "The day, as a brief",
        Some("brief"),
    )
    .await;
    let news = fired(&up, &workspace, &theirs, "The day, as news", Some("news")).await;
    let plain = fired(&up, &workspace, &theirs, "The day", None).await;
    assert_eq!(brief["toldAs"], "brief", "{brief}");
    assert_eq!(news["toldAs"], "news", "{news}");
    assert!(
        plain.get("toldAs").is_none(),
        "an event that says nothing reads as before: {plain}"
    );
    // Their person reads the same of each, as the events are theirs.
    let theirs_read: Vec<Option<String>> = listed(&up, &workspace, &me)
        .as_array()
        .expect("the events are listed")
        .iter()
        .map(|event| event["toldAs"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        theirs_read,
        [Some("brief".to_owned()), Some("news".to_owned()), None]
    );

    // With every device of theirs off, the server tells outside what is to reach them: the
    // brief, by what it is, and the event that says nothing, which is a moment and so a
    // reminder as before. The news it tells nobody outside.
    let outside = told_outside(&up, 2).await;
    assert_eq!(
        outside,
        [
            (ada, "The day, as a brief".to_owned(), Kind::Brief),
            (ada, "The day".to_owned(), Kind::Reminder),
        ],
        "a brief and a reminder raise their notice, each once, and news none"
    );

    // The person comes back and reads what they were told: the same words three times, each
    // thing as its event said.
    let told = told_to(&up, (&browser, &device), &workspace).await;
    let [as_brief, as_news, as_before] = told.as_slice() else {
        panic!("one thing for each event that fired: {told:?}");
    };
    for thing in &told {
        assert!(thing.words.contains(SAID), "{thing:?}");
        assert_eq!(
            thing.from.as_ref().map(|from| from.agent.as_str()),
            Some(SCOUT),
            "their assistant told it"
        );
    }
    assert_eq!(
        (as_brief.title.as_str(), said_of(as_brief)),
        (
            "The day, as a brief",
            (Kind::Brief, Some("brief"), true, WhyCode::Brief)
        ),
        "a brief reaches its person as what it is, and nobody judged it"
    );
    assert_eq!(
        (as_news.title.as_str(), said_of(as_news)),
        (
            "The day, as news",
            (Kind::News, Some("event"), false, WhyCode::NoRule)
        ),
        "news is left to its person's rule, and with no rule it does not reach them"
    );
    assert_eq!(
        (as_before.title.as_str(), said_of(as_before)),
        (
            "The day",
            (Kind::Reminder, Some("reminder"), true, WhyCode::Reminder)
        ),
        "an event that says nothing is told as before"
    );
    let pushed: Vec<Vec<&str>> = told
        .iter()
        .map(|thing| thing.pushed.iter().map(|push| push.by.as_str()).collect())
        .collect();
    assert_eq!(
        pushed,
        [vec!["web-push"], vec![], vec!["web-push"]],
        "that it was told outside is written on the thing"
    );
    up.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn how_an_events_result_is_told_is_said_only_where_an_event_is_set_up_at_all() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (up, workspace, page) = team_on(&root, (&device, &browser), state.path()).await;
    page.peer.close();
    let ada = root.verifying_key().to_bytes();
    let me = themselves(&root, &browser, &workspace);
    let theirs = super::their_agent(&me, SCOUT).named("Scout");
    let far = "once at 2031-01-01T09:00:00Z";

    // Their assistant sets an event up that says nothing.
    let event = up.take(
        &workspace,
        &theirs,
        workspace_events::SET_UP,
        &serde_json::json!({"name": "New year", "when": far, "instruction": "Say happy new year."}),
    );
    let id = event["id"].as_str().expect("an event has an id").to_owned();
    assert!(event.get("toldAs").is_none(), "{event}");

    // Whoever sets no event up here says nothing of how one is told either: another person,
    // an agent that acts for them, and a bot its person installed set nothing up with the
    // word and change nothing of this person's event.
    let (bo, _) = tilo_user_identity::UserIdentity::generate("Bo", 2);
    let another =
        Caller::authenticated(bo.verifying_key().to_bytes(), [9; 32]).in_workspace(&workspace);
    // A person and the agent acting for them are refused as whoever the workspace's policy
    // gives no reach to its events; a bot, which asks for no person, is told there is no such
    // action.
    let others = [
        ("another person", another.clone(), "not permitted"),
        (
            "another person's agent",
            super::their_agent(&another, "helper").named("Helper"),
            "not permitted",
        ),
        (
            "a bot",
            Caller::bot("greeter", ada, [5; 32]).in_workspace(&workspace),
            "unknown action",
        ),
    ];
    for (who, other, refused) in &others {
        let set_up = tried(
            &up,
            &workspace,
            other,
            workspace_events::SET_UP,
            &serde_json::json!({
                "name": "Loud",
                "when": far,
                "instruction": "Say hello.",
                "toldAs": "brief",
            }),
        );
        assert_eq!(
            refusal(&set_up),
            *refused,
            "{who} sets nothing up: {set_up:?}"
        );
        let changed = tried(
            &up,
            &workspace,
            other,
            workspace_events::CHANGE,
            &serde_json::json!({"event": id, "toldAs": "brief"}),
        );
        assert_eq!(
            refusal(&changed),
            *refused,
            "{who} changes nothing: {changed:?}"
        );
    }
    assert_eq!(
        listed(&up, &workspace, &me),
        serde_json::json!([event]),
        "nothing was set up, and nothing of the event changed"
    );

    // Their assistant says it of their own event, and they read the same on a device of theirs.
    let said = |caller: &Caller, change: serde_json::Value| {
        let mut named = serde_json::json!({"event": id});
        for (key, value) in change.as_object().cloned().unwrap_or_default() {
            named[key.as_str()] = value;
        }
        up.take(&workspace, caller, workspace_events::CHANGE, &named)
    };
    let changed = said(&theirs, serde_json::json!({"toldAs": "brief"}));
    assert_eq!(
        (changed["id"].as_str(), changed["toldAs"].as_str()),
        (Some(id.as_str()), Some("brief")),
        "it is the same event, told as a brief from now on"
    );
    assert_eq!(listed(&up, &workspace, &me)[0]["toldAs"], "brief");
    // What a change leaves out stays, the word too.
    let renamed = said(&theirs, serde_json::json!({"name": "The new year"}));
    assert_eq!(
        (renamed["name"].as_str(), renamed["toldAs"].as_str()),
        (Some("The new year"), Some("brief"))
    );
    // The person changes it themselves.
    let quieter = said(&me, serde_json::json!({"toldAs": "news"}));
    assert_eq!(quieter["toldAs"], "news");
    // A word that names no way an event's result is told is refused in words and changes
    // nothing. A question is none: no event makes one of what its work answers.
    for wrong in ["loud", "question", "important"] {
        let refused = tried(
            &up,
            &workspace,
            &theirs,
            workspace_events::CHANGE,
            &serde_json::json!({"event": id, "toldAs": wrong}),
        );
        assert!(
            matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains("`brief`")),
            "{wrong}: {refused:?}"
        );
    }
    assert_eq!(listed(&up, &workspace, &me)[0]["toldAs"], "news");
    // An empty word takes it away: the event says nothing again.
    let plain = said(&me, serde_json::json!({"toldAs": ""}));
    assert!(plain.get("toldAs").is_none(), "{plain}");

    // A server that starts again keeps the word with the event.
    said(&theirs, serde_json::json!({"toldAs": "brief"}));
    up.down().await;
    let up = Up::on(&root, &device, state.path()).await;
    let kept = listed(&up, &workspace, &me);
    assert_eq!(
        (kept[0]["id"].as_str(), kept[0]["toldAs"].as_str()),
        (Some(id.as_str()), Some("brief")),
        "{kept}"
    );
    up.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_events_brief_reaches_its_own_person_and_nobody_else() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (up, workspace, page) = team_on(&root, (&device, &browser), state.path()).await;
    page.peer.close();
    let ada = root.verifying_key().to_bytes();
    let me = themselves(&root, &browser, &workspace);
    let theirs = super::their_agent(&me, SCOUT).named("Scout");
    let (bo, _) = tilo_user_identity::UserIdentity::generate("Bo", 2);
    let bo = bo.verifying_key().to_bytes();

    // An event is of the person its asker is or acts for. None of what describes one names a
    // person, so an assistant sets no brief up for anybody else: one that tries is refused
    // whole, and nothing is set up.
    let aimed = tried(
        &up,
        &workspace,
        &theirs,
        workspace_events::SET_UP,
        &serde_json::json!({
            "name": "For Bo",
            "when": in_a_moment(600).0,
            "instruction": "Say what is on for the day.",
            "toldAs": "brief",
            "person": hex::encode(bo),
        }),
    );
    assert!(
        matches!(aimed, Err(ActionCallError::InvalidRequest { .. })),
        "{aimed:?}"
    );
    // Neither does an agent that acts for that other person set one up here.
    let helper = Caller::delegated_agent("helper", bo, [9; 32])
        .in_workspace(&workspace)
        .named("Helper");
    let theirs_too = tried(
        &up,
        &workspace,
        &helper,
        workspace_events::SET_UP,
        &serde_json::json!({
            "name": "From Bo",
            "when": in_a_moment(600).0,
            "instruction": "Say what is on for the day.",
            "toldAs": "brief",
        }),
    );
    assert_eq!(refusal(&theirs_too), "not permitted", "{theirs_too:?}");
    assert_eq!(listed(&up, &workspace, &me), serde_json::json!([]));

    // The assistant's own event, told as a brief, fires and is answered.
    fired(
        &up,
        &workspace,
        &theirs,
        "The day, as a brief",
        Some("brief"),
    )
    .await;

    // The server tells one person outside, the one whose event it is, and keeps a list of
    // what was told for nobody else.
    let outside = told_outside(&up, 1).await;
    assert_eq!(
        outside,
        [(ada, "The day, as a brief".to_owned(), Kind::Brief)]
    );
    assert_eq!(up.workspaces.people_told(), [ada]);
    assert!(
        up.workspaces.told_after(&bo, 0).told.is_empty(),
        "nobody else was told anything"
    );
    let told = told_to(&up, (&browser, &device), &workspace).await;
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(
        said_of(&told[0]),
        (Kind::Brief, Some("brief"), true, WhyCode::Brief)
    );
    up.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_breaks_do_not_disturb_stays_its_persons_own_to_say_above_the_word() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (up, workspace, page) = team_on(&root, (&device, &browser), state.path()).await;
    page.peer.close();
    let me = themselves(&root, &browser, &workspace);
    let theirs = super::their_agent(&me, SCOUT).named("Scout");

    // A brief reaches its person. Nothing they said lets a brief break do-not-disturb, so
    // their devices hold it back while they are quiet: the word does not say that it may.
    fired(&up, &workspace, &theirs, "The day, first", Some("brief")).await;

    // They say so themselves, on a device of their own: a brief may break do-not-disturb.
    on_their_device(
        &up,
        (&browser, &device),
        &workspace,
        (person_telling::IMPORTANCE, person_telling::LET_THROUGH),
        serde_json::json!({"kind": "brief", "may": true}),
    )
    .await;
    fired(&up, &workspace, &theirs, "The day, second", Some("brief")).await;
    // An event told as news is left to their rule on what is important. They said nothing
    // is, so it does not reach them, and breaks nothing.
    fired(&up, &workspace, &theirs, "The day, as news", Some("news")).await;

    let told = told_to(&up, (&browser, &device), &workspace).await;
    let [first, second, news] = told.as_slice() else {
        panic!("one thing for each event that fired: {told:?}");
    };
    assert_eq!(
        (said_of(first), first.through_quiet),
        ((Kind::Brief, Some("brief"), true, WhyCode::Brief), false),
        "a brief reaches its person and waits out their do-not-disturb"
    );
    assert_eq!(
        (said_of(second), second.through_quiet),
        ((Kind::Brief, Some("brief"), true, WhyCode::Brief), true),
        "it breaks do-not-disturb once its person said a brief may"
    );
    assert_eq!(
        (said_of(news), news.through_quiet),
        ((Kind::News, Some("event"), false, WhyCode::NoRule), false),
        "news reaches its person only where their own rule calls it important"
    );
    assert!(
        !first.mark.important && !second.mark.important && !news.mark.important,
        "no word of an event marks a thing important: {told:?}"
    );
    up.down().await;
}
