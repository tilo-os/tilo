//! The `tilo.moderation` record: reports, and the decisions taken on them.
//!
//! Anyone in the room may report; only those holding the room's
//! `moderation` node (Moderators, the Owner, and the host's moderation agent,
//! whose delegation carries the Moderator preset) decide and overturn,
//! because both verbs are `#[action(moderation)]` and the ordinary grant
//! check places them there.

use serde::{Deserialize, Serialize};
use ui::tilo_actions::ActorKind;
use ui::tilo_resource::{
    self, ActionCx, Id, Identified, Json, JsonSchema, Keyed, Model, Refusal, resource,
};

/// The longest reason a reporter may give.
pub const MAX_REASON_CHARS: usize = 500;
/// Reports one person may have open at once, so reporting cannot be used to
/// flood the moderators or the model.
pub const MAX_OPEN_REPORTS: usize = 5;
/// The longest reasoning a decision keeps.
pub const MAX_REASONING_CHARS: usize = 4000;
/// The longest a restriction may last; longer is a ban, which a Moderator
/// can see and lift.
pub const MAX_RESTRICTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const MAX_ID_CHARS: usize = 128;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Moderation {
    pub reports: Keyed<Report>,
    pub decisions: Keyed<Decision>,
}

/// One report of a person, optionally pointing at what they did.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Report {
    #[model(readonly)]
    pub id: String,
    /// Who reported, by person id; the moderation agent for its own
    /// heuristics.
    pub reporter: String,
    /// Whom the report is about, by person id.
    pub person: String,
    /// The app, record and item reported, when the report is about content.
    pub app: Option<String>,
    pub record: Option<String>,
    pub item: Option<String>,
    pub reason: String,
    pub at_ms: i64,
    /// The decision that settled it.
    pub decision: Option<String>,
}

impl Identified for Report {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

/// What a decision does to the person.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// Nothing: the report did not hold up.
    #[default]
    NoAction,
    /// No changes until `until_ms`; reading stays allowed.
    Restrict,
    /// Removed and kept out until a Moderator unbans them.
    Ban,
}

/// A record whose changes by the person a decision took back, and the run
/// that did it, so overturning can take the take-back back.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Reverted {
    pub app: String,
    pub record: String,
    /// The history run the moderation agent's undo ran in.
    pub run: String,
    /// How many of the person's changes it took back.
    pub changes: u32,
}

/// What was decided about a person, and why.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Ruling {
    pub outcome: Outcome,
    /// When a restriction lapses, Unix ms; only a restriction has one.
    #[serde(default)]
    pub until_ms: Option<i64>,
    /// The model's reasoning, or the Moderator's, in their words.
    pub reasoning: String,
    /// The model that judged, empty for a person's own decision.
    #[serde(default)]
    pub model: String,
    /// What was taken back.
    #[serde(default)]
    pub reverted: Vec<Reverted>,
}

/// One decision, with the reasoning that led to it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Decision {
    #[model(readonly)]
    pub id: String,
    pub person: String,
    /// The reports it settles.
    pub reports: Json<Vec<String>>,
    pub outcome: Outcome,
    /// When a restriction lapses.
    pub until_ms: Option<i64>,
    /// The model's reasoning, or the Moderator's, in their words.
    pub reasoning: String,
    /// The model that judged, empty for a person's own decision.
    pub model: String,
    pub reverted: Json<Vec<Reverted>>,
    /// Who decided: the agent's or the Moderator's actor id.
    pub by: String,
    pub at_ms: i64,
    pub overturned_by: Option<String>,
    pub overturn_note: Option<String>,
    pub overturned_at_ms: Option<i64>,
}

impl Identified for Decision {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

impl Decision {
    /// Whether the decision still stands.
    #[must_use]
    pub const fn stands(&self) -> bool {
        self.overturned_by.is_none()
    }
}

impl Moderation {
    /// The reports nobody has decided yet, oldest first.
    pub fn open_reports(&self) -> impl Iterator<Item = &Report> {
        self.reports
            .iter()
            .filter(|report| report.decision.is_none())
    }

