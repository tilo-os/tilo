//! What a host tells its people outside Tilo, with no device of theirs on:
//! what is to reach a person is told once and the rest never; the person's
//! own rule and how their situation stands decide which; a device that is
//! looking is left to tell it; and what was acknowledged, taken back or
//! kept on the host is not carried out.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "every test here keeps its host alive to the end"
)]

use tilo_actions::remote::person_telling::{
    Cause, Fact, Holds, Importance, Line, ListAfter, Tell, Teller as ToldBy, WhyCode,
};

use super::*;
use crate::workspaces::ToTell;
use crate::workspaces::fixtures::{TestHost, governor, test_host};
use crate::workspaces::person_model::ProviderProtocol;
use crate::workspaces::person_model::scripted::{Linking, Reply, ScriptedProvider};

/// A person this host keeps a log for. No device of theirs is on.
const ADA: [u8; 32] = [8; 32];
/// The workspace her assistant works in.
const WORKSPACE: &str = "6f1f0f7e-3c1d-4a54-9b6e-2d1c5a7e9f10";
/// A mail as her assistant tells it.
const MAIL: &str = "Kim at Harbor asks whether we take new customers.";
const SECOND: u64 = 1_000;

/// What was told outside Tilo, in order.
#[derive(Default)]
struct Heard {
    told: Mutex<Vec<([u8; 32], Told)>>,
}

impl Heard {
    /// The ids of the things told, in order.
    fn ids(&self) -> Vec<String> {
        self.told
            .lock()
            .unwrap()
            .iter()
            .map(|(_, told)| told.id.clone())
            .collect()
    }
}

impl Outside for Heard {
    fn name(&self) -> &'static str {
        "heard"
    }

    fn tell(&self, person: [u8; 32], told: Told) -> Pin<Box<dyn Future<Output = u32> + Send>> {
        self.told.lock().unwrap().push((person, told));
        Box::pin(async { 2 })
    }
}

/// A host with nobody's device on, and what it told outside.
struct Host {
    workspaces: Workspaces,
    heard: Arc<Heard>,
    _server: TestHost,
    _dir: tempfile::TempDir,
}

