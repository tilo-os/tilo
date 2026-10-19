use std::sync::Mutex;

use tilo_action_policy::{Authority, Context, Role, RoomPolicy};
use tilo_resource::{ActionCx, Collection, Model, Refusal, Replica, resource};

use super::*;
use crate::judge::Verdict;
use crate::{Source, activity_of, destructive_verbs};

const OWNER: &str = "owner";
const MALLORY: &str = "mallory";
const ADA: &str = "ada";
const HELPER: &str = "helper-mod";

/// A board people add cards to and delete them from: enough history to
/// moderate.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Board {
    pub cards: Collection<Card>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Card {
    pub text: String,
}

#[resource(kind = "test.board", label = "Board", params = board)]
impl Board {
    /// Add a card.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, text: String) -> Result<(), Refusal> {
        if text.is_empty() {
            return Err(Refusal::new("a card says something"));
        }
        let id = cx.id();
        self.cards.push(id, Card { text });
        Ok(())
    }

    /// Delete a card.
    #[action(destructive)]
    pub fn delete(&mut self, id: tilo_resource::Id) -> Result<(), Refusal> {
        self.cards
            .remove(&id)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such card"))
    }
}

/// Answers every case the same way, and remembers what it was shown.
struct FakeJudge {
    verdict: Verdict,
    seen: Mutex<Vec<Case>>,
}

impl FakeJudge {
    fn answering(outcome: Outcome, minutes: Option<u32>, revert: bool) -> Self {
        Self {
            verdict: Verdict {
                outcome,
                minutes,
                revert,
                reasoning: "They deleted everyone's cards in a minute.".into(),
            },
            seen: Mutex::default(),
        }
    }
}

impl Judge for FakeJudge {
    fn model(&self) -> &'static str {
        "fake-model"
    }

    fn judge(
        &self,
        case: &Case,
    ) -> impl std::future::Future<Output = Result<Verdict, JudgeError>> + Send {
        self.seen.lock().unwrap().push(case.clone());
        let verdict = self.verdict.clone();
        async move { Ok(verdict) }
    }
}

/// A room with one board, governed by `policy`, whose moderation agent acts
/// with the Moderator role (as the host delegates it).
struct FakeRoom {
    now: i64,
    policy: RoomPolicy,
    board: Replica<Board>,
    moderation: Replica<Moderation>,
    invoked: Vec<(String, String, String, serde_json::Value, String)>,
}

impl FakeRoom {
    fn new() -> Self {
        let mut policy = RoomPolicy::new("r", OWNER);
        for (person, role) in [
            (crate::AGENT, Role::Moderator),
            (MALLORY, Role::Editor),
            (ADA, Role::Editor),
            (HELPER, Role::Moderator),
        ] {
            policy.members.insert(person.to_owned(), role);
        }
        Self {
            now: 0,
            policy,
            board: Replica::new(),
            moderation: Replica::new(),
            invoked: Vec::new(),
        }
    }

    fn act(&mut self, person: &str, invoke: impl Into<tilo_resource::Invoke<Board>>) {
        let mut replica = self.board.fork().as_author(person);
        replica.apply_confirmed(invoke).unwrap();
        self.board.sync_with(&mut replica).unwrap();
    }

    fn cards(&self) -> Vec<String> {
        let mut texts: Vec<_> = self
            .board
            .state()
            .unwrap()
            .cards
            .values()
            .map(|card| card.text.clone())
            .collect();
        texts.sort();
        texts
    }
}

impl RoomDoor for FakeRoom {
    fn now_ms(&self) -> i64 {
        self.now
    }

    fn activity(&mut self, person: &str, since_ms: i64) -> Vec<Activity> {
        let versions = self.board.versions();
        let destructive = destructive_verbs(&self.board.published().unwrap());
        let source = Source {
            app: "test.app",
            record: "b1",
            versions: &versions,
            destructive: &destructive,
        };
        activity_of(&source, person, since_ms)
    }

    fn govern(&mut self, change: Change) -> Result<(), String> {
        let policy = self.policy.clone();
        let agent = Authority::person(policy.party(crate::AGENT));
        let now = u64::try_from(self.now).unwrap();
        self.policy
            .apply(crate::AGENT, &agent, change, &Context::at(now), false)
            .map_err(|refusal| refusal.to_string())
    }

