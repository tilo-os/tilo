//! What a person's assistant tells them: each thing's mark says what kind
//! of thing it is and nothing reaches a person but a question, a reminder, a
//! brief or a mention; a thing that must not be missed is raised again
//! until they acknowledge it; and the log is the person's alone.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host a test binds is kept until the test ends; dropping it earlier closes what it drives"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::Duration;

use crate::person_telling::{self, Cause, Kind, Offered, OfferedTarget, Teller, Told, ToldList};
use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, ReceiptStatus, WORKSPACE_GOVERNANCE_PROVIDER,
};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use crate::ToTell;
use crate::fixtures::TestHost;
use crate::fixtures::guest_device;
use crate::fixtures::{caller, settled};
use crate::fixtures::{create_own, let_in, open_home as open};
use crate::fixtures::{host_dispatch, hosting_device, test_host};

/// The id and name Ada's assistant is known by.
pub(super) const ASSISTANT: &str = "assistant";

/// Ada's setup: a host that keeps workspaces of people's own, a device of
/// hers, the workspace that is her own, and one she shares with others.
pub(super) struct Setup {
    pub(super) workspaces: crate::PersonServices,
    pub(super) host: tilo_workspace_service::workspaces::actions::ServiceWorkspaceActions,
    pub(super) ada: DeviceCert,
    pub(super) own: WorkspaceId,
    pub(super) shared: WorkspaceId,
    _server: TestHost,
    _dir: tempfile::TempDir,
}

impl Setup {
    pub(super) async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = &crate::PersonServices::of(server.workspaces());
        let host = host_dispatch(workspaces);
        let (_, ada) = guest_device();
        let home = open(workspaces).await;
        let_in(workspaces, &home, ada.user_public_key).await;
        let created = create_own(&host, &ada, json!({"name": "Ada's workspace"}))
            .await
            .unwrap();
        let own = WorkspaceId::parse(&created.workspace.workspace).unwrap();
        assert!(workspaces.owns(&ada.user_public_key, &own.to_string()));
        let shared = workspaces.create("Team").await.unwrap().workspace_id;
        assert!(!workspaces.owns(&ada.user_public_key, &shared.to_string()));
        Self {
            workspaces: workspaces.clone(),
            host,
            ada,
            own,
            shared,
            _server: server,
            _dir: dir,
        }
    }

    /// The page `device` holds in `workspace`, which a caller asks over.
    pub(super) fn page(
        &self,
        workspace: WorkspaceId,
        device: &DeviceCert,
    ) -> impl ActionProvider + Clone + Send + 'static {
        self.host.device_page(workspace, device)
    }
}

/// The agent a person keeps, over the connection `device` certified.
pub(super) fn agent_of(device: &DeviceCert) -> Caller {
    Caller::delegated_agent(ASSISTANT, device.user_public_key, device.device_public_key)
}

/// `answer` is the one answer for unknown and forbidden.
pub(super) fn nobody(what: &str, answer: Result<Value, ActionCallError>) {
    let refused = answer.expect_err(what);
    assert!(
        matches!(refused, ActionCallError::UnknownAction),
        "{what}: {refused:?}"
    );
}

/// A mail as the assistant tells it.
const MAIL: &str = "Kim at Harbor asks whether we take new customers.";

/// What `caller` is answered about what was told, over the page `device`
/// holds in `workspace`.
async fn ask_told(
    setup: &Setup,
    (workspace, device): (WorkspaceId, &DeviceCert),
    caller: &Caller,
    action: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    let page = setup.host.device_page(workspace, device);
    let mut request = crate::fixtures::asking(action, argument);
    request.target = person_telling::target(person_telling::TOLD, action);
    let receipt = settled(page.invoke(caller, request)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("the host answers, not {other:?}"),
    }
}

