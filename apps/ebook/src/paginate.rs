//! Books' text layout: the shared reading flow ([`ui::reading::flow`]),
//! set in a book's [`Settings`].
//!
//! Every reader on Tilo lays text out through the same engine, with real
//! inline styling, paragraph-wide composition and hyphenation by the book's
//! language; Books hands it a chapter's blocks and slices the result into
//! fixed-height pages at line boundaries (so a line is never cut in half).
//!
//! Layout is measurement-bound and cached per [`LayoutKey`]; it only
//! recomputes when the chapter, content width, or a typography setting
//! changes. The reader lays chapters out on a worker
//! ([`crate::layout_worker`]), block by block ([`ChapterLayouter`]).

use ui::reading::flow::{FlowRequest, Typesetting, layout_flow};
use ui::text_render::{FontChoice, TextRenderer};

pub use ui::reading::flow::{
    FlowKey as LayoutKey, FlowLayout as ChapterLayout, FlowLayouter as ChapterLayouter,
    LAY_BLOCK_PHASE, LaidBlock, LaidLine, LaidMath, LayoutKind, LineRun, run_style,
    selectable_text,
};

use crate::model::{Block, FontKind, Settings};

/// Map a reader [`FontKind`] to the toolkit [`FontChoice`] used for shaping.
pub fn font_choice(kind: &FontKind) -> FontChoice {
    match kind {
        FontKind::Default => FontChoice::SansSerif,
        FontKind::Serif => FontChoice::Serif,
        FontKind::Mono => FontChoice::Monospace,
        // A reading face by its name; the toolkit sets each run in the
        // face's optical cut for its size. A name this build does not ship
        // reads in the system's reading face.
        FontKind::Reading(name) => ui::ReadingFace::from_name(name)
            .map_or(FontChoice::Serif, |face| {
                FontChoice::Named(face.family().to_owned())
            }),
        FontKind::Named(n) => FontChoice::Named(n.clone()),
    }
}

/// How a book's settings set text: its size (with the face's own size
/// multiplier), spacing and composition.
pub fn typesetting(settings: &Settings, font_multiplier: f32) -> Typesetting {
    Typesetting {
        body_size: settings.body_size_with(font_multiplier),
        line_spacing: settings.line_spacing,
        justify: settings.composition.justified(),
        reading_scale: settings.font_scale,
    }
}

/// The cache key a chapter laid out with these settings has.
pub fn layout_key(
    chapter: usize,
    content_w: f32,
    content_h: f32,
    settings: &Settings,
    font_multiplier: f32,
    font_sig: u64,
    text_scale: f32,
) -> LayoutKey {
    LayoutKey::new(
        chapter,
        content_w,
        content_h,
        &typesetting(settings, font_multiplier),
        font_sig,
        text_scale,
    )
}

/// Lay out one chapter into positioned lines + page breaks. `base_family` is
/// the reader's chosen body face (serif / a named installed family / sans);
/// `font_multiplier` scales the nominal body size for that face; `font_signature`
/// feeds the layout cache key so a font change relays out.
pub struct ChapterLayoutRequest<'a> {
    pub blocks: &'a [Block],
    pub content_width: f32,
    pub content_height: f32,
    pub settings: &'a Settings,
    pub chapter: usize,
    pub language: &'a str,
    pub base_family: &'a FontChoice,
    pub font_multiplier: f32,
    pub font_signature: u64,
}

/// The shared flow's request for a chapter, set in `typesetting` (the
/// chapter's [`typesetting`]).
pub fn flow_request<'a>(
    request: &ChapterLayoutRequest<'a>,
    typesetting: &'a Typesetting,
) -> FlowRequest<'a> {
    FlowRequest {
        blocks: request.blocks,
        content_width: request.content_width,
        // A page has no room beyond its column: pictures keep to it.
        bleed_width: request.content_width,
        content_height: request.content_height,
        typesetting,
        section: request.chapter,
        language: request.language,
        base_family: request.base_family,
        font_signature: request.font_signature,
    }
}

