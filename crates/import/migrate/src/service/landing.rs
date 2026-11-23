//! Where a service source's items land: in the app that owns them (Notes for pages, Tasks for
//! issues), keyed by the item's source so nothing lands twice.

use std::io;

use serde::{Deserialize, Serialize};

/// A picture a page shows, by the BLAKE3 digest its Markdown names it by (`asset:<digest>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub digest: String,
    pub bytes: Vec<u8>,
}

/// Who wrote an item on the service: their name there, and their handle (an address or a user
/// name) when the service gives one. Not a member of the destination: they need not have joined.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingAuthor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

/// A group an issue belongs to on the service: a Linear team or project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingGroup {
    /// The service's id for it.
    pub key: String,
    pub name: String,
}

/// How an issue relates to another one: `blocks`, `blocked-by`, `duplicate-of`, `duplicated-by`
/// or `related`, and the other issue's page.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingRelation {
    pub kind: String,
    pub page: String,
}

/// A page for Notes: an Outline collection or document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingPage {
    /// The service it came from: `outline`.
    pub source: String,
    /// The service's id for it, unique within the source.
    pub key: String,
    pub title: String,
    /// Its words, pictures as `asset:<digest>` and links to other pages as `[[title]]`.
    pub markdown: String,
    /// The key of the page it sits under; none for a top-level page.
    pub parent: Option<String>,
    /// Where it is read on the service.
    pub url: String,
    pub pictures: Vec<Picture>,
    /// Who wrote it there, when the service says.
    pub author: Option<IncomingAuthor>,
    /// When it was written and last changed there (RFC 3339); empty when the service does not
    /// say.
    pub created_at: String,
    pub updated_at: String,
}

/// One comment of an issue's thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingComment {
    pub author: String,
    /// The author's handle there (an address), when the service gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// When it was written, as the service says it (RFC 3339).
    pub at: String,
    /// When it last changed there (RFC 3339); empty when the service does not say.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub updated_at: String,
    /// Its own page on the service, when it has one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    pub text: String,
}

/// An issue for Tasks, as Linear keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingIssue {
    /// The service it came from: `linear`.
    pub source: String,
    /// The service's id for it.
    pub key: String,
    /// How the service names it to people: `ENG-12`.
    pub identifier: String,
    pub title: String,
    /// Its description, as Markdown.
    pub body: String,
    /// The tracker's status: backlog, todo, in-progress, in-review, done or cancelled.
    pub status: String,
    pub labels: Vec<String>,
    /// `outside:<name>`: who it is assigned to there, who need not be a member here.
    pub assignee: Option<String>,
    pub comments: Vec<IncomingComment>,
    /// The pages of the issues its relations name: how Tasks finds them, in this import or an
    /// earlier one.
    pub links: Vec<String>,
    /// The page of the issue it is a sub-issue of.
    pub parent: Option<String>,
    /// Its page on the service: the issue's source.
    pub url: String,
    /// Who opened it there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<IncomingAuthor>,
    /// When it was opened and last changed there (RFC 3339); empty when the service does not
    /// say.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub updated_at: String,
    /// The team it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<IncomingGroup>,
    /// The project it works towards, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<IncomingGroup>,
    /// How urgent it is: none, urgent, high, medium or low.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    /// How much work it is, in points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<u32>,
    /// When it is due (`2026-11-20`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    /// How it relates to the other reviewed issues, by their pages; `links` names the same
    /// pages without how.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<IncomingRelation>,
}

/// Where an import's items go.
pub trait Landing {
    /// How many items it takes in one hand-over; the import saves its journal after each.
    fn batch(&self) -> usize {
        1
    }
    /// Hand `pages` to Notes, durably: which of them it has now, and which it refused and why.
    ///
    /// # Errors
    /// It could not be reached; what it answered for before stays kept on its side.
    fn pages(&mut self, pages: &[IncomingPage]) -> io::Result<Delivered>;
    /// Hand `issues` to Tasks, durably: which of them it has now, and which it refused and why.
    ///
    /// # Errors
    /// It could not be reached; what it answered for before stays kept on its side.
    fn issues(&mut self, issues: &[IncomingIssue]) -> io::Result<Delivered>;
}

/// What a landing did with the items handed to it: the keys of those it has now, and those it
/// refused, each with why. An item in neither was not reached and comes again in the next import.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Delivered {
    pub landed: Vec<String>,
    pub refused: Vec<RefusedItem>,
}

/// An item the destination refused: its key, how people know it (`ENG-12`, a page's title), and
/// the destination's reason.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedItem {
    pub key: String,
    pub name: String,
    pub why: String,
}
