//! The workspace's moderation agent's reach into the workspace's Chat, where members report
//! messages.
//!
//! Nothing here reads or writes around Chat: the agent's API ([`ChatApi`]) reads the reports
//! and the reported person's messages through Chat's own types, takes their messages back with
//! Chat's `delete` and settles each report with Chat's `decide`, each invoked as the moderation
//! bot, so the record's history names the bot for every step like anyone's. A verdict that
//! restricts or bans a member is the workspace's own membership action, made as the bot under
//! the grants a person gave it when they turned it on ([`Governance`]): the workspace decides it,
//! and history names the bot like anyone.

use tilo_action_policy::Change;
use tilo_action_policy::bot::MODERATION;
use tilo_actions::MemberId;
use tilo_moderation::model::moderation::DecideReports;
use tilo_moderation::{Activity, Moderation, Outcome, Report, WorkspaceApi};
use tilo_workspace_service::tl_records::Opened;
use tilo_workspace_service::tl_records::runtime::{Actor, ResourceRef};

use crate::tl_chat;

/// The longest excerpt of a message the model is shown; a spam post is recognisable from its
/// start.
const EXCERPT_CHARS: usize = 600;

/// The moderation bot, as Chat's actions name who acted.
#[must_use]
pub fn bot() -> Actor {
    tl_chat::member(MemberId::bot(MODERATION))
}

/// Unix milliseconds of an ISO-8601 time history gives; 0 for one it does not.
fn millis(when: &str) -> i64 {
    when.parse::<jiff::Timestamp>()
        .map_or(0, jiff::Timestamp::as_millisecond)
}

/// The workspace's reports as the moderation agent decides them: every report filed in Chat,
/// about the member who wrote the reported message, decided ones included, so a person's
/// earlier reports and what was decided on them stay readable after their messages are gone. A
/// report on something no member wrote here (a message relayed from another service) names
/// nobody the workspace governs, so it waits for a person.
///
/// # Errors
/// Chat's package is not loaded.
pub fn moderation(opened: &Opened) -> Result<Moderation, String> {
    let mut moderation = Moderation::default();
    for report in tl_chat::reports(opened)? {
        let (Some(Actor::Member(person)), Some(Actor::Member(reporter))) =
            (&report.person, &report.by)
        else {
            continue;
        };
        moderation.reports.push(Report {
            id: report.at.id_string(),
            reporter: reporter.clone(),
            person: person.clone(),
            app: Some(tl_chat::app().to_owned()),
            record: Some(report.conversation.id_string()),
            item: report.message.as_ref().map(ResourceRef::id_string),
            reason: if report.reason.is_empty() {
                format!("Reported: {}", report.said)
            } else {
                format!("{} (it said: {})", report.reason, report.said)
            },
            at_ms: millis(&report.when),
            decision: (!report.decision.is_empty()).then(|| report.decision.clone()),
        });
    }
    Ok(moderation)
}

/// How a decision reads on the reports it settles.
fn decision_text(decide: &DecideReports) -> String {
    let ruling = &decide.ruling;
    let outcome = match ruling.outcome {
        Outcome::NoAction => "No action".to_owned(),
        Outcome::Restrict => match ruling
            .until_ms
            .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
        {
            Some(until) => format!("Restricted until {}", until.strftime("%Y-%m-%d %H:%M UTC")),
            None => "Restricted".to_owned(),
        },
        Outcome::Ban => "Banned".to_owned(),
    };
    let reasoning = ruling.reasoning.trim();
    if reasoning.is_empty() {
        format!("{outcome}.")
    } else {
        format!("{outcome}: {reasoning}")
    }
}

/// A membership change the moderation agent makes in the workspace it moderates, recorded as
/// the bot.
pub trait Governance {
    /// Make `change`, as the workspace decides it under the bot's grants.
    ///
    /// # Errors
    /// The bot's grants do not allow it, or it could not be committed.
    fn change(&mut self, change: Change) -> Result<(), String>;
}

/// The workspace as the moderation agent reaches it: its Chat, opened on the host, changed as
/// the moderation bot, and the workspace's own governance of its members.
pub struct ChatApi<'a> {
    opened: &'a mut Opened,
    now_ms: i64,
    /// Every report filed when it opened, by id.
    reports: std::collections::BTreeMap<String, ResourceRef>,
    /// The workspace's governance, when the agent runs on the host that governs it.
    governance: Option<&'a mut dyn Governance>,
}