    fn invoke(
        &mut self,
        app: &str,
        record: &str,
        action: &str,
        args: serde_json::Value,
        run: &str,
    ) -> Result<(), String> {
        self.invoked.push((
            app.to_owned(),
            record.to_owned(),
            action.to_owned(),
            args,
            run.to_owned(),
        ));
        Ok(())
    }

    fn decide(&mut self, decision: moderation::Decide) -> Result<(), String> {
        let mut agent = self.moderation.fork().as_agent_for(crate::AGENT, OWNER);
        agent.set_now_ms(self.now);
        agent
            .apply(Moderation::DECIDE.with(decision))
            .map_err(|error| error.to_string())?;
        self.moderation
            .sync_with(&mut agent)
            .map_err(|e| e.to_string())
    }
}

/// Ada adds three cards; Mallory deletes them all; Ada reports Mallory.
fn vandalised() -> FakeRoom {
    let mut room = FakeRoom::new();
    for text in ["one", "two", "three"] {
        room.act(ADA, Board::ADD.with(text.to_owned()));
    }
    let ids: Vec<_> = room.board.state().unwrap().cards.ids().cloned().collect();
    for id in ids {
        room.act(MALLORY, Board::DELETE.with(id));
    }
    let mut ada = room.moderation.fork().as_author(ADA);
    ada.apply(Moderation::REPORT.with(moderation::Report {
        person: MALLORY.into(),
        app: None,
        record: None,
        item: None,
        reason: "deleted all our cards".into(),
    }))
    .unwrap();
    room.moderation.sync_with(&mut ada).unwrap();
    room.now = room
        .board
        .versions()
        .iter()
        .map(|version| version.time_ms)
        .max()
        .unwrap_or_default()
        .saturating_add(1_000);
    assert!(room.cards().is_empty());
    room
}

/// Looks, asks the model and enforces, as the host does.
fn consider(
    agent: &ModerationAgent<FakeJudge>,
    room: &mut FakeRoom,
    moderation: &Moderation,
    person: &str,
) -> Result<Enforced, JudgeError> {
    let look = ModerationAgent::<FakeJudge>::look(room, moderation, person);
    let (verdict, model) = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(agent.verdict(&look))?;
    Ok(ModerationAgent::<FakeJudge>::enforce(
        room, look, verdict, model,
    ))
}

#[test]
fn a_report_restricts_through_the_grant_and_takes_back_the_deletions() {
    let mut room = vandalised();
    let agent = ModerationAgent::new(FakeJudge::answering(Outcome::Restrict, Some(30), true));
    let moderation = room.moderation.state().unwrap();
    assert_eq!(
        ModerationAgent::<FakeJudge>::reported(&moderation),
        [MALLORY]
    );

    let enforced = consider(&agent, &mut room, &moderation, MALLORY).unwrap();
    assert!(enforced.failures.is_empty(), "{:?}", enforced.failures);

    // The model saw the report and Mallory's three deletions, not Ada's adds.
    let seen = agent.judge.seen.lock().unwrap().clone();
    assert_eq!(seen[0].reports[0].reason, "deleted all our cards");
    assert_eq!(seen[0].activity.len(), 3);
    assert!(seen[0].activity.iter().all(|item| item.destructive));

    // The restriction is the room's expiring Deny, checked like any grant.
    let until = room.now + 30 * 60_000;
    let cx = Context::at(u64::try_from(room.now).unwrap());
    assert_eq!(
        room.policy.restricted_until(MALLORY, &cx),
        Some(u64::try_from(until).unwrap())
    );
    assert!(room.policy.is_member(MALLORY));

    // One undo of Mallory's steps on the board, back to before the first.
    assert_eq!(room.invoked.len(), 1);
    let (app, record, action, args, run_id) = &room.invoked[0];
    assert_eq!(
        (app.as_str(), record.as_str(), action.as_str()),
        ("test.app", "b1", "undo")
    );
    assert_eq!(args["by"], seen[0].activity[0].actor);
    assert_eq!(args["to"], seen[0].activity[0].before.clone().unwrap());
    assert_eq!(*run_id, format!("moderation:{}", enforced.decision.id));

    // The decision is recorded with its reasoning and settles the report.
    let moderation = room.moderation.state().unwrap();
    assert_eq!(moderation.open_reports().count(), 0);
    let decision = moderation.decisions.iter().next().unwrap();
    assert_eq!(decision.outcome, Outcome::Restrict);
    assert_eq!(decision.until_ms, Some(until));
    assert_eq!(decision.model, "fake-model");
    assert_eq!(decision.by, crate::AGENT);
    assert!(decision.reasoning.contains("deleted everyone's cards"));
    assert_eq!(decision.reverted[0].changes, 3);
}