    /// The open reports about `person`.
    pub fn open_reports_on<'a>(&'a self, person: &'a str) -> impl Iterator<Item = &'a Report> {
        self.open_reports()
            .filter(move |report| report.person == person)
    }
}

fn reporter(cx: &ActionCx) -> Result<String, Refusal> {
    match (cx.actor_kind(), cx.author()) {
        (None | Some(ActorKind::Person), Some(author)) => Ok(author.to_owned()),
        (Some(ActorKind::Agent | ActorKind::Bot), Some(author)) if author == crate::AGENT => {
            Ok(author.to_owned())
        }
        _ => Err(Refusal::new("only a person reports, for themselves")),
    }
}

fn author(cx: &ActionCx) -> Result<String, Refusal> {
    cx.author()
        .map(str::to_owned)
        .ok_or_else(|| Refusal::new("a decision names who took it"))
}

fn id_like(value: &str, what: &str) -> Result<(), Refusal> {
    if value.is_empty()
        || value.chars().count() > MAX_ID_CHARS
        || value.chars().any(char::is_control)
    {
        return Err(Refusal::new(format!("{what} is not a valid id")));
    }
    Ok(())
}

fn optional_id(value: Option<&String>, what: &str) -> Result<(), Refusal> {
    value.map_or(Ok(()), |value| id_like(value, what))
}

#[resource(kind = "tilo.moderation", label = "Moderation", params = moderation)]
impl Moderation {
    /// Report someone.
    ///
    /// Tells the room's moderators, and its moderation agent, that this
    /// person did something they should not have, optionally pointing at
    /// the message or item.
    #[action]
    pub fn report(
        &mut self,
        cx: &mut ActionCx,
        #[param("The person reported")] person: String,
        #[param("The app of the reported content")] app: Option<String>,
        #[param("The record of the reported content")] record: Option<String>,
        #[param("The reported item, such as a message")] item: Option<String>,
        #[param("What they did")] reason: String,
    ) -> Result<(), Refusal> {
        let reporter = reporter(cx)?;
        id_like(&person, "the person")?;
        optional_id(app.as_ref(), "the app")?;
        optional_id(record.as_ref(), "the record")?;
        optional_id(item.as_ref(), "the item")?;
        if person == reporter {
            return Err(Refusal::new("you cannot report yourself"));
        }
        let reason = reason.trim().to_owned();
        if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS {
            return Err(Refusal::new(format!(
                "a reason is 1 to {MAX_REASON_CHARS} characters"
            )));
        }
        let open = self
            .open_reports()
            .filter(|r| r.reporter == reporter)
            .count();
        if open >= MAX_OPEN_REPORTS {
            return Err(Refusal::new(
                "you have reports still being looked at; wait for them",
            ));
        }
        if self
            .open_reports()
            .any(|r| r.reporter == reporter && r.person == person && r.item == item)
        {
            return Err(Refusal::new("you already reported this"));
        }
        let id = cx.id().to_string();
        self.reports.push(Report {
            id,
            reporter,
            person,
            app,
            record,
            item,
            reason,
            at_ms: cx.now_ms(),
            decision: None,
        });
        Ok(())
    }

    /// Decide on a person.
    ///
    /// Records what was done about a person and why, settling the reports
    /// named. Enforcement (the restriction, the ban, the undo) is the
    /// deciding moderator's, through the room's own governance and history.
    #[action(moderation)]
    pub fn decide(
        &mut self,
        cx: &mut ActionCx,
        #[param("A new id for the decision")] id: String,
        #[param("The person decided on")] person: String,
        #[param("The reports it settles")] reports: Vec<String>,
        #[param("What is done, and why")] ruling: Ruling,
    ) -> Result<(), Refusal> {
        let Ruling {
            outcome,
            until_ms,
            reasoning,
            model,
            reverted,
        } = ruling;
        let by = author(cx)?;
        id_like(&id, "the decision")?;
        id_like(&person, "the person")?;
        if self.decisions.find(&Id::from_raw(id.as_str())).is_some() {
            return Err(Refusal::new("a decision with this id exists"));
        }
        let now = cx.now_ms();
        match (outcome, until_ms) {
            (Outcome::Restrict, Some(until))
                if until > now && until.saturating_sub(now) <= MAX_RESTRICTION_MS => {}
            (Outcome::Restrict, _) => {
                return Err(Refusal::new(
                    "a restriction ends in the future, within a week",
                ));
            }
            (_, Some(_)) => return Err(Refusal::new("only a restriction has an end")),
            (_, None) => {}
        }
        if reasoning.trim().is_empty() || reasoning.chars().count() > MAX_REASONING_CHARS {
            return Err(Refusal::new(format!(
                "reasoning is 1 to {MAX_REASONING_CHARS} characters"
            )));
        }
        for report in &reports {
            let held = self
                .reports
                .find(&Id::from_raw(report.as_str()))
                .ok_or_else(|| Refusal::new(format!("no report {report}")))?;
            if held.person != person {
                return Err(Refusal::new(format!(
                    "report {report} is about someone else"
                )));
            }
        }
        for report in &reports {
            if let Some(held) = self.reports.find_mut(&Id::from_raw(report.as_str())) {
                held.decision = Some(id.clone());
            }
        }
        self.decisions.push(Decision {
            id,
            person,
            reports: Json(reports),
            outcome,
            until_ms,
            reasoning,
            model,
            reverted: Json(reverted),
            by,
            at_ms: now,
            overturned_by: None,
            overturn_note: None,
            overturned_at_ms: None,
        });
        Ok(())
    }

    /// Overturn a decision.
    ///
    /// Marks a decision as overturned by a Moderator, who lifts its
    /// restriction or ban and brings back what it took back.
    #[action(moderation)]
    pub fn overturn(
        &mut self,
        cx: &mut ActionCx,
        #[param("The decision")] decision: String,
        #[param("Why it is overturned")] note: String,
    ) -> Result<(), Refusal> {
        let by = author(cx)?;
        let now = cx.now_ms();
        if note.chars().count() > MAX_REASON_CHARS {
            return Err(Refusal::new(format!(
                "a note is at most {MAX_REASON_CHARS} characters"
            )));
        }
        let held = self
            .decisions
            .find_mut(&Id::from_raw(decision.as_str()))
            .ok_or_else(|| Refusal::new(format!("no decision {decision}")))?;
        if !held.stands() {
            return Err(Refusal::new("this decision was already overturned"));
        }
        held.overturned_by = Some(by);
        held.overturn_note = Some(note);
        held.overturned_at_ms = Some(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
