//! What any sub-agent made: a pull request as a document, each a thing one can open.

use serde::{Deserialize, Serialize};
use tilo_resource::{ActionCx, Id, JsonSchema, Model, Refusal};

use crate::WorkspaceAgents;
use crate::sub::{Sub, check_words, one_line, words};

/// The longest document or resource a thing made names, in bytes.
pub const MAX_PATH: usize = 1024;
/// The longest label of something a sub-agent made, in bytes.
pub const MAX_LABEL: usize = 120;
/// The longest address of something a sub-agent made, in bytes.
pub const MAX_URL: usize = 2048;
/// The most things a sub-agent's item lists as made; the oldest leave first.
pub const MAX_PRODUCED: usize = 32;
/// What a thing a sub-agent made may be.
pub const PRODUCED_KINDS: [&str; 6] = [
    "pull-request",
    "change",
    "branch",
    "file",
    "resource",
    "page",
];

/// Where a thing a sub-agent made is in Tilo.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProducedAt {
    /// The app that keeps it, by its id
    pub app: String,
    /// The app's document it is in; empty for the app's own document of the workspace
    #[serde(default)]
    pub document: String,
    /// The resource itself, by its id there: an item, a path
    pub resource: String,
}

/// Something a sub-agent made that one can open.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Produced {
    /// Its id among what its sub-agent made.
    pub id: String,
    /// What it is ([`PRODUCED_KINDS`]).
    pub kind: String,
    /// What a person calls it.
    pub label: String,
    /// Its address outside Tilo: it opens in Browser.
    pub url: Option<String>,
    /// Where it is in Tilo.
    pub at: Option<ProducedAt>,
    /// When it was said, in milliseconds since the Unix epoch.
    pub at_ms: i64,
}

/// Refuses a sub-agent whose list of what it made is none this crate wrote.
pub(crate) fn check(sub: &Sub) -> Result<(), Refusal> {
    if sub.produced.len() > MAX_PRODUCED {
        return Err(Refusal::new(format!(
            "a sub-agent lists at most {MAX_PRODUCED} things it made"
        )));
    }
    for made in &sub.produced {
        if !PRODUCED_KINDS.contains(&made.kind.as_str()) || made.url.is_some() == made.at.is_some()
        {
            return Err(Refusal::new(
                "a thing a sub-agent made has a kind, and an address or a place",
            ));
        }
        check_words(&made.label, MAX_LABEL, "what a sub-agent made")?;
    }
    Ok(())
}

impl WorkspaceAgents {
    /// The body of `sub-agent-produced`: the sub-agent `agent` itself, or its runner as it, says
    /// what it made.
    pub(crate) fn produced(
        &mut self,
        cx: &mut ActionCx,
        agent: &Id,
        (kind, label): (&str, &str),
        url: Option<String>,
        at: Option<ProducedAt>,
    ) -> Result<(), Refusal> {
        if !PRODUCED_KINDS.contains(&kind) {
            return Err(Refusal::new(format!(
                "{kind:?} is no kind of thing made: one of {}",
                PRODUCED_KINDS.join(", ")
            )));
        }
        let label = one_line(label, MAX_LABEL);
        let url = words(url, MAX_URL, "an address")?;
        if label.is_empty() || url.is_some() == at.is_some() {
            return Err(Refusal::new(
                "a thing made has a label, and either its address or its place in Tilo",
            ));
        }
        if url
            .as_deref()
            .is_some_and(|url| !url.starts_with("https://"))
        {
            return Err(Refusal::new(
                "an address a person opens starts with https://",
            ));
        }
        if let Some(at) = &at {
            check_words(&at.app, MAX_LABEL, "the app a thing made is in")?;
            check_words(&at.document, MAX_PATH, "the document a thing made is in")?;
            check_words(&at.resource, MAX_PATH, "the resource a thing made is")?;
            if at.app.trim().is_empty() || at.resource.trim().is_empty() {
                return Err(Refusal::new(
                    "a place in Tilo names its app and the resource there",
                ));
            }
        }
        let (id, at_ms) = (cx.id().to_string(), cx.now_ms());
        let sub = self.as_itself(cx, agent)?;
        // The same thing said again replaces what was said of it.
        sub.produced
            .retain(|made| made.kind != kind || made.url != url || made.at != at);
        sub.produced.push(Produced {
            id,
            kind: kind.to_owned(),
            label,
            url,
            at,
            at_ms,
        });
        let over = sub.produced.len().saturating_sub(MAX_PRODUCED);
        sub.produced.drain(..over);
        Ok(())
    }
}
