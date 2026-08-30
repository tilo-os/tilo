//! Reflowable text layout + pagination.
//!
//! Given a chapter's [`Block`]s, a content box, and the live [`Settings`], this
//! lays the chapter out as one continuous column with real inline styling
//! (bold / italic / mono) and word-level wrapping, then slices that column into
//! fixed-height pages at line boundaries (so a line is never cut in half).
//!
//! Layout is measurement-bound, so it runs inside `draw` (where a
//! [`TextRenderer`] is available) and is cached per [`LayoutKey`]; it only
//! recomputes when the chapter, content width, or a typography setting changes.

use ui::text_render::{FontChoice, TextRenderer, TextWeight};
use ui::{ParagraphBox, ParagraphOptions, compose_paragraph, discretionary_hyphen_breaks};

use crate::model::{Block, FontKind, Settings};

/// Map a reader [`FontKind`] to the toolkit [`FontChoice`] used for shaping.
pub fn font_choice(kind: &FontKind) -> FontChoice {
    match kind {
        FontKind::Default => FontChoice::SansSerif,
        FontKind::Serif => FontChoice::Serif,
        FontKind::Mono => FontChoice::Monospace,
        FontKind::Named(n) => FontChoice::Named(n.clone()),
    }
}

/// A positioned, single-style run within a line. `x` is relative to the line's
/// left edge (which is `content_x + indent`). A run here is one whitespace-
/// separated word; `char_start`/`char_len` locate it within the block's
/// canonical text so text selection can map a point to a character offset.
#[derive(Clone, Debug)]
pub struct LineRun {
    pub x: f32,
    /// Canonical source text covered by this run.
    pub text: String,
    /// Painted text; differs only when an exercised discretionary break adds
    /// a terminal hyphen that is absent from the source document.
    pub display_text: String,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    /// Char offset of this run's first character within the block's canonical
    /// text (see [`LaidBlock::text`]).
    pub char_start: usize,
    /// Number of chars in this run.
    pub char_len: usize,
}

/// One visual line of a block. `y` is absolute within the chapter's flow.
#[derive(Clone, Debug)]
pub struct LaidLine {
    pub y: f32,
    pub runs: Vec<LineRun>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutKind {
    Heading,
    Paragraph,
    Quote,
    List,
    Separator,
}

/// A laid-out block, ready to paint.
#[derive(Clone, Debug)]
pub struct LaidBlock {
    pub index: usize,
    pub kind: LayoutKind,
    /// Absolute top of the block's first line.
    pub top: f32,
    /// Absolute bottom of the block's last line.
    pub bottom: f32,
    pub size: f32,
    pub line_height: f32,
    /// Left inset for text + decorations, relative to `content_x`.
    pub indent: f32,
    pub lines: Vec<LaidLine>,
    /// Bullet/number glyph for list items.
    pub bullet: Option<String>,
    /// The block's canonical selectable text: its words joined by single
    /// spaces. Char offsets in [`LineRun::char_start`] index into this.
    pub text: String,
    /// Total chars in [`Self::text`].
    pub char_len: usize,
}

/// A fully laid-out chapter plus its page breaks.
#[derive(Clone, Debug)]
pub struct ChapterLayout {
    pub blocks: Vec<LaidBlock>,
    pub total_height: f32,
    /// Absolute y at which each page begins. `pages.len()` == page count.
    pub pages: Vec<f32>,
    pub key: LayoutKey,
}

impl ChapterLayout {
    pub fn page_count(&self) -> usize {
        self.pages.len().max(1)
    }
}

/// Cache key: layout is invalid if any of these change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutKey {
    pub chapter: usize,
    pub width_q: i32,
    pub height_q: i32,
    pub scale_q: i32,
    pub spacing_q: i32,
    /// Signature of the selected font (family + per-face size multiplier), so a
    /// font change relays out.
    pub font_sig: u64,
}