/// Ada's agent tells her `words`, caused by `cause`, from her own
/// workspace.
async fn told_by_agent(setup: &Setup, words: &str, cause: Value) -> Told {
    let answer = ask_told(
        setup,
        (setup.own, &setup.ada),
        &agent_of(&setup.ada),
        person_telling::TELL,
        json!({"words": words, "cause": cause}),
    )
    .await;
    serde_json::from_value(answer.expect("the host tells")).expect("a thing told")
}

/// What this server itself tells Ada as it carries out an event of her
/// own, a reminder she set up or a brief she asked for: of the kind that
/// event's own `cause` says, with her assistant named as its teller.
fn told_by_her_event(setup: &Setup, words: &str, cause: Value, insists: bool) -> Told {
    let cause: Cause = serde_json::from_value(cause).expect("a cause");
    let telling = ToTell {
        kind: Kind::of_cause(Some(&cause)),
        words: words.to_owned(),
        cause: Some(cause),
        insists,
        from: Some(Teller {
            agent: ASSISTANT.to_owned(),
            name: ASSISTANT.to_owned(),
        }),
        ..ToTell::default()
    };
    setup
        .workspaces
        .tell(&setup.ada.user_public_key, telling)
        .expect("the server tells")
}

/// What Ada reads of her log after place `after`, on her device.
async fn listed(setup: &Setup, after: u64) -> ToldList {
    let answer = ask_told(
        setup,
        (setup.own, &setup.ada),
        &caller(&setup.ada),
        person_telling::LIST,
        json!({"after": after}),
    )
    .await;
    serde_json::from_value(answer.expect("the host lists")).expect("a list")
}

