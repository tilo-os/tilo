//! [`ResourceRef`] — the durable address of the thing a remark is about, and
//! [`Referent`], the one method an app implements to mint one from whatever it
//! currently has selected.

use serde::{Deserialize, Serialize};

use qos_actions::{Doc, ResourceId};

/// What [`Span`]'s offsets count.
///
/// Carried explicitly because apps genuinely differ — the word processor's own
/// text actions address blocks in **characters**, while byte offsets are the
/// natural unit elsewhere — and the two agree only for ASCII. A span that did
/// not say which it meant would read correctly in every test written in
/// English and silently address the wrong run the first time someone selected
/// text with an accent in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpanUnit {
    /// Unicode scalar values, counted as `chars()`.
    Chars,
    /// UTF-8 bytes.
    Bytes,
}

/// A range inside the referenced resource's text.
///
/// The offsets are in the unit the *publishing app's own text actions* use (see
/// [`SpanUnit`]), so a consumer that wants to act on the range can hand them
/// straight to that app's action without a coordinate change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub unit: SpanUnit,
}

impl Span {
    /// A span in characters — the unit the word processor's block actions use.
    pub fn chars(start: usize, end: usize) -> Self {
        Self::new(start, end, SpanUnit::Chars)
    }

    /// A span in UTF-8 bytes.
    pub fn bytes(start: usize, end: usize) -> Self {
        Self::new(start, end, SpanUnit::Bytes)
    }

    /// Normalises a backwards range, so dragging right-to-left addresses the
    /// same run as dragging left-to-right.
    pub fn new(start: usize, end: usize, unit: SpanUnit) -> Self {
        let (start, end) = if start <= end { (start, end) } else { (end, start) };
        Self { start, end, unit }
    }

    /// How many units long. Not a character count unless [`Self::unit`] says so.
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The slice this span covers, honouring its unit, or `None` when it does
    /// not fit `text` — a span carried across an edit that moved it.
    ///
    /// `None` rather than a panic or a silently wrong slice: a reference that
    /// no longer fits its resource must be *reported*, not guessed at.
    pub fn slice<'a>(&self, text: &'a str) -> Option<&'a str> {
        match self.unit {
            SpanUnit::Bytes => text.get(self.start..self.end),
            SpanUnit::Chars => text.get(byte_at(text, self.start)?..byte_at(text, self.end)?),
        }
    }
}

/// Byte offset of the `n`th character of `text`, or `text.len()` when `n` names
/// the position just past the last character. `None` when `n` runs past the end.
fn byte_at(text: &str, n: usize) -> Option<usize> {
    if n == 0 {
        return Some(0);
    }
    let mut seen = 0usize;
    for (i, _) in text.char_indices() {
        if seen == n {
            return Some(i);
        }
        seen += 1;
    }
    (seen == n).then_some(text.len())
}

/// A durable, machine-readable address for one thing an app has open.
///
/// ## `id` is the address; `label` is not
///
/// `label` exists so a chip can read `▸ harbour.png` instead of
/// `▸ files.entry:/var/workspace/photos/harbour.png`. It is presentation: the
/// app's current wording, in the author's language, at the moment of writing.
/// Nothing may resolve, compare or route on it. `kind` + `id` are the address,
/// and they are what [`resource_id`](Self::resource_id) hands back.
///
/// Keeping those apart is the whole point of the crate. The instant a consumer
/// starts matching on `label`, the reference has degraded into the string it
/// was supposed to replace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRef {
    /// Stable kind tag — `word_processor.block`, `files.entry`. Shared with the
    /// resource trees apps publish through `qos-actions` and with the OS-wide
    /// type registry, so the same kind means the same thing across apps.
    pub kind: String,
    /// The resource's stable id, in the publishing app's own vocabulary. This
    /// is the same string the app uses as its
    /// [`ResourceId`](qos_actions::ResourceId) when it publishes the resource,
    /// so an address minted here and an address surveyed through the door are
    /// the same address.
    pub id: String,
    /// Human, localisable label for the chip. **Never** the address.
    pub label: String,
    /// The part of the resource the remark is about, when it is about a part.
    /// `None` means the whole thing (a file, a document).
    pub span: Option<Span>,
}

impl ResourceRef {
    /// A reference to a whole resource.
    pub fn whole(
        kind: impl Into<String>,
        id: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
            label: label.into(),
            span: None,
        }
    }

    /// A reference to a range within a resource.
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

    /// The address as the publishing app's own [`ResourceId`].
    ///
    /// The bridge to `qos-actions`: hand this to the door's `resolve` against a
    /// live resource tree to find the resource, and to its `survey` output to
    /// find what can be *done* to it.
    pub fn resource_id(&self) -> ResourceId {
        ResourceId::new(self.id.clone())
    }

    /// Whether this reference points at part of a resource rather than all of
    /// it.
    pub fn is_partial(&self) -> bool {
        self.span.is_some()
    }
}

/// What an app answers when asked "what is selected right now?".
///
/// One method, so *any* app can join the gesture: the compose surface never
/// knows which app it is talking to, only that something answered with an
/// address. That is what makes "select anything and say something about it"
/// general rather than a per-app feature.
///
/// Returning `None` means nothing is selected — the gesture is simply not
/// offered, rather than a reference to nothing being minted.
///
/// Takes the shared document for the same reason
/// [`ResourceProvider`](qos_actions::ResourceProvider) does: the label, the
/// ordinal and often the id itself are read from the CRDT rather than from
/// widget fields, so the address is the document's answer and not the paint
/// layer's.
pub trait Referent {
    fn selection_reference(&self, doc: &Doc) -> Option<ResourceRef>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_normalises_a_backwards_selection() {
        // Dragging right-to-left must address the same range as left-to-right.
        assert_eq!(Span::chars(11, 6), Span::chars(6, 11));
        assert_eq!(Span::chars(11, 6).len(), 5);
    }

    #[test]
    fn a_span_that_no_longer_fits_reports_rather_than_guesses() {
        assert_eq!(Span::bytes(6, 11).slice("Hello world"), Some("world"));
        assert_eq!(Span::bytes(6, 99).slice("Hello world"), None);
        assert_eq!(Span::chars(6, 99).slice("Hello world"), None);
    }

    /// The reason the unit is on the span at all: on non-ASCII text a char
    /// range and a byte range address different runs, and an app whose actions
    /// count characters would otherwise hand over offsets that quietly slice
    /// the wrong words.
    #[test]
    fn a_char_span_and_a_byte_span_address_different_runs() {
        // "é" is two bytes, so every offset after it disagrees.
        let text = "café notes";
        assert_eq!(Span::chars(5, 10).slice(text), Some("notes"));
        assert_eq!(Span::bytes(5, 10).slice(text), Some(" note"));
        assert_ne!(
            Span::chars(5, 10).slice(text),
            Span::bytes(5, 10).slice(text),
            "if these ever agree the test has lost its point"
        );
    }

    #[test]
    fn a_char_span_may_end_just_past_the_last_character() {
        assert_eq!(Span::chars(0, 4).slice("café"), Some("café"));
        assert_eq!(Span::chars(3, 4).slice("café"), Some("é"));
    }

    #[test]
    fn the_address_survives_a_relabel() {
        // The same resource, labelled two different ways (renamed, or simply a
        // different author's language). The address must not move.
        let a = ResourceRef::whole("files.entry", "files.entry:/w/harbour.png", "harbour.png");
        let b = ResourceRef::whole("files.entry", "files.entry:/w/harbour.png", "Hafen.png");
        assert_eq!(a.resource_id(), b.resource_id());
        assert_ne!(a.label, b.label);
    }
}
