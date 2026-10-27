//! The durable address of a thing in a document.

use serde::{Deserialize, Serialize};

use tilo_actions::{Doc, ResourceId};

/// What [`Span`]'s offsets count.
///
/// Explicit because apps differ — the word processor's text actions address
/// blocks in characters, byte offsets are natural elsewhere — and the two agree
/// only for ASCII, so an implicit unit reads correctly in every English test and
/// addresses the wrong run on the first accented character.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
pub enum SpanUnit {
    /// Unicode scalar values, counted as `chars()`.
    Chars,
    /// UTF-8 bytes.
    Bytes,
}

/// A range inside the referenced resource's text.
///
/// Offsets are in the publishing app's own unit, so a consumer can hand them
/// straight to that app's actions without a coordinate change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub unit: SpanUnit,
}

impl Span {
    pub fn chars(start: usize, end: usize) -> Self {
        Self::new(start, end, SpanUnit::Chars)
    }

    pub fn bytes(start: usize, end: usize) -> Self {
        Self::new(start, end, SpanUnit::Bytes)
    }

    /// Normalises a backwards range, so drag direction does not change the
    /// addressed run.
    pub fn new(start: usize, end: usize, unit: SpanUnit) -> Self {
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        Self { start, end, unit }
    }

    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `None` when the span no longer fits `text` — a reference carried across
    /// an edit that moved it must be reported, not silently sliced elsewhere.
    pub fn slice<'a>(&self, text: &'a str) -> Option<&'a str> {
        match self.unit {
            SpanUnit::Bytes => text.get(self.start..self.end),
            SpanUnit::Chars => text.get(byte_at(text, self.start)?..byte_at(text, self.end)?),
        }
    }
}

/// Byte offset of the `n`th character, accepting `n` one past the last so a
/// span may end at the end of the text.
fn byte_at(text: &str, n: usize) -> Option<usize> {
    if n == 0 {
        return Some(0);
    }
    let mut seen: usize = 0;
    for (i, _) in text.char_indices() {
        if seen == n {
            return Some(i);
        }
        seen = seen.saturating_add(1);
    }
    (seen == n).then_some(text.len())
}

/// A durable, machine-readable address for one thing an app has open.
///
/// `kind` + `id` are the address; `label` is presentation and nothing may
/// resolve, compare or route on it. That separation is the point of the crate:
/// once a consumer matches on `label`, the reference has degraded back into the
/// prose it was meant to replace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
pub struct ResourceRef {
    /// Stable kind tag (`word_processor.block`, `files.entry`), shared with the
    /// resource trees apps publish so the same kind means the same thing
    /// everywhere.
    #[model(atom)]
    pub kind: String,
    /// The publishing app's own resource id, so an address minted from a
    /// selection and one surveyed through the door are the same address.
    #[model(atom)]
    pub id: String,
    /// Human, localisable label for the chip. Never the address.
    pub label: String,
    /// `None` means the whole resource rather than a part of it.
    pub span: Option<Span>,
}

impl ResourceRef {
    pub fn whole(kind: impl Into<String>, id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
            label: label.into(),
            span: None,
        }
    }

    pub fn ranged(
        kind: impl Into<String>,
        id: impl Into<String>,
        label: impl Into<String>,
        span: Span,
    ) -> Self {
        Self {
            span: Some(span),
            ..Self::whole(kind, id, label)
        }
    }

    /// Bridge to `tilo-actions`: resolves against a live resource tree to find
    /// the resource, and against a survey to find what can be done to it.
    pub fn resource_id(&self) -> ResourceId {
        ResourceId::new(self.id.clone())
    }

    pub fn is_partial(&self) -> bool {
        self.span.is_some()
    }

    /// The one action that opens this reference: its resource's `open`
    /// view in `app`, in workspace `room`, on `record` (`None`: the app's
    /// own record of the workspace). The host launches the app on the record
    /// when no window shows it, and admits the open as the person who asked
    /// under their grants, as it admits any action; a search hit, a chip and
    /// an agent all open things this way.
    #[must_use]
    pub fn open_target(
        &self,
        room: &str,
        app: &str,
        record: Option<&str>,
    ) -> tilo_actions::remote::RemoteActionTarget {
        let open = tilo_actions::remote::APPLICATION_OPEN_ACTION;
        match record {
            Some(record) => tilo_actions::remote::application_record_target(
                room,
                app,
                record,
                self.id.clone(),
                open,
            ),
            None => {
                tilo_actions::remote::application_action_target(room, app, self.id.clone(), open)
            }
        }
    }
}

/// What an app answers when asked what is selected right now.
///
/// One method, so the compose surface never needs to know which app it is
/// talking to — that is what makes "select anything and say something about it"
/// general rather than per-app. Reads the document because the label and often
/// the id live in the CRDT, not in widget fields.
pub trait Referent {
    fn selection_reference(&self, doc: &Doc) -> Option<ResourceRef>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A span normalises its direction, counts in its declared unit, may end
    /// just past the last character, and reports rather than guesses when it
    /// no longer fits.
    #[test]
    fn a_span_slices_exactly_the_run_it_names_or_nothing() {
        // Case: a span normalises a backwards selection.
        assert_eq!(Span::chars(11, 6), Span::chars(6, 11));
        assert_eq!(Span::chars(11, 6).len(), 5);

        // Case: a span that no longer fits reports rather than guesses.
        assert_eq!(Span::bytes(6, 11).slice("Hello world"), Some("world"));
        assert_eq!(Span::bytes(6, 99).slice("Hello world"), None);
        assert_eq!(Span::chars(6, 99).slice("Hello world"), None);

        // Case: a char span and a byte span address different runs.
        // "é" is two bytes, so every offset after it disagrees.
        let text = "café notes";
        assert_eq!(Span::chars(5, 10).slice(text), Some("notes"));
        assert_eq!(Span::bytes(5, 10).slice(text), Some(" note"));
        assert_ne!(
            Span::chars(5, 10).slice(text),
            Span::bytes(5, 10).slice(text),
            "if these ever agree the test has lost its point"
        );

        // Case: a char span may end just past the last character.
        assert_eq!(Span::chars(0, 4).slice("café"), Some("café"));
        assert_eq!(Span::chars(3, 4).slice("café"), Some("é"));
    }

    #[test]
    fn a_reference_opens_through_its_apps_open_on_its_record() {
        let page = ResourceRef::whole("notes.page", "notes.page:1", "Plan");
        let target = page.open_target("room-1", "so.tilo.notes", Some("uuid:7"));
        assert_eq!(
            tilo_actions::remote::application_provider(&target.provider),
            Some(("room-1", "so.tilo.notes"))
        );
        assert_eq!(
            tilo_actions::remote::application_provider_record(&target.provider),
            Some("uuid:7")
        );
        assert_eq!(
            target.address,
            tilo_actions::ActionAddress::new("notes.page:1", "open")
        );
        let target = page.open_target("room-1", "so.tilo.notes", None);
        assert_eq!(
            tilo_actions::remote::application_provider_record(&target.provider),
            None
        );
    }

    #[test]
    fn the_address_survives_a_relabel() {
        let a = ResourceRef::whole("files.entry", "files.entry:/w/harbour.png", "harbour.png");
        let b = ResourceRef::whole("files.entry", "files.entry:/w/harbour.png", "Hafen.png");
        assert_eq!(a.resource_id(), b.resource_id());
        assert_ne!(a.label, b.label);
    }
}