impl<'a> ChatApi<'a> {
    /// The API over the workspace's Chat `opened`, reading and changing it as the moderation
    /// bot.
    pub fn new(opened: &'a mut Opened, now_ms: i64) -> Self {
        Self::with_governance(opened, now_ms, None)
    }

    /// [`Self::new`] with `governance`, the workspace's own membership actions the agent makes
    /// under its grants.
    pub fn with_governance(
        opened: &'a mut Opened,
        now_ms: i64,
        governance: Option<&'a mut dyn Governance>,
    ) -> Self {
        let reports = tl_chat::reports(opened)
            .unwrap_or_default()
            .into_iter()
            .map(|report| (report.at.id_string(), report.at))
            .collect();
        Self {
            opened,
            now_ms,
            reports,
            governance,
        }
    }

    /// The workspace's reports as Chat holds them now ([`moderation`]).
    ///
    /// # Errors
    /// Chat's package is not loaded.
    pub fn moderation(&self) -> Result<Moderation, String> {
        moderation(self.opened)
    }
}

impl WorkspaceApi for ChatApi<'_> {
    fn now_ms(&self) -> i64 {
        self.now_ms
    }

    /// What `person` wrote in Chat since `since_ms`, each message one change of the
    /// conversation it is in.
    fn activity(&mut self, person: &MemberId, since_ms: i64) -> Vec<Activity> {
        let Ok(all) = tl_chat::all_messages(self.opened) else {
            return Vec::new();
        };
        let mut activity: Vec<Activity> = all
            .into_iter()
            .filter(
                |(_, message)| matches!(&message.who, Some(Actor::Member(who)) if who == person),
            )
            .filter_map(|(conversation, message)| {
                let time_ms = millis(&message.when);
                (time_ms >= since_ms).then(|| Activity {
                    app: tl_chat::app().to_owned(),
                    record: conversation.id_string(),
                    change: message.at.id_string(),
                    before: None,
                    time_ms,
                    actor: tilo_actions::Actor::Member(person.clone()),
                    what: format!(
                        "send {}",
                        message.text.chars().take(EXCERPT_CHARS).collect::<String>()
                    ),
                    destructive: false,
                    content: true,
                })
            })
            .collect();
        activity.sort_by_key(|item| item.time_ms);
        activity
    }

    /// Makes the change in the workspace's own governance, as the moderation bot: the
    /// workspace's policy decides it under the grants a person gave the bot when they turned it
    /// on, and history names the bot.
    fn govern(&mut self, change: Change) -> Result<(), String> {
        let governance = self
            .governance
            .as_mut()
            .ok_or("this record is read without the workspace's governance")?;
        governance.change(change)
    }

    /// Takes back what a person wrote: `undo { by, steps }` on a conversation deletes the
    /// `steps` newest messages `by` wrote there, with Chat's `delete`.
    fn invoke(
        &mut self,
        app: &str,
        record: &str,
        action: &str,
        args: serde_json::Value,
        _run: &str,
    ) -> Result<(), String> {
        if app != tl_chat::app() || action != "undo" {
            return Err(format!(
                "the moderation agent does not {action} {app}/{record}"
            ));
        }
        let by: tilo_actions::Actor = args
            .get("by")
            .cloned()
            .and_then(|by| serde_json::from_value(by).ok())
            .ok_or("an undo names whose changes it takes back")?;
        let steps = args
            .get("steps")
            .and_then(serde_json::Value::as_u64)
            .and_then(|steps| usize::try_from(steps).ok())
            .ok_or("an undo of messages counts them")?;
        let mut theirs: Vec<(ResourceRef, ResourceRef, i64)> = tl_chat::all_messages(self.opened)?
            .into_iter()
            .filter(|(conversation, message)| {
                conversation.id_string() == record && message.who.as_ref() == Some(&by)
            })
            .map(|(conversation, message)| {
                let when = millis(&message.when);
                (conversation, message.at, when)
            })
            .collect();
        theirs.sort_by_key(|(_, _, when)| std::cmp::Reverse(*when));
        let bot = bot();
        for (conversation, message, _) in theirs.into_iter().take(steps) {
            tl_chat::delete(self.opened, &bot, &conversation, &message)?;
        }
        Ok(())
    }

    /// Settles each report the decision answers with Chat's `decide`, saying what was decided
    /// and why.
    fn decide(&mut self, decision: DecideReports) -> Result<(), String> {
        let text = decision_text(&decision);
        let bot = bot();
        for id in &decision.reports {
            let report = self
                .reports
                .get(id)
                .ok_or_else(|| format!("no report {id} is filed"))?;
            tl_chat::decide(self.opened, &bot, report, &text)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