#[test]
fn a_ban_removes_and_a_moderator_overturning_restores() {
    let mut room = vandalised();
    let agent = ModerationAgent::new(FakeJudge::answering(Outcome::Ban, None, true));
    let moderation = room.moderation.state().unwrap();
    consider(&agent, &mut room, &moderation, MALLORY).unwrap();
    assert!(room.policy.is_banned(MALLORY));
    assert!(!room.policy.is_member(MALLORY));

    // A human Moderator overturns; the host carries it out.
    let mut helper = room.moderation.fork().as_author(HELPER);
    let id = helper
        .state()
        .unwrap()
        .decisions
        .iter()
        .next()
        .unwrap()
        .id
        .clone();
    helper
        .apply(Moderation::OVERTURN.with(moderation::Overturn {
            decision: id.clone(),
            note: "Mallory was cleaning up test cards Ada asked her to.".into(),
        }))
        .unwrap();
    room.moderation.sync_with(&mut helper).unwrap();
    let decision = room
        .moderation
        .state()
        .unwrap()
        .decisions
        .iter()
        .next()
        .unwrap()
        .clone();
    assert!(!decision.stands());
    ModerationAgent::<FakeJudge>::restore(&mut room, &decision).unwrap();
    assert!(!room.policy.is_banned(MALLORY));
    let (_, _, action, args, _) = room.invoked.last().unwrap();
    assert_eq!(action, "undo");
    assert_eq!(args["run"], format!("moderation:{id}"));
}

#[test]
fn the_agent_cannot_reach_a_moderator_and_records_that() {
    let mut room = vandalised();
    room.policy
        .members
        .insert(MALLORY.to_owned(), Role::Moderator);
    let agent = ModerationAgent::new(FakeJudge::answering(Outcome::Restrict, Some(30), true));
    let moderation = room.moderation.state().unwrap();
    let enforced = consider(&agent, &mut room, &moderation, MALLORY).unwrap();
    assert_eq!(enforced.decision.ruling.outcome, Outcome::NoAction);
    assert!(enforced.decision.ruling.reasoning.contains("Not enforced"));
    assert!(room.invoked.is_empty(), "nothing taken back");
    assert_eq!(
        room.policy
            .restricted_until(MALLORY, &Context::at(u64::try_from(room.now).unwrap())),
        None
    );
}

#[test]
fn a_report_about_someone_who_did_nothing_never_reaches_the_model() {
    let mut room = vandalised();
    let mut ada = room.moderation.fork().as_author(MALLORY);
    ada.apply(Moderation::REPORT.with(moderation::Report {
        person: ADA.into(),
        app: None,
        record: None,
        item: None,
        reason: "she is annoying".into(),
    }))
    .unwrap();
    room.moderation.sync_with(&mut ada).unwrap();
    // Ada's adds are older than the window.
    room.now += LOOKBACK_MS + 1;
    let agent = ModerationAgent::new(FakeJudge::answering(Outcome::Ban, None, true));
    let moderation = room.moderation.state().unwrap();
    let enforced = consider(&agent, &mut room, &moderation, ADA).unwrap();
    assert_eq!(enforced.decision.ruling.outcome, Outcome::NoAction);
    assert!(agent.judge.seen.lock().unwrap().is_empty());
    assert!(room.policy.is_member(ADA));
}

#[test]
fn a_burst_of_deletions_is_noticed_without_a_report() {
    let mut room = FakeRoom::new();
    for index in 0..BURST_COUNT {
        room.act(ADA, Board::ADD.with(format!("card {index}")));
    }
    room.now = room.board.versions().last().unwrap().time_ms + 1;
    assert!(!ModerationAgent::<FakeJudge>::bursting(&mut room, MALLORY));
    let ids: Vec<_> = room.board.state().unwrap().cards.ids().cloned().collect();
    for id in ids {
        room.act(MALLORY, Board::DELETE.with(id));
    }
    room.now = room.board.versions().last().unwrap().time_ms + 1;
    assert!(ModerationAgent::<FakeJudge>::bursting(&mut room, MALLORY));
    assert!(!ModerationAgent::<FakeJudge>::bursting(&mut room, ADA));
}
