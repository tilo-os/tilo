//! What a reference inside rich text refers to: the one form native apps and tilolang write.
//!
//! A reference is a run of rich text marked [`MARK`] (`@Ada`, `#general`, `Fix the build`):
//! the run is what the text says, the mark's value names what it refers to. A member is named
//! by their [`MemberId`]'s serde form; any other resource by the catalogue's resource reference
//! ([`ResourceAt`]: the app that keeps it, its record when not the app's record of the
//! workspace, its kind and its id), the same reference an action parameter that picks a
//! resource takes ([`tilo_actions::Pick::Resource`]). It names no workspace: a reference is
//! resolved within the workspace whose document holds it. Its label is the run, so the mark
//! carries none.

use serde_json::{Map, Value as Json};
use tilo_actions::MemberId;

use crate::{ResourceAt, ResourceRef};

/// The mark a reference's run carries, valued with what it refers to ([`Embed::mark_value`]).
pub const MARK: &str = "mention";

/// What a reference refers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Embed {
    /// A person, agent or bot.
    Member(MemberId),
    /// A resource of the workspace, by the catalogue's reference to it. Its label is empty:
    /// the run is what the text calls it, and its app's title is what a reader sees.
    Resource(ResourceAt),
}

impl Embed {
    /// A reference to the resource `id` of kind `kind` in `app`'s record of the workspace.
    #[must_use]
    pub fn resource(
        app: impl Into<String>,
        kind: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self::Resource(ResourceAt {
            app: app.into(),
            record: None,
            reference: ResourceRef::whole(kind, id, String::new()),
        })
    }

    /// What a [`MARK`] mark's value refers to; `None` for an empty value or a malformed
    /// reference.
    #[must_use]
    pub fn of_mark(value: &str) -> Option<Self> {
        if value.is_empty() {
            return None;
        }
        if let Some(member) = MemberId::of_mark_value(value) {
            return Some(Self::Member(member));
        }
        if value.starts_with('{') {
            return resource_of_json(value).map(Self::Resource);
        }
        None
    }

    /// The [`MARK`] mark's value naming it, in the one form: a member's id as JSON
    /// (`{"person":…}`), or a resource's reference as JSON (`{"app":…,"id":…,"kind":…}`, with
    /// its record and span when it has them).
    #[must_use]
    pub fn mark_value(&self) -> String {
        match self {
            Self::Member(member) => member.mark_value(),
            Self::Resource(at) => {
                let mut fields = Map::new();
                fields.insert("app".into(), at.app.clone().into());
                if let Some(record) = &at.record {
                    fields.insert("record".into(), record.clone().into());
                }
                fields.insert("kind".into(), at.reference.kind.clone().into());
                fields.insert("id".into(), at.reference.id.clone().into());
                if let Some(span) = at.reference.span
                    && let Ok(span) = serde_json::to_value(span)
                {
                    fields.insert("span".into(), span);
                }
                Json::Object(fields).to_string()
            }
        }
    }

    /// The resource it refers to, when it refers to one rather than a member.
    #[must_use]
    pub fn as_resource(&self) -> Option<&ResourceAt> {
        match self {
            Self::Resource(at) => Some(at),
            Self::Member(_) => None,
        }
    }

    /// The member it names, when it names one.
    #[must_use]
    pub fn as_member(&self) -> Option<&MemberId> {
        match self {
            Self::Member(member) => Some(member),
            Self::Resource(_) => None,
        }
    }
}

/// How a reference reads in plain text for one reader: the same as its chip reads for them.
///
/// A reference's run keeps the title its resource had when the text was written; plain text
/// derived from rich text (an excerpt, a row, a notification, what search shows and matches)
/// never reads that run to a reader who cannot read the resource, and reads the title the
/// resource has now to one who can ([`read_text`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reading {
    /// A resource the reader holds, by what it is called now.
    Title(String),
    /// A resource the reader cannot read: only that something is there ([`UNSHARED`]).
    Unshared,
    /// As the run says: a person, or a resource only its own app can say how it reads.
    AsWritten,
}

/// What a reference to a resource the reader cannot read says, in the source language: the
/// wording of the chip (`rich-text-reference-unreadable`), where no catalogue says it in the
/// reader's language.
pub const UNSHARED: &str = "Not shared with you";

/// The id of the phrase [`UNSHARED`] is, in every catalogue that says it.
pub const UNSHARED_PHRASE: &str = "rich-text-reference-unreadable";

