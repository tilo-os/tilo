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
use serde_json::{Value, json};
use tilo_actions::remote::person_telling::{self, Cause, Kind, Tell, Told, ToldList, WhyCode};
use tilo_actions::remote::{ActionHost, ActionHostError, Caller, ReceiptStatus};
use tilo_relay_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::super::tests::{caller, settled};
use super::telling_tests::{ASSISTANT, Desk, agent_of, nobody};
use crate::workspaces::person_model::scripted::{Linking, Reply, ScriptedProvider};
use crate::workspaces::person_model::{ProviderProtocol, Spender};
use crate::workspaces::tests::guest_device;
use crate::workspaces::{Spenders, ToTell};

/// A mail as the assistant tells it.
const MAIL: &str = "Kim at Harbor asks whether we take new customers.";
/// A line that counts only in one situation.
const HIRING: &str = "A new customer is important only while we are hiring";
/// A line that always counts.
const LANDLORD: &str = "My landlord is always important";

/// The one assistant a person keeps, with the limit its keeper set it.
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

/// Ada links a provider of her own, which answers as the test scripts it.
fn linked(desk: &Desk) -> Arc<ScriptedProvider> {
    let scripted = ScriptedProvider::answering(Vec::<Reply>::new());
    desk.workspaces.answer_models_with(scripted.clone());
    desk.workspaces
        .link_model_for(
            &desk.ada.user_public_key,
            &Linking {
                name: "Models",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://models.example.com/v1",
                key: Some("sk-ada"),
                models: &["judge-small"],
            },
            true,
        )
        .unwrap();
    scripted
}

/// Ada's own model judges, counted on the day of the assistant she keeps,
/// which may spend `limit` a day.
fn judge(desk: &Desk, limit: Option<u64>) -> Arc<ScriptedProvider> {
    let scripted = linked(desk);
    desk.workspaces
        .count_judgements_with(Arc::new(OneAssistant { limit }));
    scripted
}