impl LayoutKey {
    pub fn new(
        chapter: usize,
        content_w: f32,
        content_h: f32,
        s: &Settings,
        font_sig: u64,
    ) -> Self {
        Self {
            chapter,
            width_q: content_w.round() as i32,
            height_q: content_h.round() as i32,
            scale_q: (s.font_scale * 100.0).round() as i32,
            spacing_q: (s.line_spacing * 100.0).round() as i32,
            font_sig,
        }
    }
}

/// Per-block style derived from kind + settings.
struct BlockStyle {
    kind: LayoutKind,
    size: f32,
    /// Base inline style applied to every run (quotes are italic; mono forces).
    force_italic: bool,
    force_bold: bool,
    indent: f32,
    space_before: f32,
    /// Fixed total height for blocks with no flowed text (separators).
    fixed_height: Option<f32>,
}

fn style_for(block: &Block, body: f32) -> BlockStyle {
    match block {
        Block::Heading { level, .. } => {
            let (mult, sb) = match level {
                1 => (1.62, 1.6),
                2 => (1.32, 1.3),
                _ => (1.14, 1.0),
            };
            BlockStyle {
                kind: LayoutKind::Heading,
                size: body * mult,
                force_italic: false,
                force_bold: true,
                indent: 0.0,
                space_before: body * sb,
                fixed_height: None,
            }
        }
        Block::Paragraph { .. } => BlockStyle {
            kind: LayoutKind::Paragraph,
            size: body,
            force_italic: false,
            force_bold: false,
            indent: 0.0,
            space_before: body * 0.75,
            fixed_height: None,
        },
        Block::Quote { .. } => BlockStyle {
            kind: LayoutKind::Quote,
            size: body * 1.02,
            force_italic: true,
            force_bold: false,
            indent: 30.0,
            space_before: body * 0.9,
            fixed_height: None,
        },
        Block::ListItem { .. } => BlockStyle {
            kind: LayoutKind::List,
            size: body,
            force_italic: false,
            force_bold: false,
            indent: 28.0,
            space_before: body * 0.4,
            fixed_height: None,
        },
        Block::Separator => BlockStyle {
            kind: LayoutKind::Separator,
            size: body,
            force_italic: false,
            force_bold: false,
            indent: 0.0,
            space_before: body * 1.2,
            fixed_height: Some(body * 1.6),
        },
    }
}

/// A word carrying its inline style.
struct Word {
    text: String,
    bold: bool,
    italic: bool,
    mono: bool,
    w: f32,
    /// Whether a space separates this word from the previous one. `false` lets
    /// punctuation and clitics hug the preceding word across a style boundary.
    space_before: bool,
    /// Char offset of this word's first char within the block's canonical text.
    char_start: usize,
    char_len: usize,
    /// This fragment continues the preceding source word and may begin a line
    /// only by exercising a discretionary hyphen.
    hyphen_break_before: bool,
    hyphen_width_before: f32,
}

fn weight(bold: bool) -> TextWeight {
    if bold {
        TextWeight::Bold
    } else {
        TextWeight::Regular
    }
}

