//! What a reading page is set from: a chapter's blocks with their inline styling, and how a
//! reader set them (text size, line spacing, justified or ragged).
//!
//! The model is the layout's own, built from a document's rich text
//! ([`super::from_rich`]): a block is a flow-level unit of a chapter, a span an inline run of
//! uniform styling. Styling — size, weight, indent — is derived from the kind at layout time,
//! never stored here.

/// How a paragraph fits its column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompositionStyle {
    /// Justified, with hyphenation in the book's language.
    Book,
    /// Ragged right: the reader's default, since justifying needs hyphenation a book may not
    /// declare.
    #[default]
    Ragged,
}

impl CompositionStyle {
    /// The style a name reads as (`"book"` or `"ragged"`), ragged for anything else.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "book" => Self::Book,
            _ => Self::Ragged,
        }
    }

    #[must_use]
    pub const fn justified(self) -> bool {
        matches!(self, Self::Book)
    }
}

/// How a reader sets the page: the text's size against the base, its line spacing, and how its
/// paragraphs fit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReadingStyle {
    /// Text size as a multiple of [`Self::BODY_CONTENT_PX`], within
    /// [`Self::SCALE_MIN`]..=[`Self::SCALE_MAX`].
    pub font_scale: f32,
    /// Line spacing as a multiple of the face's own 1.25, within
    /// [`Self::SPACING_MIN`]..=[`Self::SPACING_MAX`].
    pub line_spacing: f32,
    pub composition: CompositionStyle,
}

impl Default for ReadingStyle {
    fn default() -> Self {
        Self {
            font_scale: 1.0,
            line_spacing: 1.2,
            composition: CompositionStyle::Ragged,
        }
    }
}

impl ReadingStyle {
    pub const SCALE_MIN: f32 = 0.8;
    pub const SCALE_MAX: f32 = 2.0;
    pub const SPACING_MIN: f32 = 0.95;
    pub const SPACING_MAX: f32 = 1.6;
    /// Base body size in logical px before the scale multiplier.
    pub const BODY_CONTENT_PX: f32 = 19.0;

    /// The style `scale` names, clamped into what a reader can choose.
    #[must_use]
    pub fn with_scale(scale: f32) -> Self {
        let scale = if scale.is_finite() {
            scale.clamp(Self::SCALE_MIN, Self::SCALE_MAX)
        } else {
            1.0
        };
        Self {
            font_scale: scale,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn body_size(&self) -> f32 {
        Self::BODY_CONTENT_PX * self.font_scale
    }

    /// Body size including a chosen face's size multiplier.
    #[must_use]
    pub fn body_size_with(&self, font_mult: f32) -> f32 {
        Self::BODY_CONTENT_PX * self.font_scale * font_mult
    }
}

/// A flow-level block: a heading, a paragraph, a quote, a verse line, a list item, a display
/// formula or a section break.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// A heading, `level` 1-based; levels past 3 are set as 3.
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph {
        spans: Vec<Span>,
    },
    /// A quoted passage: indented with a rule.
    Quote {
        spans: Vec<Span>,
    },
    /// A stanza: each run whose [`Span::line_break`] is set starts a new line, set ragged with a
    /// hanging indent. `quoted` verse sits inside a quote.
    Verse {
        spans: Vec<Span>,
        quoted: bool,
    },
    /// One list item: `ordered` says a number rather than a bullet, `index` which number.
    ListItem {
        spans: Vec<Span>,
        ordered: bool,
        index: usize,
    },
    /// A display formula, laid out by the toolkit's shared math.
    Math {
        expr: ui::math::MathExpr,
        /// Its linear form, for a reader's text and for search.
        source: String,
        display: bool,
    },
    /// A section break.
    Separator,
}

impl Block {
    #[must_use]
    pub fn spans(&self) -> &[Span] {
        match self {
            Self::Heading { spans, .. }
            | Self::Paragraph { spans }
            | Self::Quote { spans }
            | Self::Verse { spans, .. }
            | Self::ListItem { spans, .. } => spans,
            Self::Math { .. } | Self::Separator => &[],
        }
    }

    /// The block's plain text: its spans' words, with a space where a word break separated one
    /// from the next.
    #[must_use]
    pub fn plain_text(&self) -> String {
        if let Self::Math { source, .. } = self {
            return source.clone();
        }
        let mut out = String::new();
        for (i, span) in self.spans().iter().enumerate() {
            if i > 0 && span.space_before {
                out.push(' ');
            }
            out.push_str(&span.text);
        }
        out
    }
}

/// An inline run of uniform styling: a word, or a few, set bold, italic or monospace.
///
/// Span text is trimmed at style boundaries: the whitespace separating a run from the one before
/// it is not in `text` but recorded in `space_before`, so `burning <em>for</em>.` is a `for` run
/// with `space_before` and a `.` run without.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "bold/italic/code/space_before/line_break are independent inline flags a span \
              carries in any combination, not a state machine"
)]
pub struct Span {
    pub text: String,
    /// Structured inline math; `text` stays its linear form.
    pub math: Option<ui::math::MathExpr>,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    /// Whether a word break separated this run from the previous one. The first run of a block
    /// starts its line either way.
    pub space_before: bool,
    /// This run starts a new line; it always carries `space_before`.
    pub line_break: bool,
    /// Indentation in ems of the line this run starts (a verse line's depth).
    pub line_indent: u8,
}

impl Span {
    #[must_use]
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            math: None,
            bold: false,
            italic: false,
            code: false,
            space_before: false,
            line_break: false,
            line_indent: 0,
        }
    }
}
