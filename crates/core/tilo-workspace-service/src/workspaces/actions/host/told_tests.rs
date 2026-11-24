//! What a person's assistant tells them: each thing is marked by the
//! person's own rule, which decides alone where it can and asks the
//! person's own model once where it cannot; every such call is counted on
//! the assistant's day and none is made past its limit; and the log is the
//! person's alone.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host a test binds is kept until the test ends; dropping it earlier closes what it drives"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use agent_provider_config::protocol::{Content, ErrorKind, Message, ModelError};
use serde_json::{Map, Value, json};
use tilo_actions::remote::person_telling::{
    self, Cause, Kind, Offered, OfferedTarget, Tell, Teller, Told, ToldList, WhyCode,
};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, ReceiptStatus, WORKSPACE_GOVERNANCE_PROVIDER,
};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::super::tests::{caller, settled};
use super::telling_tests::{ASSISTANT, Setup, agent_of, nobody};
use crate::workspaces::model_answers::scripted::{Linking, Reply, ScriptedProvider};
use crate::workspaces::model_answers::{ProviderProtocol, Spender};
use crate::workspaces::tests::guest_device;
use crate::workspaces::{Spenders, ToTell};

/// A mail as the assistant tells it.
const MAIL: &str = "Kim at Harbor asks whether we take new customers.";
/// A line that counts only in one situation.
const HIRING: &str = "A new customer is important only while we are hiring";
/// A line that always counts.
const LANDLORD: &str = "My landlord is always important";

/// The workspace's one assistant, with the limit the workspace set it.
struct OneAssistant {
    limit: Option<u64>,
}

impl Spenders for OneAssistant {
    fn spender<'a>(
        &'a self,
        _person: &'a [u8; 32],
        workspace: &'a str,
        agent: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Option<Spender>> + Send + 'a>> {
        Box::pin(async move {
            Some(Spender {
                workspace: workspace.to_owned(),
                agent: agent.unwrap_or(ASSISTANT).to_owned(),
                name: "Assistant".into(),
                day: "2026-11-17".into(),
                limit_micros: self.limit,
            })
        })
    }
}

/// The workspace's model, which answers as the test scripts it.
async fn linked(setup: &Setup) -> Arc<ScriptedProvider> {
    let scripted = ScriptedProvider::answering(Vec::<Reply>::new());
    setup.workspaces.answer_models_with(scripted.clone());
    setup
        .workspaces
        .link_model_for(
            &setup.own.to_string(),
            &Linking {
                name: "Models",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://models.example.com/v1",
                key: Some("sk-ada"),
                models: &["judge-small"],
            },
            true,
        )
        .await
        .unwrap();
    scripted
}

/// The workspace's model judges, counted on the day of the assistant Ada
/// keeps, which may spend `limit` a day.
async fn judge(setup: &Setup, limit: Option<u64>) -> Arc<ScriptedProvider> {
    let scripted = linked(setup).await;
    setup
        .workspaces
        .count_judgements_with(Arc::new(OneAssistant { limit }));
    scripted
}