/// Lay out one chapter into positioned lines + page breaks. `base_family` is
/// the reader's chosen body face (serif / a named installed family / sans);
/// `font_mult` scales the nominal body size for that face; `font_sig` feeds the
/// layout cache key so a font change relays out.
// Layout entry point: chapter/text-renderer/box/typography params are the
// natural, minimal parameter set for the pagination engine.
#[allow(clippy::too_many_arguments)]
pub fn layout_chapter(
    tr: &mut TextRenderer,
    blocks: &[Block],
    content_w: f32,
    content_h: f32,
    settings: &Settings,
    chapter: usize,
    language: &str,
    base_family: &FontChoice,
    font_mult: f32,
    font_sig: u64,
) -> ChapterLayout {
    let key = LayoutKey::new(chapter, content_w, content_h, settings, font_sig);
    let body = settings.body_size_with(font_mult);
    let base_mono = matches!(base_family, FontChoice::Monospace);
    let mut laid: Vec<LaidBlock> = Vec::with_capacity(blocks.len());
    let mut y = 0.0f32;

    for (index, block) in blocks.iter().enumerate() {
        let st = style_for(block, body);
        y += st.space_before;

        if st.kind == LayoutKind::Separator {
            let h = st.fixed_height.unwrap_or(st.size * 1.6);
            laid.push(LaidBlock {
                index,
                kind: LayoutKind::Separator,
                top: y,
                bottom: y + h,
                size: st.size,
                line_height: h,
                indent: 0.0,
                lines: vec![LaidLine { y, runs: vec![] }],
                bullet: None,
                text: String::new(),
                char_len: 0,
            });
            y += h;
            continue;
        }

        let line_height = st.size * 1.25 * settings.line_spacing;
        let text_width = (content_w - st.indent).max(40.0);
        let space_w = measure_space(tr, st.size, base_family);

        // Build the word list with measured widths, and the block's canonical
        // selectable text (words joined by single spaces). The first word of
        // each span inherits the span's `space_before`; later words are
        // whitespace-separated by construction.
        let mut words: Vec<Word> = Vec::new();
        let mut canon = String::new();
        let mut char_pos = 0usize;
        for span in block.spans() {
            let bold = span.bold || st.force_bold;
            let italic = span.italic || st.force_italic;
            let mono = span.code || base_mono;
            let word_family = if mono {
                &FontChoice::Monospace
            } else {
                base_family
            };
            for (wi, w) in span.text.split_whitespace().enumerate() {
                let space_before = if wi == 0 { span.space_before } else { true };
                // A separating space is one canonical char between words.
                if !canon.is_empty() && space_before {
                    canon.push(' ');
                    char_pos += 1;
                }
                let char_start = char_pos;
                let char_len = w.chars().count();
                canon.push_str(w);
                char_pos += char_len;
                let mut boundaries = discretionary_hyphen_breaks(w, language);
                boundaries.push(w.len());
                let hyphen_width = tr
                    .measure_font("-", st.size, None, weight(bold), italic, word_family)
                    .0;
                let mut byte_start = 0;
                let mut chars_before = 0;
                for (fragment_index, byte_end) in boundaries.into_iter().enumerate() {
                    let fragment = &w[byte_start..byte_end];
                    let fragment_chars = fragment.chars().count();
                    let fragment_width = tr
                        .measure_font(fragment, st.size, None, weight(bold), italic, word_family)
                        .0;
                    words.push(Word {
                        text: fragment.to_string(),
                        bold,
                        italic,
                        mono,
                        w: fragment_width,
                        space_before: fragment_index == 0 && space_before,
                        char_start: char_start + chars_before,
                        char_len: fragment_chars,
                        hyphen_break_before: fragment_index != 0,
                        hyphen_width_before: if fragment_index == 0 {
                            0.0
                        } else {
                            hyphen_width
                        },
                    });
                    byte_start = byte_end;
                    chars_before += fragment_chars;
                }
            }
        }

        // Compose the complete paragraph rather than accepting the first local
        // break that fits. Body prose and quotations use adjustable inter-word
        // glue; headings and lists retain natural ragged spacing while still
        // benefiting from paragraph-wide break selection.
        let mut lines: Vec<LaidLine> = Vec::new();
        let block_top = y;
        let boxes: Vec<_> = words
            .iter()
            .map(|word| ParagraphBox {
                width: word.w,
                glue_before: if word.space_before { space_w } else { 0.0 },
                break_before: word.space_before || word.hyphen_break_before,
                break_penalty: if word.hyphen_break_before { 50 } else { 0 },
                flagged_break: word.hyphen_break_before,
                break_width: word.hyphen_width_before,
            })
            .collect();
        let mut options = ParagraphOptions::new(text_width);
        options.justify = matches!(st.kind, LayoutKind::Paragraph | LayoutKind::Quote);
        let composed = compose_paragraph(&boxes, options);
        for (line_idx, line) in composed.iter().enumerate() {
            let mut runs: Vec<LineRun> = Vec::with_capacity(line.end - line.start);
            let mut x = 0.0;
            for (offset, word) in words[line.start..line.end].iter().enumerate() {
                if offset != 0 && word.space_before {
                    x += space_w * line.glue_scale;
                }
                if word.hyphen_break_before
                    && let Some(previous) = runs.last_mut()
                    && previous.bold == word.bold
                    && previous.italic == word.italic
                    && previous.mono == word.mono
                    && previous.char_start + previous.char_len == word.char_start
                {
                    previous.text.push_str(&word.text);
                    previous.display_text.push_str(&word.text);
                    previous.char_len += word.char_len;
                    let family = if word.mono {
                        &FontChoice::Monospace
                    } else {
                        base_family
                    };
                    x = previous.x
                        + tr.measure_font(
                            &previous.text,
                            st.size,
                            None,
                            weight(word.bold),
                            word.italic,
                            family,
                        )
                        .0;
                    continue;
                }
                runs.push(LineRun {
                    x,
                    text: word.text.clone(),
                    display_text: word.text.clone(),
                    bold: word.bold,
                    italic: word.italic,
                    mono: word.mono,
                    char_start: word.char_start,
                    char_len: word.char_len,
                });
                x += word.w;
            }
            if line.end < words.len()
                && words[line.end].hyphen_break_before
                && let Some(last) = runs.last_mut()
            {
                last.display_text.push('-');
            }
            lines.push(LaidLine {
                y: block_top + line_idx as f32 * line_height,
                runs,
            });
        }
        if lines.is_empty() {
            lines.push(LaidLine {
                y: block_top,
                runs: Vec::new(),
            });
        }

        let bullet = match block {
            Block::ListItem {
                ordered: true,
                index: n,
                ..
            } => Some(format!("{n}.")),
            Block::ListItem { ordered: false, .. } => Some("•".to_string()),
            _ => None,
        };

        let height = lines.len() as f32 * line_height;
        laid.push(LaidBlock {
            index,
            kind: st.kind,
            top: block_top,
            bottom: block_top + height,
            size: st.size,
            line_height,
            indent: st.indent,
            lines,
            bullet,
            char_len: char_pos,
            text: canon,
        });
        y += height;
    }

    let total_height = y;
    let pages = paginate(&laid, content_h);

    ChapterLayout {
        blocks: laid,
        total_height,
        pages,
        key,
    }
}

