//! What a reader shows: a document of blocks with inline runs.
//!
//! One model for every long-form text on Tilo: a book's chapter (lowered
//! from XHTML), a feed's article and a web page's article (lowered from
//! HTML by [`super::extract`]), a mail message. Styling (sizes, faces,
//! spacing) is derived from the block kind when the text is laid out
//! ([`super::flow`]), never stored here.

use std::sync::Arc;

/// Where a run sits against the line: footnote references and exponents
/// are raised, chemical formulae lowered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Baseline {
    #[default]
    Normal,
    Super,
    Sub,
}

impl Baseline {
    /// The run's size as a fraction of its block's.
    #[must_use]
    pub fn size_factor(self) -> f32 {
        match self {
            Self::Normal => 1.0,
            Self::Super | Self::Sub => 0.72,
        }
    }

    /// How far the run's top moves down, in ems of its block's size: up for
    /// a superscript, down for a subscript.
    #[must_use]
    pub fn shift(self) -> f32 {
        match self {
            Self::Normal => 0.0,
            Self::Super => -0.05,
            Self::Sub => 0.38,
        }
    }
}

/// An inline run with uniform styling. The smallest unit the renderer paints.
///
/// Span text is *trimmed* at style boundaries: the whitespace that separated
/// this run from the previous one is not stored in `text` but recorded in
/// `space_before`, so `"burning <em>for</em>."` yields a `for` run with a
/// leading space and a `.` run with none. The layout and `plain_text` both
/// consult `space_before` to decide whether to insert a gap.
///
/// A hard line break (`<br/>`, a verse line) is laid out from `line_break`
/// but kept in the text as the single word space it always was, so a
/// block's plain text and every character offset into it are the same
/// whether or not its lines break.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "bold/italic/code/small caps/space_before/line_break are independent inline flags a \
              span can carry in any combination, not a state machine"
)]
pub struct Span {
    pub text: String,
    /// Structured inline `MathML`. `text` remains its accessible linear form.
    pub math: Option<crate::math::MathExpr>,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    /// Set in the face's small capitals (an abbreviation a publisher marked).
    pub small_caps: bool,
    pub baseline: Baseline,
    /// Whether a word break separated this run from the previous one. Ignored
    /// for the first run of a block (it starts the line).
    pub space_before: bool,
    /// This run starts a new line. Always carries `space_before`.
    pub line_break: bool,
    /// Indentation in ems of the line this run starts (a verse line's `i1`,
    /// `i2`, … class); read on a block's first run and on `line_break` runs.
    pub line_indent: u8,
    /// Where the run links: an absolute address, or `#id` inside the
    /// document.
    pub link: Option<Arc<str>>,
    /// The footnote this run refers to, an index into
    /// [`Document::footnotes`]: it opens in place instead of jumping.
    pub note: Option<u16>,
}

impl Span {
    /// Plain text with no styling.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            math: None,
            bold: false,
            italic: false,
            code: false,
            small_caps: false,
            baseline: Baseline::Normal,
            space_before: false,
            line_break: false,
            line_indent: 0,
            link: None,
            note: None,
        }
    }

    /// The same run, separated from the previous one by a word space.
    #[must_use]
    pub fn spaced(mut self) -> Self {
        self.space_before = true;
        self
    }
}

/// A picture in the text and what it shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Image {
    /// Its absolute address (or a `data:` URL).
    pub src: String,
    /// What it shows, for a screen reader and while it loads.
    pub alt: String,
    /// Its size in CSS pixels, when the source says: the reader reserves
    /// exactly that box, so the text never moves when the picture arrives.
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl Image {
    /// Height over width, when both are known and sensible.
    #[must_use]
    pub fn aspect(&self) -> Option<f32> {
        match (self.width, self.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => {
                Some(crate::num::f32_of_u32(h) / crate::num::f32_of_u32(w))
            }
            _ => None,
        }
    }
}

/// One cell of a table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableCell {
    pub spans: Vec<Span>,
    /// A heading cell (`<th>`): set in bold.
    pub header: bool,
}

