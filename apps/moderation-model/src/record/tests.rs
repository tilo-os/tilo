use ui::tilo_resource::Replica;

use super::*;

const ADA: &str = "ada";
const MALLORY: &str = "mallory";
const MOD: &str = "mod";

fn report(replica: &mut Replica<Moderation>, person: &str, reason: &str) -> Result<(), String> {
    replica
        .apply(Moderation::REPORT.with(moderation::Report {
            person: person.into(),
            app: Some("so.tilo.chat".into()),
            record: Some("general".into()),
            item: Some("m1".into()),
            reason: reason.into(),
        }))
        .map(drop)
        .map_err(|error| error.to_string())
}

fn decide(outcome: Outcome, until_ms: Option<i64>, reports: Vec<String>) -> moderation::Decide {
    moderation::Decide {
        id: "d1".into(),
        person: MALLORY.into(),
        reports,
        ruling: Ruling {
            outcome,
            until_ms,
            reasoning: "They posted the same link forty times.".into(),
            model: "claude-haiku-5-5".into(),
            reverted: Vec::new(),
        },
    }
}

#[test]
fn a_person_reports_someone_and_it_stays_open_until_decided() {
    let mut ada = Replica::<Moderation>::new().as_author(ADA);
    report(&mut ada, MALLORY, "spam").unwrap();
    let state = ada.state().unwrap();
    let open: Vec<_> = state.open_reports_on(MALLORY).collect();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].reporter, ADA);
    assert_eq!(open[0].item.as_deref(), Some("m1"));
    let id = open[0].id.clone();

    let mut moderator = ada.fork().as_author(MOD);
    moderator
        .apply(Moderation::DECIDE.with(decide(Outcome::NoAction, None, vec![id.clone()])))
        .unwrap();
    let state = moderator.state().unwrap();
    assert_eq!(state.open_reports().count(), 0);
    let decision = state.decisions.find(&Id::from_raw("d1")).unwrap();
    assert_eq!(decision.by, MOD);
    assert_eq!(decision.reports, vec![id]);
}

#[test]
fn reports_are_bounded() {
    let mut ada = Replica::<Moderation>::new().as_author(ADA);
    assert!(
        report(&mut ada, ADA, "me").is_err(),
        "nobody reports themselves"
    );
    assert!(
        report(&mut ada, MALLORY, "   ").is_err(),
        "a reason is needed"
    );
    assert!(report(&mut ada, MALLORY, &"x".repeat(MAX_REASON_CHARS + 1)).is_err());
    report(&mut ada, MALLORY, "spam").unwrap();
    assert!(
        report(&mut ada, MALLORY, "spam again").is_err(),
        "no duplicates"
    );
    for person in ["p1", "p2", "p3", "p4"] {
        report(&mut ada, person, "spam").unwrap();
    }
    assert!(
        report(&mut ada, "p5", "spam").is_err(),
        "at most {MAX_OPEN_REPORTS} open reports each"
    );
}

#[test]
fn a_decision_is_validated() {
    let mut moderator = Replica::<Moderation>::new().as_author(MOD);
    moderator.set_now_ms(1_000);
    assert!(
        moderator
            .apply(Moderation::DECIDE.with(decide(Outcome::Restrict, None, Vec::new())))
            .is_err(),
        "a restriction needs an end"
    );
    assert!(
        moderator
            .apply(Moderation::DECIDE.with(decide(Outcome::Restrict, Some(500), Vec::new())))
            .is_err(),
        "in the future"
    );
    assert!(
        moderator
            .apply(Moderation::DECIDE.with(decide(Outcome::Ban, Some(5_000), Vec::new())))
            .is_err(),
        "only a restriction ends"
    );
    assert!(
        moderator
            .apply(Moderation::DECIDE.with(decide(Outcome::Ban, None, vec!["nope".into()])))
            .is_err(),
        "unknown report"
    );
    moderator
        .apply(Moderation::DECIDE.with(decide(Outcome::Restrict, Some(61_000), Vec::new())))
        .unwrap();
    assert!(
        moderator
            .apply(Moderation::DECIDE.with(decide(Outcome::Ban, None, Vec::new())))
            .is_err(),
        "ids are unique"
    );
}

#[test]
fn a_moderator_overturns_once() {
    let mut moderator = Replica::<Moderation>::new().as_author(MOD);
    moderator
        .apply(Moderation::DECIDE.with(decide(Outcome::Ban, None, Vec::new())))
        .unwrap();
    let overturn = || {
        Moderation::OVERTURN.with(moderation::Overturn {
            decision: "d1".into(),
            note: "It was a joke between friends.".into(),
        })
    };
    moderator.apply(overturn()).unwrap();
    let state = moderator.state().unwrap();
    let decision = state.decisions.find(&Id::from_raw("d1")).unwrap();
    assert!(!decision.stands());
    assert_eq!(decision.overturned_by.as_deref(), Some(MOD));
    assert!(moderator.apply(overturn()).is_err());
}

#[test]
fn deciding_and_overturning_sit_under_the_moderation_node() {
    let published = Replica::<Moderation>::new().published().unwrap();
    let field = |id: &str| {
        published
            .actions
            .iter()
            .find(|action| action.id == id)
            .and_then(|action| action.contract.field.clone())
    };
    assert_eq!(field("decide").as_deref(), Some("moderation"));
    assert_eq!(field("overturn").as_deref(), Some("moderation"));
    assert_eq!(field("report"), None);
}

/// A moderation record from before a decision's reports and take-backs were
/// model values: each was one JSON string. Opened now, it reads as the record
/// with both as lists, and nothing of the decision is lost.
#[test]
fn a_decision_kept_as_json_strings_migrates_to_lists() {
    use ui::tilo_resource::evolution;
    let mut doc = ui::tilo_resource::Doc::new();
    evolution::fixture(
        &mut doc,
        &ui::automerge::ROOT,
        &ui::serde_json::json!({
            "reports": {},
            "decisions": {
                "d1": {
                    "id": "d1",
                    "person": "bob",
                    "reports": r#"["r1","r2"]"#,
                    "outcome": "ban",
                    "reasoning": "spam in every channel",
                    "model": "",
                    "reverted": r#"[{"app":"so.tilo.chat","record":"","run":"run-1","changes":3}]"#,
                    "by": MOD,
                    "at_ms": 5,
                },
            },
        }),
    )
    .unwrap();
    assert!(
        evolution::read_checked::<Moderation>(&doc).is_err(),
        "unmigrated"
    );
    assert!(evolution::migrate::<Moderation>(&mut doc).unwrap());
    let moderation = evolution::read_checked::<Moderation>(&doc).unwrap();
    let decision = moderation.decisions.iter().next().unwrap();
    assert_eq!(decision.reports, ["r1", "r2"]);
    assert_eq!(
        decision.reverted,
        [Reverted {
            app: "so.tilo.chat".into(),
            record: String::new(),
            run: "run-1".into(),
            changes: 3,
        }]
    );
    assert_eq!(decision.outcome, Outcome::Ban);
    assert_eq!(decision.reasoning, "spam in every channel");
}