/// An event that hands the assistant a mail, by the event's own key for it.
fn event(key: &str) -> Value {
    json!({"kind": "event", "label": "Mail arrives", "key": key})
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "every kind of caller that is not the person, each asked to tell and to read"
)]
async fn what_a_person_was_told_is_theirs_alone() {
    let setup = Setup::new().await;
    let told = told_by_agent(&setup, MAIL, event("mail-1")).await;
    assert_eq!(listed(&setup, 0).await.told, [told]);

    // Her agent tells her, from a workspace she shares as well. It does
    // not read what she was told, and she tells herself nothing through
    // it.
    for workspace in [setup.own, setup.shared] {
        nobody(
            "an agent reading what its person was told",
            ask_told(
                &setup,
                (workspace, &setup.ada),
                &agent_of(&setup.ada),
                person_telling::LIST,
                json!({}),
            )
            .await,
        );
    }
    ask_told(
        &setup,
        (setup.shared, &setup.ada),
        &agent_of(&setup.ada),
        person_telling::TELL,
        json!({"words": "The team's plan changed."}),
    )
    .await
    .expect("her agent tells her from a shared workspace");
    nobody(
        "a person telling themselves",
        ask_told(
            &setup,
            (setup.own, &setup.ada),
            &caller(&setup.ada),
            person_telling::TELL,
            json!({"words": "A note to myself."}),
        )
        .await,
    );
    assert_eq!(listed(&setup, 0).await.told.len(), 2);

    // Another member reads their own log, which holds nothing of hers, and
    // what their agent tells is told to them.
    let (_, bo) = guest_device();
    for workspace in [setup.shared, setup.own] {
        let theirs: ToldList = serde_json::from_value(
            ask_told(
                &setup,
                (workspace, &bo),
                &caller(&bo),
                person_telling::LIST,
                json!({}),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert!(theirs.told.is_empty(), "{theirs:?}");
    }
    ask_told(
        &setup,
        (setup.shared, &bo),
        &agent_of(&bo),
        person_telling::TELL,
        json!({"words": "For Bo."}),
    )
    .await
    .expect("another member's agent tells that member");
    assert_eq!(listed(&setup, 0).await.told.len(), 2);

    // An agent that says it acts for Ada over a device that is not hers
    // is nobody, and so are an agent no device stands behind, an app, and
    // a bot installed for her.
    let pretending = (
        (setup.shared, &bo),
        Caller::delegated_agent(ASSISTANT, setup.ada.user_public_key, bo.device_public_key),
    );
    let ada = (setup.own, &setup.ada);
    let strangers = [
        (
            "an agent over a device that is not its person's",
            pretending,
        ),
        (
            "an agent no device stands behind",
            (ada, Caller::local_agent(ASSISTANT, "ada")),
        ),
        ("an app", (ada, Caller::local_app("so.tilo.chat"))),
        (
            "a bot installed for her",
            (
                ada,
                Caller::bot(
                    "a-bot",
                    setup.ada.user_public_key,
                    setup.ada.device_public_key,
                ),
            ),
        ),
    ];
    for (what, (page, stranger)) in &strangers {
        nobody(
            what,
            ask_told(
                &setup,
                *page,
                stranger,
                person_telling::TELL,
                json!({"words": "From someone else."}),
            )
            .await,
        );
        nobody(
            what,
            ask_told(&setup, *page, stranger, person_telling::LIST, json!({})).await,
        );
    }
    assert_eq!(listed(&setup, 0).await.told.len(), 2);
}

#[tokio::test]
async fn a_device_that_waits_is_answered_when_something_is_told() {
    let setup = Setup::new().await;
    let ada = (setup.own, &setup.ada);

    // Her device asks with a wait, and is answered the moment her agent
    // tells her something.
    let started = tilo_clock::now();
    let her = caller(&setup.ada);
    let waiting = ask_told(
        &setup,
        ada,
        &her,
        person_telling::LIST,
        json!({"after": 0, "waitMs": 8000}),
    );
    let telling = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        told_by_agent(&setup, MAIL, event("mail-1")).await
    };
    let (heard, told) = tokio::join!(waiting, telling);
    let heard: ToldList = serde_json::from_value(heard.unwrap()).unwrap();
    assert_eq!(heard.told, [told]);
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "answered when told, not when the wait ran out"
    );

    // With nothing newer the wait runs out, and the answer says so.
    let started = tilo_clock::now();
    let nothing: ToldList = serde_json::from_value(
        ask_told(
            &setup,
            ada,
            &caller(&setup.ada),
            person_telling::LIST,
            json!({"after": heard.newest, "waitMs": 200}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(nothing.told.is_empty());
    assert_eq!(nothing.newest, heard.newest);
    assert!(started.elapsed() >= Duration::from_millis(150));
}

/// A reminder Ada set up, told her as its time comes: a thing that reaches
/// her and waits.
fn reminded(setup: &Setup) -> Told {
    told_by_her_event(
        setup,
        "Call Jo.",
        json!({"kind": "reminder", "label": "Call Jo", "key": "r-1"}),
        false,
    )
}

#[tokio::test]
async fn only_the_person_acknowledges_and_the_first_time_stands() {
    let setup = Setup::new().await;
    let ada = (setup.own, &setup.ada);
    let reminder = reminded(&setup);
    assert!(reminder.waits());

    // Her agent told it. Whether she saw it is not her agent's to say,
    // wherever it asks from.
    for workspace in [setup.own, setup.shared] {
        nobody(
            "an agent acknowledging for its person",
            ask_told(
                &setup,
                (workspace, &setup.ada),
                &agent_of(&setup.ada),
                person_telling::ACKNOWLEDGE,
                json!({"told": reminder.id}),
            )
            .await,
        );
    }
    // Another member who names her thing reaches nothing: their own log
    // holds nothing of that id.
    let (_, bo) = guest_device();
    let theirs = ask_told(
        &setup,
        (setup.shared, &bo),
        &caller(&bo),
        person_telling::ACKNOWLEDGE,
        json!({"told": reminder.id}),
    )
    .await;
    assert!(
        matches!(theirs, Err(ActionCallError::Refused { .. })),
        "{theirs:?}"
    );
    assert!(listed(&setup, 0).await.told[0].waits());

    // She puts it away on her phone: it waits no more, says where and how,
    // and takes a new place so each device of hers hears of it.
    let seen: Told = serde_json::from_value(
        ask_told(
            &setup,
            ada,
            &caller(&setup.ada),
            person_telling::ACKNOWLEDGE,
            json!({"told": reminder.id, "device": "Ada's phone", "by": "dismissed"}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(!seen.waits());
    let acknowledged = seen.acknowledged.clone().unwrap();
    assert_eq!(
        (acknowledged.device.as_str(), acknowledged.by.as_str()),
        ("Ada's phone", "dismissed")
    );
    assert!(seen.seq > reminder.seq);
    assert_eq!(
        listed(&setup, reminder.seq).await.told,
        std::slice::from_ref(&seen)
    );

    // Opening it later on another device changes nothing: the first stands.
    let again: Told = serde_json::from_value(
        ask_told(
            &setup,
            ada,
            &caller(&setup.ada),
            person_telling::ACKNOWLEDGE,
            json!({"told": reminder.id, "device": "Ada's laptop"}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(again, seen);
    // And a thing nobody told her is not acknowledged into being.
    let nothing = ask_told(
        &setup,
        ada,
        &caller(&setup.ada),
        person_telling::ACKNOWLEDGE,
        json!({"told": "nothing"}),
    )
    .await;
    assert!(
        matches!(nothing, Err(ActionCallError::Refused { .. })),
        "{nothing:?}"
    );
}

#[tokio::test]
async fn a_device_that_asks_is_looking_for_as_long_as_it_waits() {
    let setup = Setup::new().await;
    let ada = setup.ada.user_public_key;
    let lately = Duration::from_secs(8);
    assert!(!setup.workspaces.looking(&ada, lately));

    // Reading her log is looking, and only hers.
    listed(&setup, 0).await;
    assert!(setup.workspaces.looking(&ada, lately));
    let (_, bo) = guest_device();
    assert!(!setup.workspaces.looking(&bo.user_public_key, lately));
    // A look is over once it is longer ago than asked about.
    tokio::time::sleep(Duration::from_millis(80)).await;
    let just_now = Duration::from_millis(40);
    assert!(!setup.workspaces.looking(&ada, just_now));

    // A device that waits for the log to move looks until its wait is
    // over, however lately that began.
    let her = caller(&setup.ada);
    let waiting = ask_told(
        &setup,
        (setup.own, &setup.ada),
        &her,
        person_telling::LIST,
        json!({"after": 99, "waitMs": 400}),
    );
    let during = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        setup.workspaces.looking(&ada, just_now)
    };
    let (heard, looking) = tokio::join!(waiting, during);
    assert!(looking, "a device waiting for news is looking");
    let heard: ToldList = serde_json::from_value(heard.unwrap()).unwrap();
    assert!(heard.told.is_empty());
}

#[tokio::test]
async fn whoever_tells_outside_reads_who_changed_and_writes_down_what_it_told() {
    let setup = Setup::new().await;
    let ada = setup.ada.user_public_key;
    assert!(setup.workspaces.people_told().is_empty());
    assert!(setup.workspaces.told_changed_people().is_empty());

    let reminder = reminded(&setup);
    assert_eq!(setup.workspaces.people_told(), [ada]);
    assert_eq!(setup.workspaces.told_changed_people(), [ada]);
    assert!(
        setup.workspaces.told_changed_people().is_empty(),
        "a change is handed over once"
    );

    // What was told outside is written down on the thing, which keeps its
    // place: a device has nothing new to show for it.
    let pushed = setup
        .workspaces
        .told_outside(&ada, &reminder.id, "web-push", 2)
        .unwrap();
    assert_eq!(pushed.seq, reminder.seq);
    assert_eq!(pushed.pushed.len(), 1);
    assert_eq!(
        (pushed.pushed[0].by.as_str(), pushed.pushed[0].took),
        ("web-push", 2)
    );
    assert!(setup.workspaces.told_changed_people().is_empty());
    assert_eq!(listed(&setup, 0).await.told, [pushed]);

    // What is taken back waits no more, and every device hears of it.
    let gone = setup.workspaces.withdraw_told(&ada, &reminder.id).unwrap();
    assert!(!gone.waits() && gone.withdrawn_at_ms.is_some());
    assert!(gone.seq > reminder.seq);
    assert_eq!(setup.workspaces.told_changed_people(), [ada]);
    assert!(
        setup
            .workspaces
            .told_outside(&ada, "nothing", "web-push", 1)
            .is_err(),
        "nothing is written down for a thing nobody told"
    );
}

#[tokio::test]
async fn a_person_who_comes_back_reads_what_happened_while_they_were_away() {
    let setup = Setup::new().await;
    let ada = setup.ada.user_public_key;
    let own = (setup.own, &setup.ada);
    // A device of hers looked half an hour ago, and none since.
    let half_an_hour = 30 * 60 * 1000;
    setup
        .workspaces
        .device_looked_at(&ada, tilo_clock::unix_now_ms().saturating_sub(half_an_hour));

    // Meanwhile: an event's turn told what it did alone, something came
    // in, a reminder fell due, and a question waits for her.
    let did = told_by_agent(&setup, "I answered Kim in the usual way.", event("mail-1")).await;
    let came = a_told(
        ask_told(
            &setup,
            own,
            &agent_of(&setup.ada),
            person_telling::TELL,
            json!({"words": "Harbor posted new prices.", "cause": {"kind": "competitors", "key": "c-1"}}),
        )
        .await,
    );
    let reminder = reminded(&setup);
    let question = setup
        .workspaces
        .tell(
            &ada,
            ToTell {
                id: Some("question:m1".into()),
                kind: Kind::Question,
                words: "Two versions of the deck exist. Which one?".into(),
                ..ToTell::default()
            },
        )
        .unwrap();

    // Her device asks, which is her coming back.
    let ask = || async {
        let answer = ask_told(
            &setup,
            own,
            &caller(&setup.ada),
            person_telling::SINCE_YOU_LEFT,
            json!({}),
        )
        .await;
        serde_json::from_value::<person_telling::Stretch>(answer.unwrap()).unwrap()
    };
    let stretch = ask().await;
    assert!(stretch.left_at_ms.is_some() && stretch.back_at_ms > stretch.left_at_ms);
    assert_eq!(stretch.did_alone, std::slice::from_ref(&did));
    assert_eq!(stretch.came_in, [came.clone(), reminder.clone()]);
    assert_eq!(stretch.waits, std::slice::from_ref(&question));

    // What she answers waits no more, and the stretch stands while she
    // stays: asking again is no new coming back.
    setup
        .workspaces
        .acknowledge_told(&ada, &question.id, "Ada's laptop", "answered")
        .unwrap();
    let later = ask().await;
    assert_eq!(
        (later.left_at_ms, later.back_at_ms),
        (stretch.left_at_ms, stretch.back_at_ms)
    );
    assert!(later.waits.is_empty());
    assert_eq!(later.came_in.len(), 3);

    // It is hers alone: her agent is not answered, and another member
    // reads their own, which holds nothing.
    nobody(
        "an agent reading what happened while its person was away",
        ask_told(
            &setup,
            own,
            &agent_of(&setup.ada),
            person_telling::SINCE_YOU_LEFT,
            json!({}),
        )
        .await,
    );
    let (_, bo) = guest_device();
    let theirs: person_telling::Stretch = serde_json::from_value(
        ask_told(
            &setup,
            (setup.shared, &bo),
            &caller(&bo),
            person_telling::SINCE_YOU_LEFT,
            json!({}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(theirs, person_telling::Stretch::default());
}

/// What `answer` carries, read as a thing told.
fn a_told(answer: Result<Value, ActionCallError>) -> Told {
    serde_json::from_value(answer.expect("the host answers")).expect("a thing told")
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "each way a thing is made to insist, by its teller and by its person"
)]
async fn only_its_person_makes_a_quiet_thing_insist() {
    let setup = Setup::new().await;
    let own = (setup.own, &setup.ada);
    let agent = agent_of(&setup.ada);
    let her = caller(&setup.ada);

    // News is not made loud by whoever tells it: neither told as not to be
    // missed, nor marked so afterwards.
    let quiet = a_told(
        ask_told(
            &setup,
            own,
            &agent,
            person_telling::TELL,
            json!({"words": MAIL, "cause": event("mail-1"), "insists": true}),
        )
        .await,
    );
    assert!(!quiet.reaches && quiet.insists.is_none());
    let refused = ask_told(
        &setup,
        own,
        &agent,
        person_telling::INSIST,
        json!({"told": quiet.id, "insists": true}),
    )
    .await;
    assert!(
        matches!(refused, Err(ActionCallError::Refused { .. })),
        "{refused:?}"
    );
    assert!(!listed(&setup, 0).await.told[0].reaches);

    // A reminder she said must not be missed insists from the moment it is
    // told: every ten minutes, six times at most.
    let must = told_by_her_event(
        &setup,
        "File the tax return.",
        json!({"kind": "reminder", "label": "Tax return", "key": "tax-1"}),
        true,
    );
    let insists = must.insists.unwrap();
    assert_eq!(
        (insists.every_secs, insists.times, insists.raised),
        (600, 6, 1)
    );
    assert_eq!(
        insists.next_at_ms,
        Some(must.at_ms.saturating_add(600 * 1000))
    );
    // Her agent marks a thing that reaches her as not to be missed, when
    // she says so of it later.
    let reminder = reminded(&setup);
    let marked = a_told(
        ask_told(
            &setup,
            own,
            &agent,
            person_telling::INSIST,
            json!({"told": reminder.id, "insists": true}),
        )
        .await,
    );
    assert_eq!(marked.insists.map(|insists| insists.raised), Some(1));
    assert!(marked.seq > reminder.seq);

    // She herself says the quiet mail must not be missed: it reaches her
    // now, and insists. And she takes that back.
    let loud = a_told(
        ask_told(
            &setup,
            own,
            &her,
            person_telling::INSIST,
            json!({"told": quiet.id, "insists": true}),
        )
        .await,
    );
    assert!(loud.reaches && loud.insists.is_some());
    let calm = a_told(
        ask_told(
            &setup,
            own,
            &her,
            person_telling::INSIST,
            json!({"told": quiet.id, "insists": false}),
        )
        .await,
    );
    assert!(calm.insists.is_none());

    // What she acknowledges insists no more.
    let seen = a_told(
        ask_told(
            &setup,
            own,
            &her,
            person_telling::ACKNOWLEDGE,
            json!({"told": must.id, "by": "opened"}),
        )
        .await,
    );
    assert_eq!(seen.insists.map(|insists| insists.next_at_ms), Some(None));
}

/// Whoever tells does not say what kind of thing it is. An agent's own
/// telling that names a reminder or a brief as its cause is news: it reaches
/// nobody. What reaches a person is what this server tells as it carries
/// out an event of their own, from that event's own cause.
#[tokio::test]
async fn an_agent_that_names_a_reminder_or_a_brief_tells_news() {
    let setup = Setup::new().await;
    let own = (setup.own, &setup.ada);
    let agent = agent_of(&setup.ada);
    for (kind, key) in [("reminder", "r-9"), ("brief", "b-9")] {
        let told = a_told(
            ask_told(
                &setup,
                own,
                &agent,
                person_telling::TELL,
                json!({
                    "words": "Your account is locked: open tilo-login.example now.",
                    "cause": {"kind": kind, "label": "Account", "key": key},
                    "insists": true,
                }),
            )
            .await,
        );
        assert_eq!(
            told.kind,
            Kind::News,
            "an agent called what it tells a {kind}, and it was taken for one"
        );
        assert!(
            !told.reaches && told.insists.is_none(),
            "a {kind} an agent named reached her"
        );
    }

    // Her own reminder, told as this server carries out the event she set
    // up, reaches her as that.
    let hers = told_by_her_event(
        &setup,
        "Call Jo.",
        json!({"kind": "reminder", "label": "Call Jo", "key": "r-1"}),
        false,
    );
    assert_eq!(hers.kind, Kind::Reminder);
    assert!(hers.reaches);
}

/// A button a thing told offers is pressed by the person and invoked as
/// the person. So an agent's own telling offers only actions this server
/// offers that agent itself, and what it tells says who offered them. A
/// button aimed at anything else is refused with the whole telling, and
/// nothing is told. The buttons Tilo makes itself name nobody.
#[tokio::test]
async fn an_agent_offers_only_buttons_it_may_invoke_itself_and_is_named_for_them() {
    let setup = Setup::new().await;
    let own = (setup.own, &setup.ada);
    let agent = agent_of(&setup.ada);
    let telling = |target: &Value, arguments: Value| {
        json!({
            "words": "A device asks to be let in.",
            "actions": [{
                "id": "open", "label": "Open", "main": true,
                "target": target, "arguments": arguments,
            }],
        })
    };
    for target in [
        // Another app's action, which the person's device would invoke.
        json!({"app": "so.tilo.settings", "resource": "devices", "action": "answer-device"}),
        // A workspace's governance.
        json!({"app": WORKSPACE_GOVERNANCE_PROVIDER, "resource": "governance", "action": "member.add"}),
        // What is the person's alone to do.
        json!({"app": "tilo.telling", "resource": "told", "action": "list"}),
        // Its own action, aimed at a document it does not name itself.
        json!({"app": "tilo.telling", "record": "uuid:1", "resource": "told", "action": "insist"}),
    ] {
        let refused = ask_told(
            &setup,
            own,
            &agent,
            person_telling::TELL,
            telling(&target, json!({"approve": true})),
        )
        .await;
        assert!(
            matches!(refused, Err(ActionCallError::Refused { .. })),
            "a button aimed at {target} was taken: {refused:?}"
        );
    }
    assert!(
        listed(&setup, 0).await.told.is_empty(),
        "a telling that was refused was told"
    );

    // What the agent may invoke itself, it may offer, and it is named for
    // it: marking a thing as not to be missed.
    let its_own = json!({"app": "tilo.telling", "resource": "told", "action": "insist"});
    let offered = ask_told(
        &setup,
        own,
        &agent,
        person_telling::TELL,
        telling(&its_own, json!({"told": "told-1", "insists": true})),
    )
    .await
    .expect("the server tells");
    assert_eq!(offered["actions"][0]["id"], "open", "{offered}");
    assert_eq!(offered["offeredBy"]["name"], ASSISTANT, "{offered}");

    // The buttons Tilo makes itself, like the answers of a question that
    // waits, name nobody.
    let question = ToTell {
        id: Some("question-1".to_owned()),
        kind: Kind::Question,
        words: "Send it?".to_owned(),
        actions: vec![Offered {
            id: "yes".to_owned(),
            label: "Yes".to_owned(),
            main: true,
            target: OfferedTarget {
                app: "so.tilo.chat".to_owned(),
                resource: "answers".to_owned(),
                action: "answer-question".to_owned(),
                ..OfferedTarget::default()
            },
            arguments: Map::new(),
        }],
        ..ToTell::default()
    };
    let question = setup
        .workspaces
        .tell(&setup.ada.user_public_key, question)
        .expect("the server tells");
    assert_eq!(question.actions.len(), 1);
    let read = serde_json::to_value(&question).expect("a thing told");
    assert!(read.get("offeredBy").is_none(), "{read}");
}
