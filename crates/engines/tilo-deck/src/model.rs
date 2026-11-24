//! The deck as a presentation stores it: its slides in order, each slide's elements back to
//! front, and the words of its text boxes as the paragraphs they are.
//!
//! This is the shape that crosses between an app and this engine as JSON. A text box's words are
//! paragraphs of the workspace's own kind (`paragraph`, `list_item`, `heading`) with inline marks
//! (`strong`, `em`), the way a document stores them, so a deck written here reads as text
//! anywhere.


#![allow(
    clippy::arithmetic_side_effects,
    clippy::assigning_clones,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::should_implement_trait,
    reason = "a deck's numbers are slide coordinates and mark positions inside the words they cover, bounded by the limits checked on import"
)]
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The slide's own coordinate space: every element's `x/y/w/h` and `font_size` is in these units,
/// scaled to whatever rect paints it. 16:9.
pub const SLIDE_W: f64 = 1280.0;
pub const SLIDE_H: f64 = 720.0;

/// The most slides one deck holds.
pub const MAX_SLIDES: usize = 1_000;
/// The most elements one slide holds.
pub const MAX_ELEMENTS_PER_SLIDE: usize = 10_000;

#[must_use]
pub fn new_id() -> String {
    uuid_like()
}

/// A fresh id without a random source: the engine reads no device effects. The caller that needs
/// randomness (a program inserting an element) brings its own; ids made here are for imported
/// slides that carry none.
fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("00000000-0000-4000-8000-{n:012}")
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum ElementKind {
    #[default]
    Text,
    Rect,
    Ellipse,
    Line,
    Image,
}

