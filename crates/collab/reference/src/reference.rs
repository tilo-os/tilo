//! [`ResourceRef`] — the durable address of the thing a remark is about, and
//! [`Referent`], the one method an app implements to mint one from whatever it
//! currently has selected.

use serde::{Deserialize, Serialize};

use qos_actions::ResourceId;

/// A byte range inside the referenced resource's text.
///
/// Byte offsets, matching the offsets the apps' own text actions already use
/// (`SpliceText`'s `from`/`to`), so a consumer that wants to *act* on the range
/// can hand these straight to the app's action without a coordinate change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        let (start, end) = if start <= end { (start, end) } else { (end, start) };
        Self { start, end }
    }

    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The slice this span covers, or `None` when the offsets do not land on
    /// character boundaries of `text` — a span carried across an edit that
    /// moved it. `None` rather than a panic or a silently wrong slice: a
    /// reference that no longer fits its resource must be *reported*, not
    /// guessed at.
    pub fn slice<'a>(&self, text: &'a str) -> Option<&'a str> {
        text.get(self.start..self.end)
    }
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
pub trait Referent {
    fn selection_reference(&self) -> Option<ResourceRef>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_normalises_a_backwards_selection() {
        // Dragging right-to-left must address the same range as left-to-right.
        assert_eq!(Span::new(11, 6), Span::new(6, 11));
        assert_eq!(Span::new(11, 6).len(), 5);
    }

    #[test]
    fn a_span_that_no_longer_fits_reports_rather_than_guesses() {
        assert_eq!(Span::new(6, 11).slice("Hello world"), Some("world"));
        assert_eq!(Span::new(6, 99).slice("Hello world"), None);
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