/// Slice the flow into page-start offsets at line boundaries.
fn paginate(blocks: &[LaidBlock], page_h: f32) -> Vec<f32> {
    if blocks.is_empty() || page_h <= 1.0 {
        return vec![0.0];
    }
    #[derive(Clone, Copy)]
    struct Row {
        y: f32,
        height: f32,
        block: usize,
        line: usize,
        line_count: usize,
        kind: LayoutKind,
    }

    // Flatten to row boxes while retaining enough paragraph structure to
    // enforce TeX-like page-break constraints.
    let mut rows = Vec::new();
    for (block_index, b) in blocks.iter().enumerate() {
        if b.kind == LayoutKind::Separator {
            rows.push(Row {
                y: b.top,
                height: b.bottom - b.top,
                block: block_index,
                line: 0,
                line_count: 1,
                kind: b.kind,
            });
        } else {
            for (line, l) in b.lines.iter().enumerate() {
                rows.push(Row {
                    y: l.y,
                    height: b.line_height,
                    block: block_index,
                    line,
                    line_count: b.lines.len(),
                    kind: b.kind,
                });
            }
        }
    }
    rows.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));

    let mut pages = vec![rows[0].y];
    let mut page_row = 0;
    while page_row < rows.len() {
        let page_start = rows[page_row].y;
        let Some(overflow) = (page_row..rows.len())
            .find(|&index| rows[index].y + rows[index].height - page_start > page_h + 0.5)
        else {
            break;
        };
        let mut break_at = overflow;
        let row = rows[break_at];

        // Keep at least two lines of an ordinary text block on either side of
        // a page boundary whenever that can be achieved on this page.
        if matches!(
            row.kind,
            LayoutKind::Paragraph | LayoutKind::Quote | LayoutKind::List
        ) && row.line_count >= 4
        {
            if row.line == 1 {
                break_at -= 1;
            } else if row.line_count - row.line == 1 {
                break_at -= 1;
            }
        }

        // A heading is semantically attached to the text following it. If it
        // would become the final item on a page, carry the complete heading to
        // the next page as long as it did not itself begin this page.
        if break_at > page_row
            && rows[break_at - 1].kind == LayoutKind::Heading
            && rows[break_at - 1].block != rows[break_at].block
        {
            let heading_block = rows[break_at - 1].block;
            let heading_start = (page_row..break_at)
                .find(|&index| rows[index].block == heading_block)
                .unwrap_or(break_at - 1);
            if heading_start > page_row {
                break_at = heading_start;
            }
        }

        // An indivisible item taller than a page has no legal alternative.
        if break_at <= page_row {
            break_at = overflow.max(page_row + 1);
        }
        pages.push(rows[break_at].y);
        page_row = break_at;
    }
    pages
}