impl Host {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        server
            .workspaces()
            .create_workspace("Ada's", Some(WORKSPACE))
            .await
            .unwrap();
        Self {
            workspaces: server.workspaces().clone(),
            heard: Arc::default(),
            _server: server,
            _dir: dir,
        }
    }

    /// A teller that waits four seconds and counts a device as looking for
    /// eight, as a host's own does.
    fn teller(&self) -> Teller {
        Teller::new(self.workspaces.clone(), self.heard.clone())
    }

    /// Ada's own model for judging, answering as scripted.
    async fn judge(&self, replies: impl IntoIterator<Item = Reply>) -> Arc<ScriptedProvider> {
        let scripted = ScriptedProvider::answering(replies);
        self.workspaces.answer_models_with(scripted.clone());
        self.workspaces
            .link_model_for(
                WORKSPACE,
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
}

fn assistant() -> ToldBy {
    ToldBy {
        agent: "assistant".into(),
        name: "Assistant".into(),
    }
}

/// A reminder her assistant tells her.
fn reminder(words: &str, key: &str) -> ToTell {
    ToTell {
        kind: Kind::Reminder,
        tell: Tell {
            words: words.into(),
            cause: Some(Cause {
                kind: "reminder".into(),
                label: words.into(),
                key: key.into(),
            }),
            ..Tell::default()
        },
        from: Some(assistant()),
        ..ToTell::default()
    }
}

/// News her assistant tells her, which an event caused, counted on her
/// assistant's day where it takes judgement.
fn news(words: &str, key: &str) -> ToTell {
    ToTell {
        kind: Kind::News,
        tell: Tell {
            words: words.into(),
            cause: Some(Cause {
                kind: "event".into(),
                label: "Mail arrives".into(),
                key: key.into(),
            }),
            ..Tell::default()
        },
        from: Some(assistant()),
        workspace: Some(WORKSPACE.into()),
        spender: Some(Spender {
            workspace: WORKSPACE.into(),
            agent: "assistant".into(),
            name: "Assistant".into(),
            day: "2026-11-17".into(),
            limit_micros: None,
        }),
        ..ToTell::default()
    }
}

fn line(id: &str, words: &str, only_while: Option<(&str, &str)>) -> Line {
    Line {
        id: id.into(),
        words: words.into(),
        only_while: only_while.map(|(fact, is)| Holds {
            fact: fact.into(),
            is: is.into(),
        }),
        ..Line::default()
    }
}

#[tokio::test]
async fn what_is_to_reach_a_person_is_told_outside_once_and_the_rest_never() {
    let host = Host::new().await;
    let teller = host.teller();
    let model = host
        .judge([
            Reply::text(r#"{"important": true, "why": "The landlord wrote."}"#),
            Reply::text(r#"{"important": false, "why": "It is not the landlord."}"#),
        ])
        .await;
    host.workspaces
        .keep_importance(
            &ADA,
            &Importance {
                lines: vec![line("line-1", "My landlord is always important", None)],
                revision: 1,
                ..Importance::default()
            },
        )
        .unwrap();

    let important = host
        .workspaces
        .tell(&ADA, news("The landlord writes about the lease.", "mail-1"))
        .await
        .unwrap();
    let unimportant = host
        .workspaces
        .tell(&ADA, news("A newsletter arrived.", "mail-2"))
        .await
        .unwrap();
    let reminded = host
        .workspaces
        .tell(&ADA, reminder("Call Jo.", "r-1"))
        .await
        .unwrap();
    assert!(important.mark.important && important.reaches);
    assert!(!unimportant.mark.important && !unimportant.reaches);
    assert!(reminded.reaches && !reminded.mark.important);
    assert_eq!(model.asked().len(), 2, "a reminder is not judged");

    // Not at once: a device that is open hears of it first.
    let newest = reminded.at_ms;
    let next = teller.step(important.at_ms.saturating_add(SECOND)).await;
    assert_eq!(next, Some(important.at_ms.saturating_add(4 * SECOND)));
    assert!(host.heard.ids().is_empty());

    // Four seconds on, with no device of hers looking: what reaches her is
    // told outside, each thing once, and what does not reach her is not.
    assert_eq!(teller.step(newest.saturating_add(4 * SECOND)).await, None);
    assert_eq!(
        host.heard.ids(),
        [important.id.clone(), reminded.id.clone()]
    );
    assert!(
        host.heard
            .told
            .lock()
            .unwrap()
            .iter()
            .all(|(person, _)| *person == ADA)
    );
    // That it was told is written down on the thing, which keeps its place.
    let log = host.workspaces.told_after(&ADA, 0).told;
    let outside: Vec<(&str, u32)> = log
        .iter()
        .map(|told| {
            told.pushed
                .first()
                .map_or(("", 0), |pushed| (pushed.by.as_str(), pushed.took))
        })
        .collect();
    assert_eq!(outside, [("heard", 2), ("", 0), ("heard", 2)]);
    assert_eq!(log[1].mark.why.code, WhyCode::Judged);

    // Never a second time, however long it waits unacknowledged.
    for later in [5 * SECOND, 60 * SECOND, 3_600 * SECOND] {
        assert_eq!(teller.step(newest.saturating_add(later)).await, None);
    }
    assert_eq!(host.heard.ids().len(), 2);
}

#[tokio::test]
async fn the_same_mail_is_told_outside_or_kept_quiet_as_the_persons_situation_stands() {
    let host = Host::new().await;
    let teller = host.teller();
    let model = host
        .judge([Reply::text(
            r#"{"important": true, "why": "A new customer while you are hiring."}"#,
        )])
        .await;
    let rule = |hiring: &str, revision: u64| Importance {
        lines: vec![line(
            "line-1",
            "A new customer is important only while we are hiring",
            Some(("hiring", "yes")),
        )],
        facts: vec![Fact {
            name: "Hiring".into(),
            value: hiring.into(),
            ..Fact::default()
        }],
        revision,
        ..Importance::default()
    };

    // She is not hiring: the rule decides alone, the mail stays in the
    // thread, and nothing is told outside however long it lies there.
    host.workspaces
        .keep_importance(&ADA, &rule("no", 1))
        .unwrap();
    let quiet = host
        .workspaces
        .tell(&ADA, news(MAIL, "mail-1"))
        .await
        .unwrap();
    assert_eq!(
        (
            quiet.mark.why.code,
            quiet.mark.why.fact.as_str(),
            quiet.mark.why.value.as_str()
        ),
        (WhyCode::NothingHolds, "Hiring", "no")
    );
    assert!(!quiet.reaches);
    assert!(model.asked().is_empty(), "no model was asked");
    assert_eq!(
        teller.step(quiet.at_ms.saturating_add(60 * SECOND)).await,
        None
    );
    assert!(host.heard.ids().is_empty());

    // She is hiring: the same mail takes one judgement and reaches her.
    host.workspaces
        .keep_importance(&ADA, &rule("yes", 2))
        .unwrap();
    let loud = host
        .workspaces
        .tell(&ADA, news(MAIL, "mail-2"))
        .await
        .unwrap();
    assert!(loud.mark.important && loud.reaches);
    assert_eq!(model.asked().len(), 1);
    assert_eq!(
        teller.step(loud.at_ms.saturating_add(4 * SECOND)).await,
        None
    );
    assert_eq!(host.heard.ids(), [loud.id]);
}

#[tokio::test]
async fn a_person_whose_device_is_looking_is_not_told_outside_until_it_stops() {
    let host = Host::new().await;
    let lately = Duration::from_millis(200);
    let teller = host.teller().pacing(Pace {
        after: Duration::ZERO,
        watching: lately,
    });
    let reminded = host
        .workspaces
        .tell(&ADA, reminder("Call Jo.", "r-1"))
        .await
        .unwrap();

    // A device of hers reads her log: it shows the reminder itself, and
    // the teller looks again once that device may have stopped looking.
    let read = host
        .workspaces
        .told_listed(&ADA, &ListAfter::default())
        .await;
    assert_eq!(read.told, std::slice::from_ref(&reminded));
    let now = reminded.at_ms.saturating_add(SECOND);
    assert_eq!(teller.step(now).await, Some(now.saturating_add(200)));
    assert!(host.heard.ids().is_empty());

    // The device is gone and she did not acknowledge it: now it is told
    // outside.
    tokio::time::sleep(lately.saturating_add(Duration::from_millis(100))).await;
    assert_eq!(teller.step(now.saturating_add(SECOND)).await, None);
    assert_eq!(host.heard.ids(), [reminded.id]);
}

#[tokio::test]
async fn what_was_acknowledged_or_taken_back_is_not_told_outside() {
    let host = Host::new().await;
    let teller = host.teller();
    let seen = host
        .workspaces
        .tell(&ADA, reminder("Call Jo.", "r-1"))
        .await
        .unwrap();
    let gone = host
        .workspaces
        .tell(&ADA, reminder("Send the lease.", "r-2"))
        .await
        .unwrap();
    let waits = host
        .workspaces
        .tell(&ADA, reminder("Water the plants.", "r-3"))
        .await
        .unwrap();
    host.workspaces
        .acknowledge_told(&ADA, &seen.id, "Ada's phone", "dismissed")
        .unwrap();
    host.workspaces.withdraw_told(&ADA, &gone.id).unwrap();

    assert_eq!(
        teller.step(waits.at_ms.saturating_add(4 * SECOND)).await,
        None
    );
    assert_eq!(host.heard.ids(), [waits.id]);
}

#[tokio::test]
async fn words_a_person_keeps_on_the_host_do_not_leave_it() {
    let host = Host::new().await;
    host.workspaces
        .keep_importance(
            &ADA,
            &Importance {
                kept_on_host: vec![WORKSPACE.into()],
                revision: 1,
                ..Importance::default()
            },
        )
        .unwrap();
    let kept = host
        .workspaces
        .tell(
            &ADA,
            ToTell {
                workspace: Some(WORKSPACE.into()),
                ..reminder("Sign the term sheet in the data room.", "r-1")
            },
        )
        .await
        .unwrap();
    let free = host
        .workspaces
        .tell(&ADA, reminder("Call Jo.", "r-2"))
        .await
        .unwrap();
    assert!(!kept.outside && free.outside);

    // What is told outside names who tells and says where to look, and
    // nothing of the words.
    assert_eq!(
        words_outside(&kept),
        ("Assistant".to_owned(), "Open Tilo to see it.".to_owned())
    );
    assert_eq!(
        words_outside(&free),
        ("Assistant".to_owned(), "Call Jo.".to_owned())
    );
    // A question says that somebody waits; a thing nobody is named for is
    // Tilo's.
    let question = Told {
        kind: Kind::Question,
        ..free.clone()
    };
    assert_eq!(words_outside(&question).0, "Assistant needs you");
    let nameless = Told { from: None, ..free };
    assert_eq!(words_outside(&nameless).0, "Tilo");
}

#[tokio::test]
async fn one_teller_tells_for_a_host() {
    let host = Host::new().await;
    assert!(host.workspaces.claim_telling_outside());
    // A second one, started for the same workspaces, ends at once: nothing
    // is told outside twice.
    tokio::time::timeout(Duration::from_secs(5), host.teller().run())
        .await
        .expect("a second teller ends at once");
    assert!(!host.workspaces.claim_telling_outside());
}

/// A reminder Ada said must not be missed, raised again every minute, three
/// times at most.
async fn must_not_be_missed(host: &Host) -> Told {
    host.workspaces
        .keep_importance(
            &ADA,
            &Importance {
                insists: tilo_actions::remote::person_telling::Insisting {
                    every_secs: 60,
                    times: 3,
                },
                revision: 1,
                ..Importance::default()
            },
        )
        .unwrap();
    let mut must = reminder("File the tax return.", "tax-1");
    must.tell.insists = true;
    host.workspaces.tell(&ADA, must).await.unwrap()
}

#[tokio::test]
async fn what_must_not_be_missed_is_told_again_until_it_is_acknowledged() {
    let host = Host::new().await;
    let teller = host.teller();
    let told = must_not_be_missed(&host).await;
    let began = told.at_ms;
    assert_eq!(
        told.insists
            .map(|insists| (insists.raised, insists.next_at_ms)),
        Some((1, Some(began.saturating_add(60 * SECOND))))
    );

    // Told outside the first time like anything that reaches her, and due
    // again in a minute.
    assert_eq!(
        teller.step(began.saturating_add(4 * SECOND)).await,
        Some(began.saturating_add(60 * SECOND))
    );
    assert_eq!(host.heard.ids().len(), 1);
    // Not before its time.
    assert_eq!(
        teller.step(began.saturating_add(30 * SECOND)).await,
        Some(began.saturating_add(60 * SECOND))
    );
    assert_eq!(host.heard.ids().len(), 1);

    // A minute on she has not acknowledged it: it is raised again, takes a
    // new place so a device of hers that runs raises its notice again, and
    // is told outside again.
    assert_eq!(
        teller.step(began.saturating_add(60 * SECOND)).await,
        Some(began.saturating_add(120 * SECOND))
    );
    assert_eq!(host.heard.ids(), [told.id.clone(), told.id.clone()]);
    let raised = host.workspaces.told_after(&ADA, told.seq).told;
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].insists.map(|insists| insists.raised), Some(2));

    // She acknowledges it, and it stops.
    host.workspaces
        .acknowledge_told(&ADA, &told.id, "Ada's phone", "opened")
        .unwrap();
    for later in [120 * SECOND, 180 * SECOND, 3_600 * SECOND] {
        assert_eq!(teller.step(began.saturating_add(later)).await, None);
    }
    assert_eq!(host.heard.ids().len(), 2);
}

#[tokio::test]
async fn what_insists_is_raised_no_more_often_than_its_person_said() {
    let host = Host::new().await;
    let teller = host.teller();
    let told = must_not_be_missed(&host).await;
    let began = told.at_ms;

    // Three times in all, the first telling included, and then no more,
    // though she never acknowledges it.
    teller.step(began.saturating_add(4 * SECOND)).await;
    teller.step(began.saturating_add(60 * SECOND)).await;
    assert_eq!(teller.step(began.saturating_add(120 * SECOND)).await, None);
    assert_eq!(host.heard.ids().len(), 3);
    for later in [180 * SECOND, 240 * SECOND, 3_600 * SECOND] {
        assert_eq!(teller.step(began.saturating_add(later)).await, None);
    }
    assert_eq!(host.heard.ids().len(), 3);
    let last = host.workspaces.told_after(&ADA, 0).told;
    assert_eq!(
        last[0]
            .insists
            .map(|insists| (insists.raised, insists.next_at_ms)),
        Some((3, None))
    );
    assert!(last[0].waits(), "it still waits for her in Tilo");
}

/// Somebody else on the same host.
const BO: [u8; 32] = [9; 32];

/// An agent `person` keeps, made at `made`.
fn agent(id: &str, acts_for: [u8; 32], made: i64) -> Agent {
    Agent {
        id: tilo_resource::Atom(id.to_owned()),
        name: id.to_owned(),
        acts_for: hex::encode(acts_for),
        created_at: made,
        ..Agent::default()
    }
}

/// A judgement made for a person is counted on an agent that person keeps:
/// the one that told the thing, else their assistant. It is never counted
/// on somebody else's agent, and for a person who keeps none there is no
/// day to count it on, so none is made.
#[test]
fn a_judgement_is_counted_on_an_agent_its_person_keeps_and_on_nobody_elses() {
    let mut retired = agent("old", ADA, 1);
    retired.state = tilo_workspace_agent::AgentState::Retired;
    let mut connected = agent("claude-code", ADA, 2);
    connected.mcp_client = Some("Claude Code".to_owned());
    let mut assistant = agent("assistant", ADA, 3);
    assistant.spend_limit_micros = Some(50);
    let agents = [
        retired,
        connected,
        assistant,
        agent("researcher", ADA, 4),
        agent("bos-assistant", BO, 0),
    ];
    let on = |person: &[u8; 32], told_by: Option<&str>| {
        counted_on(&agents, person, told_by).map(|agent| agent.id.0.clone())
    };

    // Nobody told it: the assistant she keeps, the first made that still
    // works and that no MCP client connects as. Its limit travels with it.
    assert_eq!(on(&ADA, None).as_deref(), Some("assistant"));
    assert_eq!(
        counted_on(&agents, &ADA, None).and_then(|agent| agent.spend_limit_micros),
        Some(50)
    );
    // An agent of hers told it: that agent's own day.
    assert_eq!(on(&ADA, Some("researcher")).as_deref(), Some("researcher"));
    assert_eq!(
        on(&ADA, Some("claude-code")).as_deref(),
        Some("claude-code")
    );
    // What is not hers, works no more or is nobody: her assistant's day.
    for other in ["bos-assistant", "old", "nobody"] {
        assert_eq!(
            on(&ADA, Some(other)).as_deref(),
            Some("assistant"),
            "{other}"
        );
    }
    // Somebody else's judgements are counted on their own agent, never on
    // hers, whatever names it.
    assert_eq!(on(&BO, None).as_deref(), Some("bos-assistant"));
    assert_eq!(on(&BO, Some("assistant")).as_deref(), Some("bos-assistant"));
    // Who keeps no agent has no day a call could be counted on.
    assert_eq!(on(&[7; 32], None), None);
    assert_eq!(on(&[7; 32], Some("assistant")), None);
}