impl ElementKind {
    /// The kind a file names, words for an unknown one.
    #[must_use]
    pub fn from_str(s: &str) -> Self {
        match s {
            "rect" => Self::Rect,
            "ellipse" => Self::Ellipse,
            "line" => Self::Line,
            "image" => Self::Image,
            _ => Self::Text,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Rect => "rect",
            Self::Ellipse => "ellipse",
            Self::Line => "line",
            Self::Image => "image",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

impl TextAlign {
    #[must_use]
    pub fn from_str(s: &str) -> Self {
        match s {
            "center" => Self::Center,
            "right" => Self::Right,
            "justify" => Self::Justify,
            _ => Self::Left,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
            Self::Justify => "justify",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    Title,
    #[default]
    TitleContent,
    TwoColumn,
    Section,
    Blank,
}

impl Layout {
    /// The layout a file names (its `qslides:` spelling), title and content for an unknown one.
    #[must_use]
    pub fn from_str(s: &str) -> Self {
        match s {
            "title" => Self::Title,
            "two-column" | "two_column" => Self::TwoColumn,
            "section" => Self::Section,
            "blank" => Self::Blank,
            _ => Self::TitleContent,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::TitleContent => "title-content",
            Self::TwoColumn => "two-column",
            Self::Section => "section",
            Self::Blank => "blank",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Background {
    #[default]
    Surface,
    Accent,
    Deep,
}

impl Background {
    #[must_use]
    pub fn from_str(s: &str) -> Self {
        match s {
            "accent" => Self::Accent,
            "deep" => Self::Deep,
            _ => Self::Surface,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Surface => "surface",
            Self::Accent => "accent",
            Self::Deep => "deep",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum PaletteKind {
    #[default]
    Indigo,
    Emerald,
    Amber,
    Rose,
    Sky,
}

impl PaletteKind {
    #[must_use]
    pub fn from_str(s: &str) -> Self {
        match s {
            "emerald" => Self::Emerald,
            "amber" => Self::Amber,
            "rose" => Self::Rose,
            "sky" => Self::Sky,
            _ => Self::Indigo,
        }
    }

    pub const ALL: [Self; 5] = [
        Self::Indigo,
        Self::Emerald,
        Self::Amber,
        Self::Rose,
        Self::Sky,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Indigo => "indigo",
            Self::Emerald => "emerald",
            Self::Amber => "amber",
            Self::Rose => "rose",
            Self::Sky => "sky",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeckLook {
    #[default]
    Light,
    Dark,
}

impl DeckLook {
    #[must_use]
    pub fn from_str(s: &str) -> Self {
        if s == "dark" { Self::Dark } else { Self::Light }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// A mark's value over a stretch of a paragraph's words: a flag (`strong`), a number (`size`) or
/// text (`em`'s neighbours, a link).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub enum MarkValue {
    Bool(bool),
    Int(i64),
    Str(String),
}

/// An inline mark over a stretch of a paragraph, counted in characters.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct BlockMark {
    pub start: usize,
    pub end: usize,
    pub name: String,
    pub value: MarkValue,
}

/// A paragraph of a text box's words.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub struct Block {
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub marks: Vec<BlockMark>,
    #[serde(default)]
    pub level: Option<i64>,
    #[serde(default)]
    pub ordered: Option<bool>,
    #[serde(default)]
    pub align: Option<String>,
    #[serde(default)]
    pub attrs: BTreeMap<String, MarkValue>,
}

impl Block {
    #[must_use]
    pub fn paragraph(text: impl Into<String>) -> Self {
        Self {
            kind: "paragraph".to_owned(),
            text: text.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn is_list(&self) -> bool {
        self.kind == "list_item"
    }

    /// Whether mark `name` covers every character of this paragraph.
    #[must_use]
    pub fn covered_by(&self, name: &str) -> bool {
        let len = self.text.chars().count();
        if len == 0 {
            return false;
        }
        let mut spans: Vec<&BlockMark> = self.marks.iter().filter(|m| m.name == name).collect();
        spans.sort_by_key(|span| span.start);
        let mut covered = 0;
        for span in spans {
            if span.start > covered {
                return false;
            }
            covered = covered.max(span.end);
        }
        covered >= len
    }
}

/// A text box, shape, line or picture on a slide.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Element {
    #[serde(default)]
    pub id: String,
    pub kind: ElementKind,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// A text box's words as Markdown, a paragraph each; empty for every other element.
    /// Bold (`**`), italic (`*`), bulleted (`- `) and numbered (`1. `) paragraphs are the marks a
    /// deck's boxes carry.
    #[serde(default)]
    pub text: String,
    /// The content address of a picture (`tilo-blob://<reference>`).
    #[serde(default)]
    pub src: String,
    /// A colour role of the deck's palette, `none`, or `#RRGGBB`.
    #[serde(default)]
    pub fill: String,
    #[serde(default)]
    pub stroke: String,
    #[serde(default)]
    pub stroke_width: f64,
    #[serde(default)]
    pub font_size: f64,
    #[serde(default)]
    pub align: TextAlign,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub radius: f64,
}

impl Element {
    /// A text box with `text` as its words, one paragraph per line.
    #[must_use]
    pub fn text(text: &str, x: f64, y: f64, w: f64, h: f64, font_size: f64) -> Self {
        Self {
            id: new_id(),
            kind: ElementKind::Text,
            x,
            y,
            w,
            h,
            text: text.to_owned(),
            fill: "text".to_owned(),
            font_size,
            ..Self::default()
        }
    }

    /// A shape, filled with the accent unless it is a line.
    #[must_use]
    pub fn shape(kind: ElementKind, x: f64, y: f64, w: f64, h: f64) -> Self {
        let line = kind == ElementKind::Line;
        Self {
            id: new_id(),
            kind,
            x,
            y,
            w,
            h,
            fill: if line { "none".to_owned() } else { "accent".to_owned() },
            stroke: if line { "accent".to_owned() } else { "none".to_owned() },
            stroke_width: if line { 6.0 } else { 0.0 },
            font_size: 32.0,
            radius: if kind == ElementKind::Rect { 16.0 } else { 0.0 },
            ..Self::default()
        }
    }

    /// A picture the content store holds at `src`.
    #[must_use]
    pub fn image(src: &str, x: f64, y: f64, w: f64, h: f64) -> Self {
        Self {
            id: new_id(),
            kind: ElementKind::Image,
            x,
            y,
            w,
            h,
            src: src.to_owned(),
            fill: "surface".to_owned(),
            font_size: 32.0,
            radius: 12.0,
            ..Self::default()
        }
    }

    /// The paragraphs it draws: its words as Markdown, read.
    #[must_use]
    pub fn rich_blocks(&self) -> Vec<Block> {
        blocks_of_markdown(&self.text)
    }

    /// The element's words without their marks.
    #[must_use]
    pub fn plain_text(&self) -> String {
        plain_text_of(&self.rich_blocks())
    }

    /// The same element with `f` applied (a decoded style, a place).
    #[must_use]
    pub fn with(mut self, f: impl FnOnce(&mut Self)) -> Self {
        f(&mut self);
        self
    }

    /// A text box with no words yet.
    #[must_use]
    pub fn is_blank_text(&self) -> bool {
        self.kind == ElementKind::Text && self.text.trim().is_empty()
    }
}

/// A slide.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Slide {
    #[serde(default)]
    pub id: String,
    pub layout: Layout,
    #[serde(default)]
    pub background: Background,
    #[serde(default)]
    pub notes: String,
    /// Elements, back to front.
    #[serde(default)]
    pub elements: Vec<Element>,
}

/// A deck.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Deck {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub palette: PaletteKind,
    #[serde(default)]
    pub look: DeckLook,
    #[serde(default)]
    pub slides: Vec<Slide>,
}

/// A paragraph per line of `text`, carrying `bold` and `italic` over its words.
#[must_use]
pub fn paragraphs_of(text: &str, bold: bool, italic: bool) -> Vec<Block> {
    text.split('\n')
        .map(|line| {
            let mut block = Block::paragraph(line);
            let len = line.chars().count();
            if len > 0 {
                if bold {
                    block.marks.push(BlockMark {
                        start: 0,
                        end: len,
                        name: "strong".to_owned(),
                        value: MarkValue::Bool(true),
                    });
                }
                if italic {
                    block.marks.push(BlockMark {
                        start: 0,
                        end: len,
                        name: "em".to_owned(),
                        value: MarkValue::Bool(true),
                    });
                }
            }
            block
        })
        .collect()
}

/// The plain text of `blocks`, a line each.
#[must_use]
pub fn plain_text_of(blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `(text, bold, italic)` runs of a paragraph, in order: how a presentation format stores words.
#[must_use]
pub fn runs_of(block: &Block) -> Vec<(&str, bool, bool)> {
    let len = block.text.len();
    let styled = |name: &str, at: usize| {
        let chars_before = block.text.get(..at).map_or(0, |head| head.chars().count());
        block
            .marks
            .iter()
            .filter(|span| span.name == name)
            .any(|span| span.start <= chars_before && chars_before < span.end)
    };
    let mut cuts: Vec<usize> = vec![0, len];
    for span in &block.marks {
        if span.name == "strong" || span.name == "em" {
            for character in [span.start, span.end] {
                if let Some((byte, _)) = block.text.char_indices().nth(character) {
                    cuts.push(byte);
                } else {
                    cuts.push(len);
                }
            }
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2)
        .filter_map(|pair| {
            let [start, end] = pair else { return None };
            let text = block.text.get(*start..*end)?;
            (!text.is_empty()).then(|| {
                (
                    text,
                    styled("strong", *start),
                    styled("em", *start),
                )
            })
        })
        .collect()
}

/// Appends a run to `block`, extending a mark that reached its end.
pub fn push_run(block: &mut Block, text: &str, bold: bool, italic: bool) {
    let start = block.text.chars().count();
    block.text.push_str(text);
    let end = block.text.chars().count();
    if start == end {
        return;
    }
    for (on, name) in [(bold, "strong"), (italic, "em")] {
        if !on {
            continue;
        }
        match block
            .marks
            .iter_mut()
            .find(|span| span.name == name && span.end == start)
        {
            Some(span) => span.end = end,
            None => block.marks.push(BlockMark {
                start,
                end,
                name: name.to_owned(),
                value: MarkValue::Bool(true),
            }),
        }
    }
}

/// The paragraphs a Markdown text says: a line each, `- ` and `1. ` lists, and `**bold**` and
/// `*italic*` runs.
///
/// This is the text a slide's box holds, not a document: headings, quotes, tables and links are
/// not slide text, so their markers stay as words.
#[must_use]
pub fn blocks_of_markdown(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut ordered_list = false;
    for line in text.split('\n') {
        let mut block = Block::paragraph("");
        let mut rest = line;
        if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            block.kind = "list_item".to_owned();
            block.level = Some(0);
            block.ordered = Some(false);
            ordered_list = false;
            rest = item;
        } else if let Some(item) = numbered_item(line) {
            block.kind = "list_item".to_owned();
            block.level = Some(0);
            block.ordered = Some(true);
            ordered_list = true;
            rest = item;
        } else if ordered_list && line.trim().is_empty() {
            ordered_list = false;
        }
        let (plain, marks) = inline_marks(rest);
        block.text = plain;
        block.marks = marks;
        blocks.push(block);
    }
    if blocks.is_empty() {
        blocks.push(Block::paragraph(""));
    }
    blocks
}

/// A numbered list item's words after its marker (`1. `, `12. `).
fn numbered_item(line: &str) -> Option<&str> {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    line.get(digits..)?.strip_prefix(". ")
}

/// `text` without its `**` and `*` markers, and the marks they stood for.
fn inline_marks(text: &str) -> (String, Vec<BlockMark>) {
    let mut plain = String::new();
    let mut marks = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut at = 0;
    let mut bold_from: Option<usize> = None;
    let mut italic_from: Option<usize> = None;
    while at < chars.len() {
        let two = chars.get(at) == Some(&'*') && chars.get(at + 1) == Some(&'*');
        if two {
            let len = plain.chars().count();
            match bold_from.take() {
                Some(start) => marks.push(BlockMark {
                    start,
                    end: len,
                    name: "strong".to_owned(),
                    value: MarkValue::Bool(true),
                }),
                None => bold_from = Some(len),
            }
            at += 2;
        } else if chars.get(at) == Some(&'*') {
            let len = plain.chars().count();
            match italic_from.take() {
                Some(start) => marks.push(BlockMark {
                    start,
                    end: len,
                    name: "em".to_owned(),
                    value: MarkValue::Bool(true),
                }),
                None => italic_from = Some(len),
            }
            at += 1;
        } else if let Some(&ch) = chars.get(at) {
            plain.push(ch);
            at += 1;
        }
    }
    (plain, marks)
}

/// `blocks` as Markdown: the text a slide's box holds.
#[must_use]
pub fn markdown_of_blocks(blocks: &[Block]) -> String {
    let mut out = String::new();
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        if block.kind == "list_item" {
            if block.ordered == Some(true) {
                out.push_str("1. ");
            } else {
                out.push_str("- ");
            }
        }
        let bold = block
            .marks
            .iter()
            .filter(|mark| mark.name == "strong")
            .collect::<Vec<_>>();
        let italic = block
            .marks
            .iter()
            .filter(|mark| mark.name == "em")
            .collect::<Vec<_>>();
        let chars: Vec<char> = block.text.chars().collect();
        let mut at = 0;
        while at < chars.len() {
            let on_bold = bold.iter().any(|mark| mark.start <= at && at < mark.end);
            let on_italic = italic.iter().any(|mark| mark.start <= at && at < mark.end);
            let mut run = String::new();
            while at < chars.len()
                && bold.iter().any(|mark| mark.start <= at && at < mark.end) == on_bold
                && italic.iter().any(|mark| mark.start <= at && at < mark.end) == on_italic
            {
                if let Some(&ch) = chars.get(at) {
                    run.push(ch);
                }
                at += 1;
            }
            if on_bold && on_italic {
                out.push_str("***");
                out.push_str(&run);
                out.push_str("***");
            } else if on_bold {
                out.push_str("**");
                out.push_str(&run);
                out.push_str("**");
            } else if on_italic {
                out.push('*');
                out.push_str(&run);
                out.push('*');
            } else {
                out.push_str(&run);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paragraphs_marks_read_as_runs_and_write_back() {
        let mut block = Block::paragraph("plain bold");
        block.marks.push(BlockMark {
            start: 6,
            end: 10,
            name: "strong".to_owned(),
            value: MarkValue::Bool(true),
        });
        assert_eq!(
            runs_of(&block),
            vec![("plain ", false, false), ("bold", true, false)]
        );
        let mut built = Block::paragraph("plain ");
        push_run(&mut built, "bo", true, false);
        push_run(&mut built, "ld", true, true);
        assert_eq!(runs_of(&built), vec![("plain ", false, false), ("bo", true, false), ("ld", true, true)]);
        assert_eq!(plain_text_of(&[built.clone()]), "plain bold");
        assert_eq!(paragraphs_of("a\nb", true, false)[1].text, "b");
    }

    #[test]
    fn markdown_round_trips_marks_and_lists() {
        let text = "plain **bold** and *italic*\n- one\n1. two";
        let blocks = blocks_of_markdown(text);
        assert_eq!(blocks.len(), 3);
        assert!(blocks[1].is_list() && blocks[1].ordered == Some(false));
        assert!(blocks[2].is_list() && blocks[2].ordered == Some(true));
        assert_eq!(blocks[0].text, "plain bold and italic");
        assert!(
            blocks[0]
                .marks
                .iter()
                .any(|mark| mark.name == "strong" && mark.start == 6 && mark.end == 10)
        );
        assert_eq!(markdown_of_blocks(&blocks), text);
    }
}