/// One row of a table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableRow {
    pub cells: Vec<TableCell>,
}

/// A flow-level block. Kinds map to the structural HTML elements a book's
/// chapter or an article uses; styling (size, weight, indent) is derived from
/// the kind at layout time, never stored here.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// `<h1>`..`<h6>`: `level` is 1-based and clamped to 1..=3 for layout.
    /// Level 0 is the document's own title, set above everything else.
    Heading { level: u8, spans: Vec<Span> },
    /// `<p>`
    Paragraph { spans: Vec<Span> },
    /// `<blockquote>`: rendered indented with a rule.
    Quote { spans: Vec<Span> },
    /// A stanza or verse paragraph: a `<p>` (or other line container) inside
    /// z3998 `poem`/`verse`/`song`/`lyrics` semantics whose lines are marked
    /// up. Each run with [`Span::line_break`] starts a new line; lines are set
    /// ragged with a hanging indent. `quoted` verse sits in a `<blockquote>`.
    Verse { spans: Vec<Span>, quoted: bool },
    /// One `<li>` inside `<ul>`/`<ol>`. `ordered` picks bullet vs number;
    /// `index` is the 1-based ordinal for ordered lists; `depth` is how many
    /// lists it is nested in beyond the first.
    ListItem {
        spans: Vec<Span>,
        ordered: bool,
        index: usize,
        depth: u8,
    },
    /// Presentation `MathML` lowered into the toolkit's shared expression tree.
    Math {
        expr: crate::math::MathExpr,
        /// Linear fallback used for search, accessibility, and annotations.
        source: String,
        display: bool,
    },
    /// `<hr/>`: a section break.
    Separator,
    /// A quiet line about the document (its site, its author and date), in
    /// the interface face.
    Meta { spans: Vec<Span> },
    /// `<pre>`: preformatted text, kept as written. Long lines scroll
    /// sideways instead of wrapping, so the measure never breaks.
    Code {
        text: String,
        language: Option<String>,
    },
    /// A picture with its caption.
    Figure { image: Image, caption: Vec<Span> },
    /// A table; a wide one scrolls sideways inside the measure.
    Table { rows: Vec<TableRow> },
}

impl Block {
    /// The block's inline runs; empty for blocks without running text.
    pub fn spans(&self) -> &[Span] {
        match self {
            Block::Heading { spans, .. }
            | Block::Paragraph { spans }
            | Block::Quote { spans }
            | Block::Verse { spans, .. }
            | Block::ListItem { spans, .. }
            | Block::Meta { spans }
            | Block::Figure { caption: spans, .. } => spans,
            Block::Math { .. } | Block::Separator | Block::Code { .. } | Block::Table { .. } => &[],
        }
    }

    /// The text a person reads in this block, words joined by single spaces
    /// (code keeps its own spacing, its lines joined by a space).
    pub fn plain_text(&self) -> String {
        match self {
            Block::Math { source, .. } => source.clone(),
            Block::Code { text, .. } => text.replace('\n', " "),
            Block::Table { rows } => rows
                .iter()
                .flat_map(|row| row.cells.iter())
                .map(|cell| spans_text(&cell.spans))
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
            _ => spans_text(self.spans()),
        }
    }

    pub fn is_separator(&self) -> bool {
        matches!(self, Block::Separator)
    }
}

/// The plain text of `spans`: each run, with the word spaces between them.
pub fn spans_text(spans: &[Span]) -> String {
    // Spans are trimmed; a word break at a style boundary is carried by the
    // next span's `space_before` flag, so re-insert those spaces here.
    let mut out = String::new();
    for (i, s) in spans.iter().enumerate() {
        if i > 0 && s.space_before {
            out.push(' ');
        }
        out.push_str(&s.text);
    }
    out
}

/// A note the text refers to, shown in place when its reference is chosen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Footnote {
    /// What the reference says ("1", "*").
    pub label: String,
    pub spans: Vec<Span>,
}

