use tilo_resource::{AppNamed, NonBlank, Replica};

use super::*;

const ADA: &str = "ada";
const MALLORY: &str = "mallory";
const MOD: &str = "mod";

fn report(
    replica: &mut Replica<Moderation, AppNamed>,
    person: &str,
    reason: &str,
) -> Result<(), String> {
    replica
        .apply(Moderation::REPORT_MESSAGE.with(moderation::ReportMessage {
            author: MemberId::person(person),
            message: "m1".into(),
            reason: NonBlank::parse(reason).expect("a test reason says something"),
        }))
        .map(drop)
        .map_err(|error| error.to_string())
}

fn decide(
    outcome: Outcome,
    until_ms: Option<i64>,
    reports: Vec<String>,
) -> moderation::DecideReports {
    moderation::DecideReports {
        id: "d1".into(),
        person: MemberId::person(MALLORY),
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
    let mut ada = Replica::<Moderation>::new()
        .in_app("so.tilo.model", "Model")
        .as_author(ADA);
    report(&mut ada, MALLORY, "spam").unwrap();
    let state = ada.state().unwrap();
    let mallory = MemberId::person(MALLORY);
    let open: Vec<_> = state.open_reports_on(&mallory).collect();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].reporter, MemberId::person(ADA));
    assert_eq!(open[0].item.as_deref(), Some("m1"));
    let id = open[0].id.clone();

    let mut moderator = ada.fork().as_author(MOD);
    moderator
        .apply(Moderation::DECIDE_REPORTS.with(decide(Outcome::NoAction, None, vec![id.clone()])))
        .unwrap();
    let state = moderator.state().unwrap();
    assert_eq!(state.open_reports().count(), 0);
    let decision = state.decisions.find(&Id::from_raw("d1")).unwrap();
    assert_eq!(decision.by, MemberId::person(MOD));
    assert_eq!(decision.reports, vec![id]);
}

#[test]
fn reports_are_bounded() {
    let mut ada = Replica::<Moderation>::new()
        .in_app("so.tilo.model", "Model")
        .as_author(ADA);
    assert!(
        report(&mut ada, ADA, "me").is_err(),
        "nobody reports themselves"
    );
    assert!(
        serde_json::from_value::<moderation::ReportPerson>(
            serde_json::json!({"person": {"person": MALLORY}, "reason": "   "})
        )
        .is_err(),
        "a reason is needed, where the arguments are read"
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
    let mut moderator = Replica::<Moderation>::new()
        .in_app("so.tilo.model", "Model")
        .as_author(MOD);
    moderator.set_now_ms(1_000);
    assert!(
        moderator
            .apply(Moderation::DECIDE_REPORTS.with(decide(Outcome::Restrict, None, Vec::new())))
            .is_err(),
        "a restriction needs an end"
    );
    assert!(
        moderator
            .apply(Moderation::DECIDE_REPORTS.with(decide(
                Outcome::Restrict,
                Some(500),
                Vec::new()
            )))
            .is_err(),
        "in the future"
    );
    assert!(
        moderator
            .apply(Moderation::DECIDE_REPORTS.with(decide(Outcome::Ban, Some(5_000), Vec::new())))
            .is_err(),
        "only a restriction ends"
    );
    assert!(
        moderator
            .apply(Moderation::DECIDE_REPORTS.with(decide(Outcome::Ban, None, vec!["nope".into()])))
            .is_err(),
        "unknown report"
    );
    moderator
        .apply(Moderation::DECIDE_REPORTS.with(decide(Outcome::Restrict, Some(61_000), Vec::new())))
        .unwrap();
    assert!(
        moderator
            .apply(Moderation::DECIDE_REPORTS.with(decide(Outcome::Ban, None, Vec::new())))
            .is_err(),
        "ids are unique"
    );
}

#[test]
fn a_moderator_overturns_once() {
    let mut moderator = Replica::<Moderation>::new()
        .in_app("so.tilo.model", "Model")
        .as_author(MOD);
    moderator
        .apply(Moderation::DECIDE_REPORTS.with(decide(Outcome::Ban, None, Vec::new())))
        .unwrap();
    let overturn = || {
        Moderation::OVERTURN_DECISION.with(moderation::OverturnDecision {
            decision: "d1".into(),
            note: "It was a joke between friends.".into(),
        })
    };
    moderator.apply(overturn()).unwrap();
    let state = moderator.state().unwrap();
    let decision = state.decisions.find(&Id::from_raw("d1")).unwrap();
    assert!(!decision.stands());
    assert_eq!(decision.overturned_by, Some(MemberId::person(MOD)));
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
            .expect("the moderation record publishes this action")
            .contract
            .field
            .as_deref()
    };
    assert_eq!(field("decide-reports"), Some("moderation"));
    assert_eq!(field("overturn-decision"), Some("moderation"));
    assert_eq!(field("report-message"), None);
    assert_eq!(field("report-person"), None);
}