/// What `caller` is answered about what was told, over the page `device`
/// holds in `workspace`.
async fn ask_told(
    setup: &Setup,
    (workspace, device): (WorkspaceId, &DeviceCert),
    caller: &Caller,
    action: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    let page = setup.host.in_workspace_as(workspace, device);
    let mut request = super::accounts_tests::asking(action, argument);
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
async fn told_by_her_event(setup: &Setup, words: &str, cause: Value, insists: bool) -> Told {
    let cause: Cause = serde_json::from_value(cause).expect("a cause");
    let telling = ToTell {
        kind: Kind::of_cause(Some(&cause)),
        tell: Tell {
            words: words.to_owned(),
            cause: Some(cause),
            insists,
            ..Tell::default()
        },
        from: Some(Teller {
            agent: ASSISTANT.to_owned(),
            name: ASSISTANT.to_owned(),
        }),
        workspace: Some(setup.own.to_string()),
        ..ToTell::default()
    };
    setup
        .workspaces
        .tell(&setup.ada.user_public_key, telling)
        .await
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

/// What the same event tells besides, handed over by the host itself under
/// an id of its own.
fn besides(setup: &Setup, id: &str, key: &str) -> ToTell {
    ToTell {
        id: Some(id.to_owned()),
        kind: Kind::News,
        tell: Tell {
            words: "I answered Kim in the usual way.".into(),
            cause: Some(Cause {
                kind: "event".into(),
                label: "Mail arrives".into(),
                key: key.to_owned(),
            }),
            ..Tell::default()
        },
        workspace: Some(setup.own.to_string()),
        ..ToTell::default()
    }
}

/// A message's words.
fn said(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one mail through every way the rule decides, in the order a person lives it"
)]
async fn the_rule_decides_alone_where_it_can_and_the_persons_own_model_is_asked_once_where_it_cannot()
 {
    let setup = Setup::new().await;
    let model = judge(&setup, None).await;
    let ada = setup.ada.user_public_key;

    // Nobody said what is important: the mail is told and marked, reaches
    // nobody, and no model is asked.
    let first = told_by_agent(&setup, MAIL, event("mail-1")).await;
    assert_eq!((first.kind, first.words.as_str()), (Kind::News, MAIL));
    assert_eq!(first.mark.why.code, WhyCode::NoRule);
    assert!(!first.mark.important && !first.reaches);
    assert_eq!(
        first.from.as_ref().map(|from| from.name.as_str()),
        Some(ASSISTANT)
    );
    assert!(model.asked().is_empty());

    // She says it depends on a situation, and the situation is otherwise:
    // the rule decides alone and says which situation.
    setup
        .ada(
            person_telling::SAY,
            json!({"words": HIRING, "onlyWhile": {"fact": "hiring", "is": "yes"}}),
        )
        .await
        .unwrap();
    setup
        .ada(
            person_telling::SET_FACT,
            json!({"name": "Hiring", "value": "no"}),
        )
        .await
        .unwrap();
    let quiet = told_by_agent(&setup, MAIL, event("mail-2")).await;
    assert_eq!(
        (
            quiet.mark.why.code,
            quiet.mark.why.fact.as_str(),
            quiet.mark.why.value.as_str()
        ),
        (WhyCode::NothingHolds, "Hiring", "no")
    );
    assert!(!quiet.mark.important && !quiet.reaches);
    assert!(model.asked().is_empty(), "the rule decided without a model");

    // The situation changes, and the same mail takes judgement: one call
    // of her own model, with her key, shown her lines and the mail.
    setup
        .ada(
            person_telling::SET_FACT,
            json!({"name": "hiring", "value": "yes"}),
        )
        .await
        .unwrap();
    model.then(
        Reply::text(r#"{"important": true, "why": "A new customer while you are hiring."}"#)
            .costing(40),
    );
    let important = told_by_agent(&setup, MAIL, event("mail-3")).await;
    assert!(important.mark.important && important.reaches);
    assert_eq!(important.mark.why.code, WhyCode::Judged);
    assert_eq!(
        important.mark.why.words,
        "A new customer while you are hiring."
    );
    let asked = model.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].model, "judge-small");
    assert_eq!(asked[0].key.as_deref(), Some("sk-ada"));
    assert!(asked[0].request.tools.is_empty());
    let (told, about) = (
        said(&asked[0].request.messages[0]),
        said(&asked[0].request.messages[1]),
    );
    assert!(
        told.starts_with("You decide whether one thing is important to one person"),
        "{told}"
    );
    assert!(about.contains(HIRING), "{about}");
    assert!(about.contains("- Hiring: yes"), "{about}");
    assert!(about.ends_with(MAIL), "{about}");

    // Handed over again it is the same thing: told once, judged once.
    assert_eq!(
        told_by_agent(&setup, MAIL, event("mail-3")).await,
        important
    );
    // And what else the same cause tells is not judged a second time.
    let report = setup
        .workspaces
        .tell(&ada, besides(&setup, "reply:m9", "mail-3"))
        .await
        .unwrap();
    assert!(report.mark.important && report.reaches);
    assert_eq!(report.mark.why, important.mark.why);
    assert_eq!(model.asked().len(), 1);

    // She changes the rule: what the cause tells next is judged by the
    // rule as it stands now.
    setup
        .ada(person_telling::SAY, json!({"words": LANDLORD}))
        .await
        .unwrap();
    model.then(Reply::text(
        "```json\n{\"important\": false, \"why\": \"No line covers a report of what was done.\"}\n```",
    ));
    let later = setup
        .workspaces
        .tell(&ada, besides(&setup, "reply:m10", "mail-3"))
        .await
        .unwrap();
    assert!(!later.mark.important && !later.reaches);
    assert_eq!(later.mark.why.code, WhyCode::Judged);
    assert_eq!(
        later.mark.why.words,
        "No line covers a report of what was done."
    );
    assert_eq!(model.asked().len(), 2);

    // A reminder is hers: told as this server carries out the event she
    // set up, it reaches her as that, and nobody judges it.
    let reminder = told_by_her_event(
        &setup,
        "Call Jo.",
        json!({"kind": "reminder", "label": "Call Jo", "key": "r-1"}),
        false,
    )
    .await;
    assert_eq!(reminder.kind, Kind::Reminder);
    assert!(reminder.reaches && !reminder.mark.important);
    assert_eq!(reminder.mark.why.code, WhyCode::Reminder);
    assert_eq!(model.asked().len(), 2);

    // A brief reaches her as a brief, and carries the mark where her rule
    // gives it one.
    model.then(Reply::text(
        r#"{"important": true, "why": "The landlord is in it."}"#,
    ));
    let brief = told_by_her_event(
        &setup,
        "At ten: the landlord, about the lease.",
        json!({"kind": "brief", "label": "Morning rundown", "key": "b-1"}),
        false,
    )
    .await;
    assert_eq!(brief.kind, Kind::Brief);
    assert!(brief.reaches && brief.mark.important);
    assert_eq!(model.asked().len(), 3);

    // A judgement that cannot be had is said as such and never guessed,
    // and the next thing of that cause is judged anew.
    model.then(Reply::failing(ModelError::new(ErrorKind::Unavailable)));
    let unjudged = told_by_agent(&setup, MAIL, event("mail-4")).await;
    assert_eq!(
        (unjudged.mark.why.code, unjudged.mark.why.value.as_str()),
        (WhyCode::Unjudged, "provider")
    );
    assert!(!unjudged.mark.important && !unjudged.reaches);
    assert_eq!(model.asked().len(), 4);
    model.then(Reply::text(
        r#"{"important": true, "why": "A new customer while you are hiring."}"#,
    ));
    let retried = setup
        .workspaces
        .tell(&ada, besides(&setup, "reply:m11", "mail-4"))
        .await
        .unwrap();
    assert!(retried.mark.important && retried.reaches);
    assert_eq!(model.asked().len(), 5);

    // She reads all of it back, oldest first, each in its place.
    let log = listed(&setup, 0).await;
    let places: Vec<u64> = log.told.iter().map(|told| told.seq).collect();
    assert_eq!(places, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(log.newest, 9);
    assert_eq!(log.told[2], important);
    assert_eq!(listed(&setup, 8).await.told, [retried]);
}

#[tokio::test]
async fn a_judgement_is_counted_on_the_assistants_day_and_none_is_made_past_its_limit() {
    let setup = Setup::new().await;
    let model = judge(&setup, Some(50)).await;
    setup
        .ada(person_telling::SAY, json!({"words": LANDLORD}))
        .await
        .unwrap();

    for key in ["mail-1", "mail-2"] {
        model.then(
            Reply::text(r#"{"important": false, "why": "It is not the landlord."}"#).costing(40),
        );
        let told = told_by_agent(&setup, MAIL, event(key)).await;
        assert_eq!(told.mark.why.code, WhyCode::Judged, "{key}");
    }
    assert_eq!(model.asked().len(), 2);

    // The day's 50 are spent, 80 by now: the next thing is not judged, no
    // provider is asked, and the thing says why in words she can act on.
    let past = told_by_agent(&setup, MAIL, event("mail-3")).await;
    assert_eq!(
        (past.mark.why.code, past.mark.why.value.as_str()),
        (WhyCode::Unjudged, "day-limit")
    );
    assert!(!past.mark.why.words.is_empty());
    assert!(!past.mark.important && !past.reaches);
    assert_eq!(model.asked().len(), 2);
}

#[tokio::test]
async fn no_judgement_is_made_that_no_assistants_day_counts() {
    let setup = Setup::new().await;
    // She linked a model, and nothing on this host says whose day a call
    // for her would be counted on.
    let model = linked(&setup).await;
    setup
        .ada(person_telling::SAY, json!({"words": LANDLORD}))
        .await
        .unwrap();

    let told = told_by_agent(&setup, MAIL, event("mail-1")).await;
    assert_eq!(
        (told.mark.why.code, told.mark.why.value.as_str()),
        (WhyCode::Unjudged, "no-assistant")
    );
    assert!(!told.mark.important && !told.reaches);
    assert!(model.asked().is_empty());
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
            (ada, Caller::local_agent(ASSISTANT)),
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
async fn reminded(setup: &Setup) -> Told {
    told_by_her_event(
        setup,
        "Call Jo.",
        json!({"kind": "reminder", "label": "Call Jo", "key": "r-1"}),
        false,
    )
    .await
}

#[tokio::test]
async fn only_the_person_acknowledges_and_the_first_time_stands() {
    let setup = Setup::new().await;
    let ada = (setup.own, &setup.ada);
    let reminder = reminded(&setup).await;
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

    let reminder = reminded(&setup).await;
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

/// What `answer` carries, read as a thing told.
fn a_told(answer: Result<Value, ActionCallError>) -> Told {
    serde_json::from_value(answer.expect("the host answers")).expect("a thing told")
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
    let reminder = reminded(&setup).await;
    let question = setup
        .workspaces
        .tell(
            &ada,
            ToTell {
                id: Some("question:m1".into()),
                kind: Kind::Question,
                tell: Tell {
                    words: "Two versions of the deck exist. Which one?".into(),
                    ..Tell::default()
                },
                ..ToTell::default()
            },
        )
        .await
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
    setup
        .ada(
            person_telling::SET_INSISTING,
            json!({"everySecs": 120, "times": 3}),
        )
        .await
        .unwrap();

    // News her rule keeps quiet is not made loud by whoever tells it:
    // neither told as not to be missed, nor marked so afterwards.
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

    // A reminder she said must not be missed insists by her own rhythm,
    // from the moment it is told.
    let must = told_by_her_event(
        &setup,
        "File the tax return.",
        json!({"kind": "reminder", "label": "Tax return", "key": "tax-1"}),
        true,
    )
    .await;
    let insists = must.insists.unwrap();
    assert_eq!(
        (insists.every_secs, insists.times, insists.raised),
        (120, 3, 1)
    );
    assert_eq!(
        insists.next_at_ms,
        Some(must.at_ms.saturating_add(120 * 1000))
    );
    // Her agent marks a thing that reaches her as not to be missed, when
    // she says so of it later.
    let reminder = reminded(&setup).await;
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
    assert!(!loud.mark.important, "her rule's mark stands as it was");
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

#[tokio::test]
async fn a_thing_may_break_do_not_disturb_only_where_its_person_let_its_kind_through() {
    let setup = Setup::new().await;
    // Nothing breaks do-not-disturb until she says so: a reminder reaches
    // her, and her devices hold it back while they are quiet.
    let held = reminded(&setup).await;
    assert!(held.reaches && !held.through_quiet);

    // She lets reminders through. From then on a reminder says it may
    // break do-not-disturb; what reaches her another way still may not.
    setup
        .ada(
            person_telling::LET_THROUGH,
            json!({"kind": "reminder", "may": true}),
        )
        .await
        .unwrap();
    let through = told_by_her_event(
        &setup,
        "Leave for the airport.",
        json!({"kind": "reminder", "label": "Airport", "key": "r-2"}),
        false,
    )
    .await;
    assert!(through.reaches && through.through_quiet);
    let brief = told_by_her_event(
        &setup,
        "At ten: the landlord, about the lease.",
        json!({"kind": "brief", "label": "Morning rundown", "key": "b-1"}),
        false,
    )
    .await;
    assert!(brief.reaches && !brief.through_quiet);
    // What was told before she said so stands as it was told.
    assert!(!listed(&setup, 0).await.told[0].through_quiet);

    // She takes it back, and the next reminder is held again.
    setup
        .ada(
            person_telling::LET_THROUGH,
            json!({"kind": "reminder", "may": false}),
        )
        .await
        .unwrap();
    let again = told_by_her_event(
        &setup,
        "Water the plants.",
        json!({"kind": "reminder", "label": "Plants", "key": "r-3"}),
        false,
    )
    .await;
    assert!(again.reaches && !again.through_quiet);
}

/// Whoever tells does not say what kind of thing it is. An agent's own
/// telling that names a reminder or a brief as its cause is news: it is
/// judged by its person's rule like any other, and where the rule keeps it
/// quiet it neither reaches them nor insists. What reaches a person
/// unjudged is what this server tells as it carries out an event of their
/// own, from that event's own cause.
#[tokio::test]
async fn an_agent_that_names_a_reminder_or_a_brief_is_judged_by_its_persons_rule() {
    let setup = Setup::new().await;
    let model = judge(&setup, None).await;
    setup
        .ada(person_telling::SAY, json!({"words": LANDLORD}))
        .await
        .unwrap();
    let own = (setup.own, &setup.ada);
    let agent = agent_of(&setup.ada);
    for (kind, key) in [("reminder", "r-9"), ("brief", "b-9")] {
        model.then(Reply::text(
            r#"{"important": false, "why": "No line covers it."}"#,
        ));
        let asked = model.asked().len();
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
        assert_eq!(
            model.asked().len(),
            asked + 1,
            "her rule was not asked about a {kind} an agent named"
        );
        assert_eq!(told.mark.why.code, WhyCode::Judged);
        assert!(
            !told.reaches && told.insists.is_none(),
            "a {kind} an agent named reached her past her rule"
        );
    }

    // Her own reminder, told as this server carries out the event she set
    // up, reaches her as that, and nobody judges it.
    let asked = model.asked().len();
    let hers = told_by_her_event(
        &setup,
        "Call Jo.",
        json!({"kind": "reminder", "label": "Call Jo", "key": "r-1"}),
        false,
    )
    .await;
    assert_eq!(
        (hers.kind, hers.mark.why.code),
        (Kind::Reminder, WhyCode::Reminder)
    );
    assert!(hers.reaches);
    assert_eq!(model.asked().len(), asked);
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
        // What is the person's alone to say.
        json!({"app": "tilo.telling", "resource": "importance", "action": "let-through"}),
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
        tell: Tell {
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
            ..Tell::default()
        },
        workspace: Some(setup.own.to_string()),
        ..ToTell::default()
    };
    let question = setup
        .workspaces
        .tell(&setup.ada.user_public_key, question)
        .await
        .expect("the server tells");
    assert_eq!(question.actions.len(), 1);
    let read = serde_json::to_value(&question).expect("a thing told");
    assert!(read.get("offeredBy").is_none(), "{read}");
}
