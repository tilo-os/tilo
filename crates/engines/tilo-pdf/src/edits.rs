//! Edits materialised into a new PDF on export.
//!
//! The engine-facing projection of a document's edit records: output pages by
//! their source, annotations as standard PDF kinds with geometry in each
//! source page's user space, and form values by fully qualified field name.
//! Exporting the same [`Edits`] over the same source bytes is deterministic.
//!
//! An annotation in the source bytes that a record took over
//! ([`Edits::adopted`]) is replaced, never duplicated: unchanged keeps its
//! original object, changed is written again from its record, deleted is
//! removed.

use serde::{Deserialize, Serialize};

use crate::geom::{Point, Rect, Rotation};
use crate::model::{AnnotationKind, ReplyType, Rgba};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageSource {
    /// 0-based page of the document being exported.
    Original(usize),
    /// Page `page` of the `import`-th document passed alongside the export.
    Imported { import: usize, page: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageEdit {
    pub source: PageSource,
    /// Rotation added to the source page's own `/Rotate`.
    pub rotation: Rotation,
    /// The occurrence that keeps the page's adopted annotations
    /// ([`Edits::adopted`]); a page used more than once keeps them on its primary
    /// only, the others being copies of the page content.
    #[serde(default = "primary_default")]
    pub primary: bool,
}

fn primary_default() -> bool {
    true
}

impl PageEdit {
    #[must_use]
    pub fn new(source: PageSource, rotation: Rotation) -> Self {
        Self {
            source,
            rotation,
            primary: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NewAnnotationKind {
    /// Text markup; `quads` are the marked line rectangles in user space.
    Highlight {
        quads: Vec<Rect>,
    },
    Underline {
        quads: Vec<Rect>,
    },
    StrikeOut {
        quads: Vec<Rect>,
    },
    Squiggly {
        quads: Vec<Rect>,
    },
    /// A sticky note at a user-space anchor; its text is the annotation's
    /// `contents`.
    Note {
        at: Point,
    },
    /// Freehand strokes in user space.
    Ink {
        strokes: Vec<Vec<Point>>,
        width: f32,
    },
    /// Text drawn on the page inside `rect`, wrapped to its width: `contents`,
    /// in the annotation's `color`, with a font embedded for the glyphs it uses.
    FreeText {
        rect: Rect,
        font_size: f32,
    },
    /// Strokes and text lines drawn in `color`: how a signature (drawn ink or a
    /// typed name with its date) is written so that every reader shows it.
    Stamp {
        rect: Rect,
        strokes: Vec<Vec<Point>>,
        stroke_width: f32,
        text: Vec<StampText>,
    },
}

/// One line of text in a [`NewAnnotationKind::Stamp`] appearance, set in the
/// pinned font that covers it, embedded as a subset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StampText {
    /// Start of the baseline, in user space.
    pub at: Point,
    pub size: f32,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewAnnotation {
    /// Output page index into [`Edits::pages`], or the original page index when
    /// `pages` is empty.
    pub page: usize,
    pub kind: NewAnnotationKind,
    pub color: Rgba,
    /// `/T`.
    pub author: String,
    /// `/Contents`.
    pub contents: String,
    /// `/NM`: a stable unique name (the record id), so a re-export names the same
    /// annotation the same way.
    pub name: String,
    /// `/M` as a PDF date string (`D:YYYYMMDDHHmmSSZ`); empty to omit. From the
    /// record, never the clock, so export stays deterministic.
    pub modified: String,
    /// `/IRT`: the [`Self::name`] of another annotation in the same [`Edits`], on
    /// the same output page, that this one answers.
    #[serde(default)]
    pub in_reply_to: Option<String>,
    /// `/RT`, written with [`Self::in_reply_to`].
    #[serde(default)]
    pub reply_type: ReplyType,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationRef {
    /// 0-based page of the source document.
    pub page: usize,
    /// Index in that page's `/Annots`.
    pub index: usize,
    /// `/NM` when set: the address that survives other annotations being added or
    /// removed.
    pub name: String,
    /// The subtype found there, checked before anything is removed.
    pub kind: AnnotationKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptedAnnotation {
    pub target: AnnotationRef,
    /// The record says exactly what the original says: keep the original object on
    /// the primary occurrence. Otherwise it is removed there (a changed record is
    /// written again as a [`NewAnnotation`], a deleted one is gone). Copies of a
    /// page never have adopted annotations.
    pub unchanged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldValue {
    Text(String),
    /// Check box or radio state; for a radio group, the widget whose export value
    /// is `export` is turned on.
    Checked {
        on: bool,
        export: Option<String>,
    },
    Choice(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormValue {
    /// Fully qualified field name.
    pub name: String,
    pub value: FieldValue,
}

/// Everything an export writes; the default exports the document unchanged.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Edits {
    /// The output pages in order. Empty keeps the original pages as they are.
    pub pages: Vec<PageEdit>,
    pub annotations: Vec<NewAnnotation>,
    pub form_values: Vec<FormValue>,
    /// Annotations of the source bytes owned by edit records.
    #[serde(default)]
    pub adopted: Vec<AdoptedAnnotation>,
}

impl Edits {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
            && self.annotations.is_empty()
            && self.form_values.is_empty()
            && self.adopted.iter().all(|a| a.unchanged)
    }
}