/// A moderation resource from before a decision's reports and take-backs were model values:
/// each was one JSON string.
#[test]
fn a_decision_kept_as_json_strings_migrates_to_lists() {
    use tilo_resource::evolution;
    let mut doc = tilo_resource::Doc::new();
    evolution::fixture(
        &mut doc,
        &automerge::ROOT,
        &serde_json::json!({
            "reports": {},
            "decisions": {
                "d1": {
                    "id": "d1",
                    "person": r#"{"person":"bob"}"#,
                    "reports": r#"["r1","r2"]"#,
                    "outcome": "ban",
                    "reasoning": "spam in every channel",
                    "model": "",
                    "reverted": r#"[{"app":"so.tilo.chat","record":"","run":"run-1","changes":3}]"#,
                    "by": r#"{"person":"mod"}"#,
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
    assert!(
        tilo_resource::founding::bring_up_resource::<Moderation>("record", &mut doc)
        .unwrap()
    );
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

#[test]
fn a_moderator_can_reconsider_a_report_after_the_bot_looked_at_it() {
    let mut ada = Replica::<Moderation>::new()
        .in_app("so.tilo.model", "Model")
        .as_author(ADA);
    report(&mut ada, MALLORY, "spam").unwrap();
    let id = ada
        .state()
        .unwrap()
        .open_reports()
        .next()
        .unwrap()
        .id
        .clone();
    let mut bot = ada.fork().as_bot_for("moderation", MOD);
    bot.apply(Moderation::DECIDE_REPORTS.with(decide(Outcome::NoAction, None, vec![id.clone()])))
        .unwrap();

    let mut later = decide(Outcome::Ban, None, vec![id.clone()]);
    later.id = "human-review".into();
    assert!(
        bot.apply(Moderation::DECIDE_REPORTS.with(later.clone()))
            .is_err()
    );

    let mut moderator = bot.fork().as_author(MOD);
    moderator
        .apply(Moderation::DECIDE_REPORTS.with(later))
        .unwrap();
    let state = moderator.state().unwrap();
    assert_eq!(
        state
            .reports
            .find(&Id::from_raw(id))
            .unwrap()
            .decision
            .as_deref(),
        Some("human-review")
    );
    assert_eq!(state.decisions.len(), 2);
}

/// The surface this crate ships for the tilolang host to publish as Chat's record: the type and
/// the verbs, written from the model itself, so a field or a verb that changes here changes
/// there. `UPDATE_SNAPSHOTS=1` writes it.
#[test]
fn the_moderation_surface_file_is_what_this_crate_publishes() {
    let surface = tilo_resource::surface::surface_of::<Moderation>();
    let text = tilo_surface_compile::canonical(&serde_json::json!({ "resources": [surface] }));
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("surface.json");
    let committed = std::fs::read_to_string(&path).ok();
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
        std::fs::write(&path, &text).expect("the surface file is written");
        return;
    }
    assert_eq!(
        committed.as_deref(),
        Some(text.as_str()),
        "apps/moderation-model/surface.json is not what this crate publishes; \
         regenerate it with UPDATE_SNAPSHOTS=1"
    );
}
