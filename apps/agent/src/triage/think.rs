//! What a model adds to triage, reached through the AI hub.
//!
//! The model is asked through the model gateway's fixed `Transform` verb
//! under the `feedback.triage` task, never a provider or a model name: the
//! person's assignments pick the model. How private the report is travels
//! with the request. A public `#feedback` report is open content; a report
//! from a direct room is sensitive, so the hub serves it on this device
//! unless the person allowed otherwise.
//!
//! The model sees only the report's redacted words and the redacted words of
//! the earlier reports the structure already found alike. It may name one of
//! those as the original, and it may propose a title and a summary. An
//! answer that names anything else is ignored rather than acted on, and
//! without a model the deterministic match and the template draft decide.

use std::time::Duration;

use agent_provider_config::tasks::{FEEDBACK_TRIAGE, PrivacyClass};
use serde::Deserialize;

/// Largest answer a model is asked for.
const MAX_OUTPUT_TOKENS: u32 = 600;

/// How long triage waits on a model before deciding without one.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The instructions every triage request carries. The model's answer is data
/// the agent checks, never an action it takes.
const INSTRUCTIONS: &str = "You triage feedback reports about a desktop operating system. \
The input is JSON: `report` is a new report's text, `candidates` are earlier reports \
({id, text}) that look similar. Answer with JSON only, no prose: \
{\"duplicate_of\": <the id of the candidate describing the same problem, or null>, \
\"title\": <a short issue title for the new report, at most 80 characters>, \
\"summary\": <one or two sentences stating the problem for a developer>}. \
Choose duplicate_of only when both describe the same defect, not merely the same app. \
Placeholder characters (▮) stand for text that was removed; never guess what it was.";

/// An earlier report shown to the model.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Candidate {
    pub id: String,
    pub text: String,
}

/// What triage asks a model about one report.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Ask {
    pub report: String,
    pub candidates: Vec<Candidate>,
}

/// A model's answer, as far as it survived checking.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Judgement {
    /// One of the asked candidates' ids.
    pub duplicate_of: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
}

/// Something that can answer an [`Ask`] for content of a privacy class.
pub trait Think: Send {
    /// `None` when no model answered in time, or its answer was unusable.
    fn judge(&self, privacy: PrivacyClass, ask: &Ask) -> Option<Judgement>;
}

/// No model: the deterministic floor decides everything.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoModel;

impl Think for NoModel {
    fn judge(&self, _privacy: PrivacyClass, _ask: &Ask) -> Option<Judgement> {
        None
    }
}

/// The model the AI hub assigns `feedback.triage` for this agent.
#[derive(Clone, Debug)]
pub struct Hub {
    /// The agent's name, for a per-agent assignment.
    pub agent: String,
}

impl Think for Hub {
    fn judge(&self, privacy: PrivacyClass, ask: &Ask) -> Option<Judgement> {
        if !tilo_model_gateway::available() {
            return None;
        }
        let input = serde_json::to_string(ask).ok()?;
        match tilo_model_gateway::transform_for(
            FEEDBACK_TRIAGE.id,
            Some(&self.agent),
            privacy,
            INSTRUCTIONS,
            &input,
            MAX_OUTPUT_TOKENS,
            TIMEOUT,
        ) {
            Ok(answer) => parse(&answer, ask),
            Err(error) => {
                tracing::info!(%error, ?privacy, "triage: no model answer; deciding without one");
                None
            }
        }
    }
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    duplicate_of: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
}

/// A model's answer to `ask`, checked: the JSON object in it, a duplicate
/// only among the asked candidates, a one-line title of bounded length.
pub fn parse(answer: &str, ask: &Ask) -> Option<Judgement> {
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    let answer: Answer = serde_json::from_str(answer.get(start..=end)?).ok()?;
    let duplicate_of = answer
        .duplicate_of
        .filter(|id| ask.candidates.iter().any(|candidate| &candidate.id == id));
    let title = answer
        .title
        .map(|title| title.trim().to_string())
        .filter(|title| {
            !title.is_empty() && title.chars().count() <= 120 && !title.contains(['\n', '\r'])
        });
    let summary = answer
        .summary
        .map(|summary| summary.trim().to_string())
        .filter(|summary| !summary.is_empty() && summary.chars().count() <= 2_000);
    Some(Judgement {
        duplicate_of,
        title,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask() -> Ask {
        Ask {
            report: "Send does nothing".into(),
            candidates: vec![Candidate {
                id: "m_1".into(),
                text: "Send is broken".into(),
            }],
        }
    }

    #[test]
    fn an_answer_counts_only_for_what_was_asked() {
        let judged = parse(
            "Sure: {\"duplicate_of\":\"m_1\",\"title\":\"Send does nothing\",\"summary\":\"s\"}",
            &ask(),
        )
        .unwrap();
        assert_eq!(judged.duplicate_of.as_deref(), Some("m_1"));
        let invented = parse("{\"duplicate_of\":\"m_999\",\"title\":\"a\\nb\"}", &ask()).unwrap();
        assert_eq!(invented.duplicate_of, None, "only an asked candidate");
        assert_eq!(invented.title, None, "a title is one line");
        assert!(parse("no json here", &ask()).is_none());
    }
}