/// Width of a single space at `size` in `family`, derived by differencing.
fn measure_space(tr: &mut TextRenderer, size: f32, family: &FontChoice) -> f32 {
    let w2 = tr
        .measure_font("m m", size, None, TextWeight::Regular, false, family)
        .0;
    let w1 = tr
        .measure_font("mm", size, None, TextWeight::Regular, false, family)
        .0;
    (w2 - w1).max(size * 0.22)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Block, Span};

    fn para(text: &str) -> Block {
        Block::Paragraph {
            spans: vec![Span::plain(text)],
        }
    }

    #[test]
    fn layout_produces_pages_and_lines() {
        let mut tr = TextRenderer::new();
        let blocks = vec![
            Block::Heading {
                level: 1,
                spans: vec![Span::plain("A Chapter")],
            },
            para(
                "This is a reasonably long paragraph that should wrap across \
                  several lines when laid out into a narrow content column so \
                  that pagination has something real to slice into pages here.",
            ),
            Block::Separator,
            para(
                "Another paragraph after the rule, also long enough to wrap a \
                  couple of times within the narrow measure we hand the engine.",
            ),
        ];
        let layout = layout_chapter(
            &mut tr,
            &blocks,
            360.0,
            220.0,
            &Settings::default(),
            0,
            "en-US",
            &FontChoice::SansSerif,
            1.0,
            0,
        );
        assert!(layout.total_height > 0.0);
        assert!(layout.page_count() >= 1);
        // The long paragraph wrapped into multiple lines.
        let para_block = &layout.blocks[1];
        assert!(para_block.lines.len() >= 2, "paragraph should wrap");
        // Lines are monotonically increasing in y.
        let ys: Vec<f32> = para_block.lines.iter().map(|l| l.y).collect();
        assert!(ys.windows(2).all(|w| w[1] > w[0]));
    }

    #[test]
    fn narrow_tall_content_paginates_into_several_pages() {
        let mut tr = TextRenderer::new();
        let big = "word ".repeat(400);
        let blocks = vec![para(&big)];
        let layout = layout_chapter(
            &mut tr,
            &blocks,
            300.0,
            160.0,
            &Settings::default(),
            0,
            "en-US",
            &FontChoice::SansSerif,
            1.0,
            0,
        );
        assert!(layout.page_count() >= 3, "lots of text -> several pages");
        // Page starts strictly increase.
        assert!(layout.pages.windows(2).all(|w| w[1] > w[0]));
    }

    #[test]
    fn layout_key_changes_with_settings_and_font() {
        let a = LayoutKey::new(0, 300.0, 200.0, &Settings::default(), 0);
        let s = Settings {
            font_scale: 1.4,
            ..Default::default()
        };
        let b = LayoutKey::new(0, 300.0, 200.0, &s, 0);
        assert_ne!(a, b, "font scale change relays out");
        // A different font signature (family/size-mult) also relays out.
        let c = LayoutKey::new(0, 300.0, 200.0, &Settings::default(), 99);
        assert_ne!(a, c, "font change relays out");
    }

    #[test]
    fn selection_char_offsets_are_assigned_across_words() {
        let mut tr = TextRenderer::new();
        let blocks = vec![para("alpha beta gamma")];
        let layout = layout_chapter(
            &mut tr,
            &blocks,
            600.0,
            400.0,
            &Settings::default(),
            0,
            "en-US",
            &FontChoice::SansSerif,
            1.0,
            0,
        );
        let b = &layout.blocks[0];
        assert_eq!(
            b.text, "alpha beta gamma",
            "canonical text joins words with spaces"
        );
        // Each run carries a char range that indexes into the canonical text.
        let runs: Vec<&LineRun> = b.lines.iter().flat_map(|l| l.runs.iter()).collect();
        assert_eq!(runs.len(), 3);
        for r in &runs {
            let slice: String = b.text.chars().skip(r.char_start).take(r.char_len).collect();
            assert_eq!(slice, r.text, "run char range slices back to its text");
        }
    }

    #[test]
    fn language_aware_hyphenation_paints_only_the_exercised_break() {
        let mut tr = TextRenderer::new();
        let blocks = vec![para("characteristically")];
        let layout = layout_chapter(
            &mut tr,
            &blocks,
            105.0,
            400.0,
            &Settings::default(),
            0,
            "en-US",
            &FontChoice::Serif,
            1.0,
            0,
        );
        let block = &layout.blocks[0];
        assert!(block.lines.len() >= 2, "the long word should hyphenate");
        assert!(
            block.lines[..block.lines.len() - 1]
                .iter()
                .filter_map(|line| line.runs.last())
                .any(|run| run.display_text.ends_with('-')),
            "a selected discretionary break paints its hyphen"
        );
        let source: String = block
            .lines
            .iter()
            .flat_map(|line| line.runs.iter())
            .map(|run| run.text.as_str())
            .collect();
        assert_eq!(
            source, block.text,
            "virtual hyphens never enter source text"
        );
    }

    fn block_with_lines(kind: LayoutKind, top: f32, line_count: usize) -> LaidBlock {
        let line_height = 20.0;
        LaidBlock {
            index: 0,
            kind,
            top,
            bottom: top + line_count as f32 * line_height,
            size: 16.0,
            line_height,
            indent: 0.0,
            lines: (0..line_count)
                .map(|line| LaidLine {
                    y: top + line as f32 * line_height,
                    runs: Vec::new(),
                })
                .collect(),
            bullet: None,
            text: String::new(),
            char_len: 0,
        }
    }

    #[test]
    fn pagination_avoids_single_line_widows_and_orphans() {
        let widow = block_with_lines(LayoutKind::Paragraph, 0.0, 5);
        assert_eq!(paginate(&[widow], 85.0), vec![0.0, 60.0]);

        let before = block_with_lines(LayoutKind::Separator, 0.0, 1);
        let paragraph = block_with_lines(LayoutKind::Paragraph, 40.0, 5);
        assert_eq!(paginate(&[before, paragraph], 65.0), vec![0.0, 40.0, 100.0]);
    }

    #[test]
    fn pagination_keeps_a_heading_with_following_text() {
        let intro = block_with_lines(LayoutKind::Paragraph, 0.0, 1);
        let heading = block_with_lines(LayoutKind::Heading, 25.0, 1);
        let body = block_with_lines(LayoutKind::Paragraph, 45.0, 3);
        assert_eq!(
            paginate(&[intro, heading, body], 55.0),
            vec![0.0, 25.0, 65.0]
        );
    }
}