/// What `caller` is answered about what was told, over the page `device`
/// holds in `workspace`.
async fn ask_told(
    desk: &Desk,
    (workspace, device): (WorkspaceId, &DeviceCert),
    caller: &Caller,
    action: &str,
    argument: Value,
) -> Result<Value, ActionHostError> {
    let page = desk.host.in_workspace_as(workspace, device);
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
async fn told_by_agent(desk: &Desk, words: &str, cause: Value) -> Told {
    let answer = ask_told(
        desk,
        (desk.own, &desk.ada),
        &agent_of(&desk.ada),
        person_telling::TELL,
        json!({"words": words, "cause": cause}),
    )
    .await;
    serde_json::from_value(answer.expect("the host tells")).expect("a thing told")
}

/// What Ada reads of her log after place `after`, on her device.
async fn listed(desk: &Desk, after: u64) -> ToldList {
    let answer = ask_told(
        desk,
        (desk.own, &desk.ada),
        &caller(&desk.ada),
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
fn besides(desk: &Desk, id: &str, key: &str) -> ToTell {
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
        workspace: Some(desk.own.to_string()),
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
    let desk = Desk::new().await;
    let model = judge(&desk, None);
    let ada = desk.ada.user_public_key;

    // Nobody said what is important: the mail is told and marked, reaches
    // nobody, and no model is asked.
    let first = told_by_agent(&desk, MAIL, event("mail-1")).await;
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
    desk.ada(
        person_telling::SAY,
        json!({"words": HIRING, "onlyWhile": {"fact": "hiring", "is": "yes"}}),
    )
    .await
    .unwrap();
    desk.ada(
        person_telling::SET_FACT,
        json!({"name": "Hiring", "value": "no"}),
    )
    .await
    .unwrap();
    let quiet = told_by_agent(&desk, MAIL, event("mail-2")).await;
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
    desk.ada(
        person_telling::SET_FACT,
        json!({"name": "hiring", "value": "yes"}),
    )
    .await
    .unwrap();
    model.then(
        Reply::text(r#"{"important": true, "why": "A new customer while you are hiring."}"#)
            .costing(40),
    );
    let important = told_by_agent(&desk, MAIL, event("mail-3")).await;
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
    assert_eq!(told_by_agent(&desk, MAIL, event("mail-3")).await, important);
    // And what else the same cause tells is not judged a second time.
    let report = desk
        .workspaces
        .tell(&ada, besides(&desk, "reply:m9", "mail-3"))
        .await
        .unwrap();
    assert!(report.mark.important && report.reaches);
    assert_eq!(report.mark.why, important.mark.why);
    assert_eq!(model.asked().len(), 1);

    // She changes the rule: what the cause tells next is judged by the
    // rule as it stands now.
    desk.ada(person_telling::SAY, json!({"words": LANDLORD}))
        .await
        .unwrap();
    model.then(Reply::text(
        "```json\n{\"important\": false, \"why\": \"No line covers a report of what was done.\"}\n```",
    ));
    let later = desk
        .workspaces
        .tell(&ada, besides(&desk, "reply:m10", "mail-3"))
        .await
        .unwrap();
    assert!(!later.mark.important && !later.reaches);
    assert_eq!(later.mark.why.code, WhyCode::Judged);
    assert_eq!(
        later.mark.why.words,
        "No line covers a report of what was done."
    );
    assert_eq!(model.asked().len(), 2);

    // A reminder is hers: it reaches her as that, and nobody judges it.
    let reminder = told_by_agent(
        &desk,
        "Call Jo.",
        json!({"kind": "reminder", "label": "Call Jo", "key": "r-1"}),
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
    let brief = told_by_agent(
        &desk,
        "At ten: the landlord, about the lease.",
        json!({"kind": "brief", "label": "Morning rundown", "key": "b-1"}),
    )
    .await;
    assert_eq!(brief.kind, Kind::Brief);
    assert!(brief.reaches && brief.mark.important);
    assert_eq!(model.asked().len(), 3);

    // A judgement that cannot be had is said as such and never guessed,
    // and the next thing of that cause is judged anew.
    model.then(Reply::failing(ModelError::new(ErrorKind::Unavailable)));
    let unjudged = told_by_agent(&desk, MAIL, event("mail-4")).await;
    assert_eq!(
        (unjudged.mark.why.code, unjudged.mark.why.value.as_str()),
        (WhyCode::Unjudged, "provider")
    );
    assert!(!unjudged.mark.important && !unjudged.reaches);
    assert_eq!(model.asked().len(), 4);
    model.then(Reply::text(
        r#"{"important": true, "why": "A new customer while you are hiring."}"#,
    ));
    let retried = desk
        .workspaces
        .tell(&ada, besides(&desk, "reply:m11", "mail-4"))
        .await
        .unwrap();
    assert!(retried.mark.important && retried.reaches);
    assert_eq!(model.asked().len(), 5);

    // She reads all of it back, oldest first, each in its place.
    let log = listed(&desk, 0).await;
    let places: Vec<u64> = log.told.iter().map(|told| told.seq).collect();
    assert_eq!(places, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(log.newest, 9);
    assert_eq!(log.told[2], important);
    assert_eq!(listed(&desk, 8).await.told, [retried]);
}

#[tokio::test]
async fn a_judgement_is_counted_on_the_assistants_day_and_none_is_made_past_its_limit() {
    let desk = Desk::new().await;
    let model = judge(&desk, Some(50));
    desk.ada(person_telling::SAY, json!({"words": LANDLORD}))
        .await
        .unwrap();

    for key in ["mail-1", "mail-2"] {
        model.then(
            Reply::text(r#"{"important": false, "why": "It is not the landlord."}"#).costing(40),
        );
        let told = told_by_agent(&desk, MAIL, event(key)).await;
        assert_eq!(told.mark.why.code, WhyCode::Judged, "{key}");
    }
    assert_eq!(model.asked().len(), 2);

    // The day's 50 are spent, 80 by now: the next thing is not judged, no
    // provider is asked, and the thing says why in words she can act on.
    let past = told_by_agent(&desk, MAIL, event("mail-3")).await;
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
    let desk = Desk::new().await;
    // She linked a model, and nothing on this host says whose day a call
    // for her would be counted on.
    let model = linked(&desk);
    desk.ada(person_telling::SAY, json!({"words": LANDLORD}))
        .await
        .unwrap();

    let told = told_by_agent(&desk, MAIL, event("mail-1")).await;
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
    let desk = Desk::new().await;
    let told = told_by_agent(&desk, MAIL, event("mail-1")).await;
    assert_eq!(listed(&desk, 0).await.told, [told]);

    // Her agent tells her, from a workspace she shares as well. It does
    // not read what she was told, and she tells herself nothing through
    // it.
    for workspace in [desk.own, desk.shared] {
        nobody(
            "an agent reading what its person was told",
            ask_told(
                &desk,
                (workspace, &desk.ada),
                &agent_of(&desk.ada),
                person_telling::LIST,
                json!({}),
            )
            .await,
        );
    }
    ask_told(
        &desk,
        (desk.shared, &desk.ada),
        &agent_of(&desk.ada),
        person_telling::TELL,
        json!({"words": "The team's plan changed."}),
    )
    .await
    .expect("her agent tells her from a shared workspace");
    nobody(
        "a person telling themselves",
        ask_told(
            &desk,
            (desk.own, &desk.ada),
            &caller(&desk.ada),
            person_telling::TELL,
            json!({"words": "A note to myself."}),
        )
        .await,
    );
    assert_eq!(listed(&desk, 0).await.told.len(), 2);

    // Another member reads their own log, which holds nothing of hers, and
    // what their agent tells is told to them.
    let (_, bo) = guest_device();
    for workspace in [desk.shared, desk.own] {
        let theirs: ToldList = serde_json::from_value(
            ask_told(
                &desk,
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
        &desk,
        (desk.shared, &bo),
        &agent_of(&bo),
        person_telling::TELL,
        json!({"words": "For Bo."}),
    )
    .await
    .expect("another member's agent tells that member");
    assert_eq!(listed(&desk, 0).await.told.len(), 2);

    // An agent that says it acts for Ada over a device that is not hers
    // is nobody, and so are an agent no device stands behind, an app, and
    // a bot installed for her.
    let pretending = (
        (desk.shared, &bo),
        Caller::delegated_agent(ASSISTANT, desk.ada.user_public_key, bo.device_public_key),
    );
    let ada = (desk.own, &desk.ada);
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
                    desk.ada.user_public_key,
                    desk.ada.device_public_key,
                ),
            ),
        ),
    ];
    for (what, (page, stranger)) in &strangers {
        nobody(
            what,
            ask_told(
                &desk,
                *page,
                stranger,
                person_telling::TELL,
                json!({"words": "From someone else."}),
            )
            .await,
        );
        nobody(
            what,
            ask_told(&desk, *page, stranger, person_telling::LIST, json!({})).await,
        );
    }
    assert_eq!(listed(&desk, 0).await.told.len(), 2);
}

#[tokio::test]
async fn a_device_that_waits_is_answered_when_something_is_told() {
    let desk = Desk::new().await;
    let ada = (desk.own, &desk.ada);

    // Her device asks with a wait, and is answered the moment her agent
    // tells her something.
    let started = tilo_clock::now();
    let her = caller(&desk.ada);
    let waiting = ask_told(
        &desk,
        ada,
        &her,
        person_telling::LIST,
        json!({"after": 0, "waitMs": 8000}),
    );
    let telling = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        told_by_agent(&desk, MAIL, event("mail-1")).await
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
            &desk,
            ada,
            &caller(&desk.ada),
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
