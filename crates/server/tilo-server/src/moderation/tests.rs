#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Mutex;

use tilo_moderation::{Case, Judge, JudgeError, ModerationAgent, Verdict};
use tilo_workspace_service::tl_records::RecordBytes;

use super::*;

const OWNER: &str = "owner";
const ADA: &str = "ada";
const MALLORY: &str = "mallory";

/// Restricts for half an hour and takes back what the person did.
struct FakeJudge(Mutex<Vec<Case>>);

impl Judge for FakeJudge {
    fn model(&self) -> &'static str {
        "fake-model"
    }

    fn judge(
        &self,
        case: &Case,
    ) -> impl std::future::Future<Output = Result<Verdict, JudgeError>> + Send {
        self.0.lock().unwrap().push(case.clone());
        async {
            Ok(Verdict {
                outcome: Outcome::Restrict,
                minutes: Some(30),
                revert: true,
                reasoning: "They flooded the channel with the same link.".into(),
            })
        }
    }
}

fn person(id: &str) -> Actor {
    tl_chat::member(MemberId::person(id))
}

fn texts_by(opened: &Opened, who: &str) -> Vec<String> {
    tl_chat::all_messages(opened)
        .unwrap()
        .into_iter()
        .filter(|(_, message)| message.who == Some(person(who)))
        .map(|(_, message)| message.text)
        .collect()
}

/// The workspace's Chat after Mallory flooded `#general` and Ada reported one of her posts.
fn flooded_workspace() -> Opened {
    let mut chat = Opened::open(
        &RecordBytes::default(),
        &tl_chat::packages().unwrap(),
        tl_chat::viewer(),
    )
    .unwrap();
    let general = tl_chat::new_channel(&mut chat, &person(OWNER), ("general", ""), None).unwrap();
    tl_chat::send(&mut chat, &person(ADA), &general, "hello everyone", None).unwrap();
    let mut spam = Vec::new();
    for _ in 0..3 {
        spam.push(
            tl_chat::send(
                &mut chat,
                &person(MALLORY),
                &general,
                "buy at spam.example",
                None,
            )
            .unwrap(),
        );
    }
    tl_chat::report(
        &mut chat,
        &person(ADA),
        &general,
        spam.get(1).unwrap(),
        "spam",
    )
    .unwrap();
    chat
}

#[test]
fn a_report_is_about_who_wrote_the_message() {
    let chat = flooded_workspace();
    let filed = moderation(&chat).unwrap();
    let open: Vec<_> = filed.open_reports().collect();
    assert_eq!(open.len(), 1);
    let report = open.first().unwrap();
    assert_eq!(report.reporter, MemberId::person(ADA));
    assert_eq!(report.person, MemberId::person(MALLORY));
    assert!(report.reason.starts_with("spam"), "{}", report.reason);
    assert_eq!(
        ModerationAgent::<FakeJudge>::reported(&filed),
        [MemberId::person(MALLORY)]
    );
}

#[tokio::test]
async fn a_report_restricts_through_the_workspace_and_takes_the_flood_back_as_the_bot() {
    let mut chat = flooded_workspace();
    let agent = ModerationAgent::new(FakeJudge(Mutex::new(Vec::new())));
    let mut governed = Vec::new();
    let mut govern = |change: Change| {
        governed.push(change);
        Ok(())
    };
    let mut api = ChatApi::new(&mut chat, &mut govern, tilo_clock::unix_now_ms_i64());
    let filed = api.moderation().unwrap();
    let mallory = MemberId::person(MALLORY);
    let look = ModerationAgent::<FakeJudge>::look(&mut api, &filed, &mallory);
    assert_eq!(look.case.activity.len(), 3, "{:?}", look.case.activity);
    let (verdict, model) = agent.verdict(&look).await.unwrap();
    let enforced = ModerationAgent::<FakeJudge>::enforce(&mut api, look, verdict, model);
    assert!(enforced.failures.is_empty(), "{:?}", enforced.failures);
    drop(api);

    assert!(matches!(
        governed.as_slice(),
        [Change::Restrict { person, .. }] if person == MALLORY
    ));
    // Her posts are gone; Ada's stays.
    assert!(texts_by(&chat, MALLORY).is_empty());
    assert_eq!(texts_by(&chat, ADA), ["hello everyone"]);
    // The report is settled, saying what was decided and why.
    let after = moderation(&chat).unwrap();
    assert_eq!(after.open_reports().count(), 0);
    // The report outlives the message it is about: who wrote it, what it said and what was
    // decided stay readable, as the person's history the next look reads.
    let kept = after
        .reports
        .iter()
        .next()
        .expect("the report outlives the message");
    assert_eq!(kept.person, mallory);
    assert_eq!(kept.item, None, "the message is deleted");
    assert!(
        kept.reason.contains("buy at spam.example"),
        "{}",
        kept.reason
    );
    assert!(
        kept.decision
            .as_deref()
            .is_some_and(|said| said.starts_with("Restricted until")),
        "{:?}",
        kept.decision
    );
    assert_eq!(agent.model(), "fake-model");
}

#[test]
fn a_decision_reads_on_the_reports_it_settles() {
    let mut chat = flooded_workspace();
    let mut govern = |_: Change| Ok(());
    let mut api = ChatApi::new(&mut chat, &mut govern, tilo_clock::unix_now_ms_i64());
    let filed = api.moderation().unwrap();
    let report = filed.open_reports().next().unwrap().clone();
    api.decide(Decide {
        id: "mallory@1".into(),
        person: report.person.clone(),
        reports: vec![report.id.clone()],
        ruling: tilo_moderation::Ruling {
            outcome: Outcome::NoAction,
            until_ms: None,
            reasoning: "One post, not a flood.".into(),
            model: String::new(),
            reverted: Vec::new(),
        },
    })
    .unwrap();
    drop(api);
    let after = moderation(&chat).unwrap();
    let settled = after.reports.iter().next().unwrap();
    assert_eq!(
        settled.decision.as_deref(),
        Some("No action: One post, not a flood.")
    );
}