/// `text` as one reader reads it: each run of a [`MARK`] mark (`runs`: its start and end, in
/// characters, and the mark's value) read as `read` says ([`Reading`]), `unshared` for a
/// resource they cannot read, and the rest of the text as written. A run that overlaps one
/// before it, or names nothing, reads as written.
pub fn read_text<'a>(
    text: &str,
    runs: impl IntoIterator<Item = (usize, usize, &'a str)>,
    read: &mut dyn FnMut(&Embed) -> Reading,
    unshared: &str,
) -> String {
    let mut runs: Vec<(usize, usize, &str)> = runs
        .into_iter()
        .filter(|(start, end, _)| start < end)
        .collect();
    if runs.is_empty() {
        return text.to_owned();
    }
    runs.sort_by_key(|(start, end, _)| (*start, *end));
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    for (start, end, value) in runs {
        if start < at || end > chars.len() {
            continue;
        }
        let said = match Embed::of_mark(value).map(|embed| read(&embed)) {
            Some(Reading::Title(title)) if !title.trim().is_empty() => title,
            Some(Reading::Unshared) => unshared.to_owned(),
            Some(Reading::Title(_) | Reading::AsWritten) | None => continue,
        };
        out.extend(chars.get(at..start).unwrap_or_default());
        out.push_str(&said);
        at = end;
    }
    out.extend(chars.get(at..).unwrap_or_default());
    out
}

/// Whether a [`MARK`] mark's value names a resource rather than a member.
#[must_use]
pub fn names_resource(value: &str) -> bool {
    matches!(Embed::of_mark(value), Some(Embed::Resource(_)))
}

fn resource_of_json(value: &str) -> Option<ResourceAt> {
    let Ok(Json::Object(mut fields)) = serde_json::from_str::<Json>(value) else {
        return None;
    };
    fields
        .entry("label")
        .or_insert_with(|| Json::String(String::new()));
    fields.entry("span").or_insert(Json::Null);
    let at: ResourceAt = serde_json::from_value(Json::Object(fields)).ok()?;
    (!at.app.is_empty() && !at.reference.id.is_empty()).then_some(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;

    #[test]
    fn a_resource_reference_round_trips_in_the_one_form() {
        let task = Embed::resource(
            "so.tilo.lang.tasks",
            "tl.so.tilo.lang.tasks.Task",
            "tl:so.tilo.lang.tasks:t1",
        );
        let written = task.mark_value();
        assert!(written.starts_with('{'), "{written}");
        assert!(names_resource(&written));
        assert_eq!(Embed::of_mark(&written), Some(task));

        let mut part = ResourceAt {
            app: "so.tilo.notes".into(),
            record: Some("note:plan".into()),
            reference: ResourceRef::ranged("notes.block", "b7", "", Span::chars(2, 9)),
        };
        assert_eq!(
            Embed::of_mark(&Embed::Resource(part.clone()).mark_value()),
            Some(Embed::Resource(part.clone()))
        );
        // The label is the run's, never the mark's.
        part.reference.label = "Plan".into();
        assert!(!Embed::Resource(part).mark_value().contains("Plan"));
    }

    #[test]
    fn a_member_is_named_by_member_id() {
        let ada = Embed::Member(MemberId::person("u-ada"));
        assert_eq!(ada.mark_value(), r#"{"person":"u-ada"}"#);
        assert!(!names_resource(&ada.mark_value()));
        assert_eq!(Embed::of_mark(&ada.mark_value()), Some(ada));
        let tide = Embed::Member(MemberId::agent("tide", "ada"));
        assert_eq!(Embed::of_mark(&tide.mark_value()), Some(tide));
        assert_eq!(Embed::of_mark("u-ada"), None);
        assert_eq!(Embed::of_mark(""), None);
    }

    #[test]
    fn plain_text_reads_each_reference_for_its_reader() {
        let held = Embed::resource("so.tilo.lang.board", "", "tl:so.tilo.lang.board:d/f:t1");
        let hidden = Embed::resource("so.tilo.lang.board", "", "tl:so.tilo.lang.board:other");
        // `Ask @Ada about Fix it, not Acquisition plan.` as written.
        let text = "Ask @Ada about Fix it, not Acquisition plan.";
        let held_value = held.mark_value();
        let hidden_value = hidden.mark_value();
        let ada = Embed::Member(MemberId::person("u-ada")).mark_value();
        let runs = [
            (4, 8, ada.as_str()),
            (15, 21, held_value.as_str()),
            (27, 43, hidden_value.as_str()),
        ];
        let mut read = |embed: &Embed| match embed {
            Embed::Member(_) => Reading::AsWritten,
            other if *other == held => Reading::Title("Fix the release build".into()),
            Embed::Resource(_) => Reading::Unshared,
        };
        assert_eq!(
            read_text(text, runs, &mut read, UNSHARED),
            "Ask @Ada about Fix the release build, not Not shared with you."
        );
        // A run past the text, or one overlapping another, reads as written.
        let mut all_unshared = |_: &Embed| Reading::Unshared;
        assert_eq!(
            read_text(
                "ab",
                [(0, 9, hidden_value.as_str())],
                &mut all_unshared,
                "x"
            ),
            "ab"
        );
        assert_eq!(
            read_text(
                "abcd",
                [(0, 2, hidden_value.as_str()), (1, 3, held_value.as_str())],
                &mut all_unshared,
                "x"
            ),
            "xcd"
        );
    }
}
