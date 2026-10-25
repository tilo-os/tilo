//! The facts a task's check is shown ([`crate::facts`]): those of each
//! record the task changed or acted on that states them, and what changed
//! in it since an earlier task exported it, so the check compares the
//! record with the request (and v2 with v1) as data.

use tilo_actions::ActionOutput;
use tilo_actions::door::ActionDescriptor;
use tilo_browser_peer::DocumentRoute;

use super::AgentParticipant;
use super::running::FollowUp;
use crate::facts::{FACTS, changes_text, facts_read};
use crate::planner::Step;

/// The most records a check is shown the facts of.
const MOST_FACT_RECORDS: usize = 3;

/// The most characters of one record's facts a check is shown.
const MOST_FACT_CHARS: usize = 4_000;

/// The most exports whose facts the agent remembers.
const MOST_EXPORTS: usize = 16;

/// An export whose answer stated the facts of what it exported.
pub(super) struct Exported {
    pub(super) route: DocumentRoute,
    /// The task whose step exported it.
    pub(super) task: String,
    pub(super) content: String,
    pub(super) facts: serde_json::Value,
}

impl AgentParticipant {
    /// Keep the facts `output` states of what `step` of task `task`
    /// exported: an answer naming the content it made beside `facts`.
    pub(super) fn note_export(&mut self, task: &str, step: &Step, output: &ActionOutput) {
        let Some(facts) = output
            .value
            .as_ref()
            .and_then(|value| value.get(FACTS))
            .filter(|facts| facts.is_object())
        else {
            return;
        };
        let Some(content) = super::running::produced(output).into_iter().next() else {
            return;
        };
        let Some(route) = self.running.as_ref().and_then(|running| {
            running
                .touched
                .iter()
                .chain(&running.made)
                .rev()
                .find(|route| route.app_id == step.app)
                .cloned()
        }) else {
            return;
        };
        if self.exported.len() >= MOST_EXPORTS {
            self.exported.remove(0);
        }
        self.exported.push(Exported {
            route,
            task: task.to_owned(),
            content,
            facts: facts.clone(),
        });
    }

    /// What a check of the running task is shown of the facts of the
    /// records it made or acted on, among `open` (each with what it
    /// publishes): each one's facts now, and what changed since an earlier
    /// task's export of it. Nothing on other turns.
    pub(super) fn check_facts(
        &mut self,
        open: &[(DocumentRoute, Vec<ActionDescriptor>)],
    ) -> Vec<String> {
        let Some(running) = self.running.as_ref() else {
            return Vec::new();
        };
        if !matches!(
            running.follow_ups.turn,
            Some(FollowUp::Checking | FollowUp::Obligations)
        ) {
            return Vec::new();
        }
        let task = running.id.clone();
        let mut routes: Vec<DocumentRoute> = Vec::new();
        for route in running.made.iter().chain(&running.touched) {
            if !routes.contains(route) {
                routes.push(route.clone());
            }
        }
        let mut shown = Vec::new();
        let mut records = 0_usize;
        for route in routes {
            if records >= MOST_FACT_RECORDS {
                break;
            }
            let Some(address) = open
                .iter()
                .find(|(open, _)| *open == route)
                .and_then(|(_, descriptors)| facts_read(descriptors))
                .map(|descriptor| descriptor.address.clone())
            else {
                continue;
            };
            let facts = match self.read_now(&route, &address, &serde_json::Map::new()) {
                Ok(ActionOutput {
                    value: Some(facts), ..
                }) => facts,
                Ok(_) => continue,
                Err(error) => {
                    tracing::debug!(%error, "agent: a record's facts could not be read for its check");
                    continue;
                }
            };
            records = records.saturating_add(1);
            shown.push(facts_line(&route, &address.resource, &facts));
            if let Some(earlier) = self
                .exported
                .iter()
                .rev()
                .find(|exported| exported.route == route && exported.task != task)
            {
                shown.push(format!(
                    "(document {}) since an earlier task exported it as {}: {}",
                    route.record,
                    earlier.content,
                    changes_text(&earlier.facts, &facts)
                ));
            }
        }
        shown
    }
}

/// One record's facts as a check is shown them, at most
/// [`MOST_FACT_CHARS`] of them.
fn facts_line(route: &DocumentRoute, resource: &str, facts: &serde_json::Value) -> String {
    let text = crate::planner::compact(facts).to_string();
    let text = if text.chars().count() > MOST_FACT_CHARS {
        format!(
            "{}… (cut)",
            text.chars().take(MOST_FACT_CHARS).collect::<String>()
        )
    } else {
        text
    };
    format!(
        "(document {}) {} {resource}#{FACTS} states it now: {text}",
        route.record, route.app_id
    )
}
