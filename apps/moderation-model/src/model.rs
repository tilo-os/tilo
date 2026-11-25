//! A workspace's moderation reports and decisions. Chat owns the record.
//! The workspace's grants decide who can report, decide or overturn a decision.

use automerge::ObjId;
use serde::{Deserialize, Serialize};
use tilo_resource::{
    self, ActionCx, Fields, Id, Identified, JsonSchema, Keyed, MemberId, Model, ModelError,
    NonBlank, Refusal, resource,
};

pub const MAX_REASON_CHARS: usize = 500;
/// Reports one person may have open at once, so reporting cannot be used to flood the
/// moderators or the model.
pub const MAX_OPEN_REPORTS: usize = 5;
pub const MAX_REASONING_CHARS: usize = 4000;
pub const MAX_RESTRICTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const MAX_ID_CHARS: usize = 128;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Moderation {
    #[model(readonly)]
    pub reports: Keyed<Report>,
    #[model(readonly)]
    pub decisions: Keyed<Decision>,
}

/// One report of a person, optionally pointing at what they did.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Report {
    #[model(readonly)]
    pub id: String,
    /// Who reported; the moderation agent for its own heuristics.
    pub reporter: MemberId,
    /// Whom the report is about.
    pub person: MemberId,
    /// The app, resource and item reported, when the report is about content.
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
    #[default]
    NoAction,
    Restrict,
    Ban,
}

/// A resource whose changes by the person a decision took back, and the run that did it, so
/// overturning can take the take-back back.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Reverted {
    #[model(atom)]
    pub app: String,
    #[model(atom)]
    pub record: String,
    /// The history run the moderation agent's undo ran in.
    #[model(atom)]
    pub run: String,
    /// How many of the person's changes it took back.
    pub changes: u32,
}

/// What was decided about a person, and why.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Ruling {
    pub outcome: Outcome,
    #[serde(default)]
    pub until_ms: Option<i64>,
    pub reasoning: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub reverted: Vec<Reverted>,
}

/// One decision, with the reasoning that led to it.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Decision {
    #[model(readonly)]
    pub id: String,
    pub person: MemberId,
    /// The reports it settles.
    pub reports: Vec<String>,
    pub outcome: Outcome,
    /// When a restriction lapses.
    pub until_ms: Option<i64>,
    /// The model's reasoning, or the Moderator's, in their words.
    pub reasoning: String,
    /// The model that judged, empty for a person's own decision.
    pub model: String,
    /// What it took back.
    pub reverted: Vec<Reverted>,
    /// Who decided: the agent or the Moderator.
    pub by: MemberId,
    pub at_ms: i64,
    pub overturned_by: Option<MemberId>,
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
    pub fn open_reports_on<'a>(&'a self, person: &'a MemberId) -> impl Iterator<Item = &'a Report> {
        self.open_reports()
            .filter(move |report| &report.person == person)
    }
}

fn reporter(cx: &ActionCx) -> Result<MemberId, Refusal> {
    let by_person = cx
        .invoked_by()
        .as_ref()
        .is_none_or(tilo_actions::Acting::is_person);
    match cx.author() {
        Some(author) if by_person => Ok(author.clone()),
        _ => Err(Refusal::new("only a person reports, for themselves")),
    }
}

