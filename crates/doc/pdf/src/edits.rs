//! Edits materialised into a new PDF on export.
//!
//! An [`Edits`] value is the engine-facing projection of a document's edit
//! records: it names output pages by their source, carries annotations as
//! standard PDF annotation kinds with geometry in each source page's **user
//! space**, and form values by fully qualified field name. Exporting the
//! same `Edits` against the same source bytes is deterministic.

use serde::{Deserialize, Serialize};

use crate::geom::{Point, Rect, Rotation};
use crate::model::Rgba;

/// Where an output page comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageSource {
    /// Page `index` (0-based) of the document being exported.
    Original(usize),
    /// Page `page` of the `import`-th document passed alongside the export.
    Imported { import: usize, page: usize },
}

/// One output page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageEdit {
    pub source: PageSource,
    /// Rotation added to the source page's own `/Rotate`.
    pub rotation: Rotation,
}

/// The annotation types an export can write.
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
    /// A sticky note anchored at a user-space point; its text is the
    /// annotation's `contents`.
    Note {
        at: Point,
    },
    /// Freehand strokes in user space.
    Ink {
        strokes: Vec<Vec<Point>>,
        width: f32,
    },
    /// Text drawn directly on the page inside `rect`; the text is
    /// `contents`.
    FreeText {
        rect: Rect,
        font_size: f32,
    },
}

/// An annotation to add to an output page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewAnnotation {
    /// Output page index (into [`Edits::pages`], or the original page
    /// index when `pages` is empty).
    pub page: usize,
    pub kind: NewAnnotationKind,
    pub color: Rgba,
    /// `/T`.
    pub author: String,
    /// `/Contents`.
    pub contents: String,
    /// `/NM` — a stable unique name (the record id), so a re-export names
    /// the same annotation the same way.
    pub name: String,
    /// `/M` as a PDF date string (`D:YYYYMMDDHHmmSSZ`); empty to omit. Taken
    /// from the record rather than the clock so export stays deterministic.
    pub modified: String,
}

/// A value for a form field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldValue {
    Text(String),
    /// Check box / radio state. For a radio group, the widget whose export
    /// value is `export` is turned on.
    Checked {
        on: bool,
        export: Option<String>,
    },
    /// The selected option of a combo or list box.
    Choice(String),
}

/// A form field assignment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormValue {
    /// Fully qualified field name.
    pub name: String,
    pub value: FieldValue,
}

/// Everything an export writes. The default value exports the document
/// unchanged.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Edits {
    /// The output pages in order. Empty keeps the original pages as they are.
    pub pages: Vec<PageEdit>,
    pub annotations: Vec<NewAnnotation>,
    pub form_values: Vec<FormValue>,
}

impl Edits {
    /// Whether the export would reproduce the source unchanged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty() && self.annotations.is_empty() && self.form_values.is_empty()
    }
}