pub fn layout_chapter(tr: &mut TextRenderer, request: &ChapterLayoutRequest<'_>) -> ChapterLayout {
    let typesetting = typesetting(request.settings, request.font_multiplier);
    layout_flow(tr, &flow_request(request, &typesetting))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Block, CompositionStyle, Span};
    use ui::text_render::TextWeight;

    /// Justified composition, which these layouts exercise; a reader's
    /// default is ragged.
    fn book_settings() -> Settings {
        Settings {
            composition: CompositionStyle::Book,
            ..Settings::default()
        }
    }

    fn para(text: &str) -> Block {
        Block::Paragraph {
            spans: vec![Span::plain(text)],
        }
    }

    fn test_layout(
        text: &mut TextRenderer,
        blocks: &[Block],
        content_width: f32,
        content_height: f32,
        settings: &Settings,
        language: &str,
        base_family: &FontChoice,
    ) -> ChapterLayout {
        layout_chapter(
            text,
            &ChapterLayoutRequest {
                blocks,
                content_width,
                content_height,
                settings,
                chapter: 0,
                language,
                base_family,
                font_multiplier: 1.0,
                font_signature: 0,
            },
        )
    }

    /// Layout produces pages and lines (including math and tall narrow content), keys on settings and font, and assigns selection offsets.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn layout_paginates_content_and_keys_on_settings() {
        // --- phase: layout_produces_pages_and_lines ---
        {
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
            let layout = test_layout(
                &mut tr,
                &blocks,
                360.0,
                220.0,
                &book_settings(),
                "en-US",
                &FontChoice::SansSerif,
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

        // --- phase: math_block_uses_shared_two_dimensional_layout ---
        {
            let mut tr = TextRenderer::new();
            let expr = ui::math::MathExpr {
                root: vec![ui::math::Node::Frac {
                    num: vec![
                        ui::math::Node::Char('x'),
                        ui::math::Node::Sup {
                            exp: vec![ui::math::Node::Char('2')],
                        },
                    ],
                    den: vec![ui::math::Node::Sqrt {
                        index: None,
                        radicand: vec![ui::math::Node::Char('y')],
                    }],
                }],
            };
            let layout = test_layout(
                &mut tr,
                &[Block::Math {
                    expr,
                    source: "(x^(2))/(sqrt(y))".into(),
                    display: true,
                }],
                420.0,
                500.0,
                &book_settings(),
                "en-US",
                &FontChoice::Serif,
            );
            let block = &layout.blocks[0];
            let math = block.math.as_ref().expect("shared math layout attached");
            assert_eq!(block.kind, LayoutKind::Math);
            assert!(math.layout.height > f64::from(block.size) * 2.0);
            assert!(math.x > 0.0, "display formula is centered in the measure");
            assert!(
                math.layout
                    .boxes
                    .iter()
                    .any(|math_box| matches!(math_box.kind, ui::math::BoxKind::FracBar))
            );
        }

        // --- phase: narrow_tall_content_paginates_into_several_pages ---
        {
            let mut tr = TextRenderer::new();
            let big = "word ".repeat(400);
            let blocks = vec![para(&big)];
            let layout = test_layout(
                &mut tr,
                &blocks,
                300.0,
                160.0,
                &book_settings(),
                "en-US",
                &FontChoice::SansSerif,
            );
            assert!(layout.page_count() >= 3, "lots of text -> several pages");
            // Page starts strictly increase.
            assert!(layout.pages.windows(2).all(|w| w[1] > w[0]));
        }

        // --- phase: layout_key_changes_with_settings_and_font ---
        {
            let base = layout_key(0, 300.0, 200.0, &book_settings(), 1.0, 0, 1.0);
            let bigger_font = Settings {
                font_scale: 1.4,
                ..Default::default()
            };
            let scaled = layout_key(0, 300.0, 200.0, &bigger_font, 1.0, 0, 1.0);
            assert_ne!(base, scaled, "font scale change relays out");
            // A different font signature (family/size-mult) also relays out.
            let font_changed = layout_key(0, 300.0, 200.0, &book_settings(), 1.0, 99, 1.0);
            assert_ne!(base, font_changed, "font change relays out");
            let ragged = Settings {
                composition: CompositionStyle::Ragged,
                ..Default::default()
            };
            let ragged_key = layout_key(0, 300.0, 200.0, &ragged, 1.0, 0, 1.0);
            assert_ne!(base, ragged_key, "composition change relays out");
            let accessibility_scaled = layout_key(0, 300.0, 200.0, &book_settings(), 1.0, 0, 2.0);
            assert_ne!(
                base, accessibility_scaled,
                "host accessibility text scale relays out"
            );
        }

        // --- phase: selection_char_offsets_are_assigned_across_words ---
        {
            let mut tr = TextRenderer::new();
            let blocks = vec![para("alpha beta gamma")];
            let layout = test_layout(
                &mut tr,
                &blocks,
                600.0,
                400.0,
                &book_settings(),
                "en-US",
                &FontChoice::SansSerif,
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
    }

    /// Ragged composition, hyphenation, hanging punctuation, CJK breaking and RTL alignment follow language and composition.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn microtypography_follows_language_and_composition() {
        // --- phase: ragged_composition_disables_book_microtypography ---
        {
            let mut tr = TextRenderer::new();
            let settings = Settings {
                composition: CompositionStyle::Ragged,
                ..Default::default()
            };
            let layout = test_layout(
                &mut tr,
                &[para(
                    "“Characteristically typographic paragraphs demonstrate hyphenation and spacing.”",
                )],
                145.0,
                600.0,
                &settings,
                "en-US",
                &FontChoice::Serif,
            );
            let lines = &layout.blocks[0].lines;
            assert!(
                lines.len() >= 2,
                "fixture wraps across several ragged lines"
            );
            assert!(
                lines
                    .iter()
                    .flat_map(|line| &line.runs)
                    .all(|run| !run.display_text.ends_with('-')),
                "ragged composition inserts no discretionary hyphens"
            );
            #[allow(
                clippy::float_cmp,
                reason = "ragged composition assigns glyph_scale exactly 1.0 (no computed \
                      expansion), so exact equality is the correct assertion"
            )]
            {
                assert!(
                    lines.iter().all(|line| line.glyph_scale == 1.0),
                    "ragged composition does not expand or condense glyphs"
                );
            }
            assert_eq!(lines[0].runs[0].x, 0.0, "opening punctuation does not hang");
        }

        // --- phase: language_aware_hyphenation_paints_only_the_exercised_break ---
        {
            let mut tr = TextRenderer::new();
            let blocks = vec![para("characteristically")];
            let layout = test_layout(
                &mut tr,
                &blocks,
                105.0,
                400.0,
                &book_settings(),
                "en-US",
                &FontChoice::Serif,
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

        // --- phase: opening_punctuation_hangs_into_the_margin_without_changing_source ---
        {
            let mut tr = TextRenderer::new();
            let blocks = vec![para("“Typography begins at the edge.”")];
            let layout = test_layout(
                &mut tr,
                &blocks,
                600.0,
                400.0,
                &book_settings(),
                "en-US",
                &FontChoice::Serif,
            );
            let block = &layout.blocks[0];
            let line = &block.lines[0];
            assert!(line.runs[0].x < 0.0, "opening quote protrudes left");
            for run in &line.runs {
                let source: String = block
                    .text
                    .chars()
                    .skip(run.char_start)
                    .take(run.char_len)
                    .collect();
                assert_eq!(source, run.text);
            }
        }

        // --- phase: unspaced_cjk_uses_unicode_breaks_and_inter_character_justification ---
        {
            let mut tr = TextRenderer::new();
            let text = "天地玄黃宇宙洪荒日月盈昃辰宿列張寒來暑往秋收冬藏";
            let layout = test_layout(
                &mut tr,
                &[para(text)],
                120.0,
                500.0,
                &book_settings(),
                "zh-Hant",
                &FontChoice::Serif,
            );
            let block = &layout.blocks[0];
            assert!(block.lines.len() >= 2, "CJK wraps without ASCII spaces");
            assert_eq!(block.text, text);
            assert!(
                block.lines[..block.lines.len() - 1]
                    .iter()
                    .any(|line| line.runs.len() > 1 && line.adjustment_ratio != 0.0),
                "non-final CJK lines participate in micro-justification"
            );
        }

        // --- phase: rtl_paragraphs_use_visual_bidi_order_and_right_align_the_final_line ---
        {
            let mut tr = TextRenderer::new();
            let layout = test_layout(
                &mut tr,
                &[para("مرحبا بالعالم 123")],
                500.0,
                400.0,
                &book_settings(),
                "ar",
                &FontChoice::Serif,
            );
            let line = &layout.blocks[0].lines[0];
            assert!(line.rtl);
            assert!(line.runs.windows(2).all(|pair| pair[0].x < pair[1].x));
            let logical_first = line
                .runs
                .iter()
                .find(|run| run.char_start == 0)
                .expect("first Arabic word");
            assert!(
                logical_first.x > line.runs[0].x,
                "logical first word is visually rightmost"
            );
            assert!(line.runs[0].x > 100.0, "short RTL final line aligns right");
        }
    }

    /// A run of `text` that starts a new line indented `indent` ems.
    fn line(text: &str, indent: u8) -> Span {
        Span {
            space_before: true,
            line_break: true,
            line_indent: indent,
            ..Span::plain(text)
        }
    }

    fn line_text(line: &LaidLine) -> String {
        line.runs
            .iter()
            .map(|run| run.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Verse sets one ragged line per source line, indents marked lines, hangs wrapped ones, and keeps the prose offsets.
    #[test]
    fn verse_lays_out_one_ragged_line_per_source_line() {
        // --- phase: verse_lines_are_ragged_indented_and_hang_when_wrapped ---
        {
            let mut tr = TextRenderer::new();
            let long =
                "The capitals of many nations, learned the customs of their dwellers, and endured";
            let verse = Block::Verse {
                spans: vec![
                    Span::plain("Tell me, O Muse,"),
                    line("Who, having overthrown", 0),
                    line("Of Ilium, wandered far", 2),
                    line(long, 0),
                ],
                quoted: false,
            };
            let layout = test_layout(
                &mut tr,
                std::slice::from_ref(&verse),
                360.0,
                2000.0,
                &book_settings(),
                "en-US",
                &FontChoice::Serif,
            );
            let block = &layout.blocks[0];
            assert_eq!(block.kind, LayoutKind::Verse { quoted: false });
            assert_eq!(block.text, verse.plain_text(), "offsets are the prose ones");
            assert_eq!(line_text(&block.lines[0]), "Tell me, O Muse,");
            assert_eq!(line_text(&block.lines[1]), "Who, having overthrown");
            assert_eq!(line_text(&block.lines[2]), "Of Ilium, wandered far");
            assert!(block.lines.len() >= 5, "the long line wraps");
            let x = |line: usize| block.lines[line].runs[0].x;
            let em = block.size;
            assert!((x(2) - x(1) - 2.0 * em).abs() < 0.5, "i2 indents two ems");
            assert!(
                (x(4) - x(3) - em).abs() < 0.5,
                "a wrapped line hangs one em"
            );
            #[allow(
                clippy::float_cmp,
                reason = "unjustified lines keep exactly the natural glyph scale 1.0"
            )]
            {
                assert!(block.lines.iter().all(|line| line.glyph_scale == 1.0));
            }
            assert!(
                block
                    .lines
                    .iter()
                    .flat_map(|line| &line.runs)
                    .all(|run| !run.display_text.ends_with('-')),
                "verse is not hyphenated"
            );
            for run in block.lines.iter().flat_map(|line| &line.runs) {
                let source: String = block
                    .text
                    .chars()
                    .skip(run.char_start)
                    .take(run.char_len)
                    .collect();
                assert_eq!(source, run.text);
            }
        }
    }

    /// A prose `<br/>` ends its line with natural spacing and no hanging indent.
    #[test]
    fn a_prose_break_ends_its_line_without_justifying_it() {
        {
            let mut tr = TextRenderer::new();
            let prose = Block::Paragraph {
                spans: vec![
                    Span::plain("Short line"),
                    line(
                        "and then a paragraph long enough to wrap and be justified across the measure",
                        0,
                    ),
                ],
            };
            let layout = test_layout(
                &mut tr,
                std::slice::from_ref(&prose),
                300.0,
                2000.0,
                &book_settings(),
                "en-US",
                &FontChoice::Serif,
            );
            let natural = test_layout(
                &mut tr,
                &[para("Short line")],
                300.0,
                2000.0,
                &book_settings(),
                "en-US",
                &FontChoice::Serif,
            );
            let block = &layout.blocks[0];
            assert_eq!(line_text(&block.lines[0]), "Short line");
            let xs = |line: &LaidLine| line.runs.iter().map(|run| run.x).collect::<Vec<_>>();
            assert_eq!(
                xs(&block.lines[0]),
                xs(&natural.blocks[0].lines[0]),
                "the broken line keeps natural spacing"
            );
            assert!(block.lines[1].runs[0].x.abs() < 0.5, "prose does not hang");
            assert_eq!(
                block.text,
                "Short line and then a paragraph long enough to wrap and be justified across the measure"
            );
        }
    }

    /// Standard Ebooks' Odyssey, Book I: a zero-width no-break space glues
    /// every em dash to the word before it.
    const ODYSSEY_BOOK_ONE_SUMMARY: &str = "A council of the gods\u{feff}—Deliberations concerning Ulysses\u{feff}—Mercury despatched to Calypso, to bid her send Ulysses to Ithaca\u{feff}—Visit of Pallas, in the shape of Mentor, to Telemachus, advising him to repair to Pylos and Sparta in quest of his father, Ulysses\u{feff}—Revels of the suitors of Penelope\u{feff}—Phemius, the minstrel, and his song of the return of the Grecians\u{feff}—The suitors rebuked by Telemachus.";

    fn painted_line(line: &LaidLine) -> String {
        let mut text = String::new();
        for (index, run) in line.runs.iter().enumerate() {
            if index > 0 && !text.ends_with(['—', '\u{feff}', '\u{2060}']) {
                text.push(' ');
            }
            text.push_str(&run.display_text);
        }
        text
    }

    /// At phone measures, em-dash compounds glued by invisible joiners
    /// break after the dash, and no justified line is left holding a lone
    /// short word.
    #[test]
    fn em_dash_compounds_break_without_stranding_words_at_phone_widths() {
        let mut tr = TextRenderer::new();
        let word_joined = ODYSSEY_BOOK_ONE_SUMMARY.replace("\u{feff}—", "\u{2060}—\u{2060}");
        for text in [ODYSSEY_BOOK_ONE_SUMMARY, word_joined.as_str()] {
            for width in (300..=400).step_by(2) {
                let width = width as f32;
                let layout = test_layout(
                    &mut tr,
                    &[para(text)],
                    width,
                    4000.0,
                    &book_settings(),
                    "en-US",
                    &FontChoice::Serif,
                );
                let block = &layout.blocks[0];
                let lines: Vec<String> = block.lines.iter().map(painted_line).collect();
                let size = block.size;
                for line in &lines[..lines.len() - 1] {
                    let natural = tr
                        .measure_font(
                            line,
                            size,
                            None,
                            TextWeight::Regular,
                            false,
                            &FontChoice::Serif,
                        )
                        .0;
                    assert!(
                        natural > width * 0.6,
                        "{width}px: underfull justified line {line:?} in {lines:#?}"
                    );
                }
                let source: String = block
                    .lines
                    .iter()
                    .flat_map(|line| &line.runs)
                    .map(|run| run.text.as_str())
                    .collect();
                assert_eq!(
                    source.replace(' ', ""),
                    block.text.replace(' ', ""),
                    "breaks never drop or duplicate source text"
                );
            }
        }
    }

    /// Word joiners and zero-width no-break spaces are invisible: they add
    /// no advance and paint no glyph in any reader face. A joiner does glue
    /// the dash to its words, which a kerned face (New Computer Modern) may
    /// then kern: the joined run is never wider than the plain one and at
    /// most a kern pair or two narrower.
    #[test]
    fn invisible_joiners_have_no_width() {
        const SIZE: f32 = 18.0;
        let mut tr = TextRenderer::new();
        for family in [FontChoice::Serif, FontChoice::SansSerif] {
            let mut width = |text: &str| {
                tr.measure_font(text, SIZE, None, TextWeight::Regular, false, &family)
                    .0
            };
            let plain = width("gods—Deliberations");
            for joined in [
                "gods\u{feff}—Deliberations",
                "gods\u{2060}—\u{2060}Deliberations",
            ] {
                let measured = width(joined);
                assert!(
                    measured < plain + 0.5 && plain - measured < 0.2 * SIZE,
                    "{joined:?} measures {measured} vs {plain}"
                );
            }
            assert!(width("\u{2060}\u{feff}") < 0.5);
        }
    }

    /// A block's selectable text, read from the book alone, is the text its
    /// layout holds, verse and runs of whitespace included.
    #[test]
    fn a_blocks_selectable_text_is_the_laid_out_one() {
        let mut tr = TextRenderer::new();
        let book = crate::content::bundled_sample().expect("bundled book");
        let mut blocks: Vec<Block> = book
            .chapters
            .iter()
            .flat_map(|chapter| chapter.blocks.iter().cloned())
            .collect();
        blocks.push(Block::Verse {
            spans: vec![
                Span::plain("Tell me,   O Muse,"),
                line("Who, having\toverthrown", 0),
                line(" Of Ilium ", 2),
            ],
            quoted: false,
        });
        blocks.push(para("  spaced   out \n words  "));
        let layout = test_layout(
            &mut tr,
            &blocks,
            360.0,
            2000.0,
            &book_settings(),
            "en-US",
            &FontChoice::Serif,
        );
        assert_eq!(layout.blocks.len(), blocks.len());
        for laid in &layout.blocks {
            let (text, hard_breaks) = selectable_text(&blocks[laid.index]);
            assert_eq!(text, laid.text, "block {}", laid.index);
            assert_eq!(text.chars().count(), laid.char_len, "block {}", laid.index);
            assert_eq!(hard_breaks, laid.hard_breaks, "block {}", laid.index);
        }
    }
}