fn author(cx: &ActionCx) -> Result<MemberId, Refusal> {
    cx.author()
        .cloned()
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

/// Rewrite the decisions of the moderation resource stored at `moderation` from before a
/// decision's reports and take-backs were model values (each one JSON string) into the
/// shape they are stored in now: a list of report ids and a list of take-backs.
///
/// # Errors Returns why a stored value does not read as its new type.
pub fn upgrade_decisions(
    doc: &mut tilo_resource::Doc,
    moderation: &ObjId,
) -> Result<(), ModelError> {
    for (_, decision) in tilo_resource::evolution::map_values(doc, moderation, "decisions")? {
        tilo_resource::evolution::retype_json::<Vec<String>>(doc, &decision, "reports")?;
        tilo_resource::evolution::retype_json::<Vec<Reverted>>(doc, &decision, "reverted")?;
    }
    Ok(())
}

/// The moderation history still held in a Chat document written before the split.
/// The earlier document's signed history and original actors are kept there.
///
/// # Errors
/// The old record could not be decoded or brought up to the current shape.
pub fn earlier_chat_history(doc: &mut tilo_resource::Doc) -> Result<Moderation, ModelError> {
    let Some(old) = tilo_resource::evolution::map_at(doc, &automerge::ROOT, "moderation")? else {
        return Ok(Moderation::default());
    };
    upgrade_decisions(doc, &old)?;
    Moderation::read_fields(doc, &old)
}

impl Moderation {
    /// What the resource holds: the reports made and the decisions taken on them.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<tilo_resource::__private::serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

impl Moderation {
    /// Report someone.
    ///
    /// Tells the workspace's moderators, and its moderation agent, that this person did
    /// something they should not have, optionally pointing at the message or item.
    fn report(
        &mut self,
        cx: &mut ActionCx,
        person: MemberId,
        item: Option<String>,
        reason: NonBlank,
    ) -> Result<(), Refusal> {
        let reporter = reporter(cx)?;
        id_like(person.id(), "the person")?;
        if let Some(item) = &item {
            id_like(item, "the item")?;
        }
        if reporter == person {
            return Err(Refusal::new("you cannot report yourself"));
        }
        if reason.chars().count() > MAX_REASON_CHARS {
            return Err(Refusal::new(format!(
                "a reason is at most {MAX_REASON_CHARS} characters"
            )));
        }
        let reason = reason.into_string();
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
            app: item.as_ref().map(|_| "so.tilo.chat".to_owned()),
            record: None,
            item,
            reason,
            at_ms: cx.now_ms(),
            decision: None,
        });
        Ok(())
    }
}

#[resource(
    kind = "tilo.moderation",
    app = "so.tilo.chat",
    params = moderation,
    version = 2,
    migrate = Moderation::upgrade,
    read_content = Moderation::read_document
)]
impl Moderation {
    /// Bring a moderation resource of version 1 up to this one: its decisions' reports and
    /// take-backs become model values ([`upgrade_decisions`]).
    fn upgrade(doc: &mut tilo_resource::Doc, from: u32) -> Result<(), ModelError> {
        if from != 1 {
            return Err(ModelError::invalid(format!(
                "no moderation migration from {from}"
            )));
        }
        upgrade_decisions(doc, &automerge::ROOT)
    }

    /// Report message.
    ///
    /// Tells the workspace's moderators that this message, and its author,
    /// should not be here.
    #[action(audience = "agents")]
    pub fn report_message(
        &mut self,
        cx: &mut ActionCx,
        #[param("The message")] message: String,
        #[param("Who wrote it")] author: MemberId,
        #[param("What is wrong with it")] reason: NonBlank,
    ) -> Result<(), Refusal> {
        if author.id().is_empty() || author.id().len() > 4096 {
            return Err(Refusal::new("a reported message is someone's"));
        }
        self.report(cx, author, Some(message), reason)
    }

    /// Report person.
    ///
    /// Tells the workspace's moderators that this person is misbehaving.
    #[action]
    pub fn report_person(
        &mut self,
        cx: &mut ActionCx,
        #[param("The person")] person: MemberId,
        #[param("What they did")] reason: NonBlank,
    ) -> Result<(), Refusal> {
        if !person.is_person() {
            return Err(Refusal::new("there is no such person"));
        }
        self.report(cx, person, None, reason)
    }

    /// Decide on a person.
    ///
    /// Resources what was done about a person and why, settling the reports named.
    #[action(moderation, audience = "agents")]
    pub fn decide_reports(
        &mut self,
        cx: &mut ActionCx,
        #[param("A new id for the decision")] id: String,
        #[param("The person decided on")] person: MemberId,
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
        id_like(person.id(), "the person")?;
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
            if by.is_bot() && held.decision.is_some() {
                return Err(Refusal::new(format!(
                    "report {report} has already been decided"
                )));
            }
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
            reports,
            outcome,
            until_ms,
            reasoning,
            model,
            reverted,
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
    /// Marks a decision as overturned by a Moderator, who lifts its restriction or ban and
    /// brings back what it took back.
    #[action(moderation, audience = "agents")]
    pub fn overturn_decision(
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