/// A whole document as a reader shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    pub title: String,
    /// Who wrote it, when known.
    pub byline: String,
    /// Where it was published ("ploum.net"), when known.
    pub site: String,
    /// Unix seconds; `None` when unknown.
    pub published: Option<i64>,
    /// BCP-47 language, used for hyphenation; empty when unknown.
    pub language: String,
    /// Where it lives, against which its relative links were resolved.
    pub url: String,
    /// More lines about the document under its byline (the tags a reader
    /// gave it), each a [`Block::Meta`]: shown with the title, not part of
    /// the body.
    pub header: Vec<Block>,
    pub blocks: Vec<Block>,
    pub footnotes: Vec<Footnote>,
}

/// Words a person reads per minute before the reader has measured them.
pub const DEFAULT_WORDS_PER_MINUTE: f32 = 230.0;

impl Document {
    /// How many words the document's blocks hold.
    #[must_use]
    pub fn word_count(&self) -> usize {
        self.blocks.iter().map(block_words).sum()
    }

    /// The blocks a reader lays out: the title, the line about the document
    /// (its site, author and date), then the body. A body that begins with
    /// its own title does not repeat it.
    #[must_use]
    pub fn presented_blocks(&self, date: Option<String>) -> Vec<Block> {
        let mut blocks = Vec::with_capacity(self.blocks.len().saturating_add(3));
        let title = self.title.trim();
        if !self.site.trim().is_empty() {
            blocks.push(Block::Meta {
                spans: vec![Span::plain(self.site.trim())],
            });
        }
        if !title.is_empty() {
            blocks.push(Block::Heading {
                level: 0,
                spans: vec![Span::plain(title)],
            });
        }
        let byline: Vec<String> = [Some(self.byline.trim().to_owned()), date]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect();
        if !byline.is_empty() {
            blocks.push(Block::Meta {
                spans: vec![Span::plain(byline.join(", "))],
            });
        }
        blocks.extend(self.header.iter().cloned());
        let mut body = self.blocks.iter().peekable();
        // A body that opens with the title (as a heading or a paragraph)
        // would say it twice.
        if let Some(first) = body.peek()
            && !title.is_empty()
            && matches!(first, Block::Heading { .. } | Block::Paragraph { .. })
            && same_words(&first.plain_text(), title)
        {
            body.next();
        }
        blocks.extend(body.cloned());
        blocks
    }
}

fn block_words(block: &Block) -> usize {
    block.plain_text().split_whitespace().count()
}

/// Whether `a` and `b` say the same words, whatever their case and
/// punctuation.
#[must_use]
pub fn same_words(a: &str, b: &str) -> bool {
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let (a, b) = (words(a), words(b));
    !a.is_empty() && a == b
}

/// `text` without a leading `title` (an excerpt that opens with the title of
/// what it excerpts), compared by words, so case and punctuation differences
/// still match. Returns `text` itself when it does not start with it.
#[must_use]
pub fn strip_leading_title<'a>(text: &'a str, title: &str) -> &'a str {
    let wanted: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    if wanted.is_empty() {
        return text;
    }
    let mut matched = 0;
    let mut end = 0;
    let mut in_word = false;
    let mut word_start = 0;
    for (at, c) in text.char_indices() {
        let alnum = c.is_alphanumeric();
        if alnum && !in_word {
            in_word = true;
            word_start = at;
        } else if !alnum && in_word {
            in_word = false;
            let word = text.get(word_start..at).unwrap_or_default().to_lowercase();
            if wanted.get(matched) != Some(&word) {
                return text;
            }
            matched += 1;
            end = at;
            if matched == wanted.len() {
                break;
            }
        }
    }
    if in_word && matched < wanted.len() {
        let word = text.get(word_start..).unwrap_or_default().to_lowercase();
        if wanted.get(matched) == Some(&word) {
            matched += 1;
            end = text.len();
        }
    }
    if matched < wanted.len() {
        return text;
    }
    // Past the punctuation and spaces that ended the title (": ", " – ").
    text.get(end..)
        .unwrap_or_default()
        .trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, ':' | '-' | '–' | '—' | '|' | '.')
        })
}
