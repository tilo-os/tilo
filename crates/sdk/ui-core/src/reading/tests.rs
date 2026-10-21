//! The reader's rules, each pinned as the design states it.

use std::rc::Rc;
use std::sync::Arc;

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::*;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

fn renderer() -> TextRenderer {
    crate::font_catalog::fixture_renderer()
}

fn paragraph(text: &str) -> Block {
    Block::Paragraph {
        spans: vec![Span::plain(text)],
    }
}

/// A long article: enough paragraphs to scroll many screens.
fn long_article() -> Rc<Document> {
    let text = "Reading on a screen should be as calm as reading on paper: a line \
                long enough to carry a thought, short enough for the eye to find \
                the start of the next one, and spacing that lets the text breathe.";
    let mut blocks = vec![Block::Heading {
        level: 1,
        spans: vec![Span::plain("A section")],
    }];
    for _ in 0..60 {
        blocks.push(paragraph(text));
    }
    Rc::new(Document {
        title: "On reading".into(),
        byline: "Ada".into(),
        site: "example.org".into(),
        language: "en".into(),
        blocks,
        ..Document::default()
    })
}

fn paint(view: &mut ReaderView, text: &mut TextRenderer, theme: &Theme, area: Rect) {
    view.paint(&mut Scene::new(), Affine::IDENTITY, text, theme, area);
}

/// A line never holds more than 72 characters at any window width; wherever the window can hold
/// 60 characters and the margins, it holds at least 60; the column is centred, so the margins
/// take the rest and grow with the window.
#[test]
fn the_measure_stays_within_60_to_72_characters_at_any_width() {
    let mut text = renderer();
    let theme = Theme::light();
    for measure in Measure::ALL {
        let prefs = ReadingPreferences {
            measure,
            ..ReadingPreferences::default()
        };
        let family = prefs.font.family(None);
        let style = flow::run_style(prefs.body_px(), false, false, false, &family);
        let char_width = average_char_width(&mut text, &style);
        let mut last_margin = 0.0;
        for width in (320..=3200).step_by(40) {
            let width = f64::from(width);
            let margin = min_margin(&theme, width);
            let geometry = reader_geometry(width, char_width, measure, margin);
            let chars = geometry.measure / char_width;
            assert!(
                chars <= MAX_MEASURE_CHARS + 0.01,
                "{measure:?} at {width}: {chars} characters"
            );
            if width - margin * 2.0 >= f64::from(char_width * MIN_MEASURE_CHARS) {
                assert!(
                    chars >= MIN_MEASURE_CHARS - 0.01,
                    "{measure:?} at {width}: only {chars} characters"
                );
            }
            // Centred, and the margins grow with the window.
            let right = width - geometry.left - f64::from(geometry.measure);
            assert!((geometry.left - right).abs() < 0.5, "centred at {width}");
            assert!(
                geometry.left + 0.5 >= last_margin,
                "margins grow at {width}"
            );
            last_margin = geometry.left;
        }
    }

    // The laid lines keep to the measure too.
    let mut view = ReaderView::new();
    view.set_document("article", long_article(), None);
    paint(
        &mut view,
        &mut text,
        &theme,
        Rect::new(0.0, 0.0, 2400.0, 900.0),
    );
    let geometry = view.geometry();
    let layout = view.layout().expect("laid out");
    for block in layout
        .blocks
        .iter()
        .filter(|b| b.kind == flow::LayoutKind::Paragraph)
    {
        for line in &block.lines {
            let chars: usize = line.runs.iter().map(|run| run.char_len + 1).sum();
            assert!(chars <= 74, "a line of {chars} characters");
            let right = line.runs.last().map_or(0.0, |run| {
                run.x
                    + text
                        .measure_style(
                            &run.display_text,
                            None,
                            &flow::run_style(
                                run.size,
                                run.bold,
                                run.italic,
                                run.small_caps,
                                &view_family(),
                            ),
                        )
                        .0
            });
            assert!(
                right <= geometry.measure + 1.0,
                "a line runs past the measure"
            );
        }
    }
}

fn view_family() -> crate::text_render::FontChoice {
    ReadingFont::Serif.family(None)
}

/// One step larger sets the body text one step larger and lays the text out again in it, and
/// the passage on screen stays on screen.
#[test]
fn ctrl_plus_and_minus_change_the_reading_size() {
    let mut prefs = ReadingPreferences::default();
    let base = prefs.body_px();
    prefs.step_size(1);
    assert!((prefs.size - 1.1).abs() < 1e-4);
    assert!(prefs.body_px() > base);
    prefs.step_size(-1);
    prefs.step_size(-1);
    assert!((prefs.size - 0.9).abs() < 1e-4);
    // Never past the ends.
    for _ in 0..40 {
        prefs.step_size(1);
    }
    assert!((prefs.size - *ReadingPreferences::SIZE_RANGE.end()).abs() < 1e-4);

    let mut text = renderer();
    let theme = Theme::light();
    let area = Rect::new(0.0, 0.0, 900.0, 700.0);
    let mut view = ReaderView::new();
    view.set_document("article", long_article(), None);
    paint(&mut view, &mut text, &theme, area);
    let before = view.layout().map(|l| l.blocks[3].size).unwrap_or_default();
    view.show_place(ReadingPlace {
        block: 20,
        fraction: 0.0,
        progress: 0.3,
    });
    let mut larger = view.preferences();
    larger.step_size(1);
    view.set_preferences(larger);
    paint(&mut view, &mut text, &theme, area);
    let after = view.layout().map(|l| l.blocks[3].size).unwrap_or_default();
    assert!(after > before, "the text is set larger: {before} → {after}");
    assert_eq!(
        view.place().map(|p| p.block),
        Some(20),
        "the passage stays on screen"
    );
}

/// The place a reader reports, handed back when the same document opens again, shows the same
/// passage.
#[test]
fn the_reading_position_survives_reopening() {
    let mut text = renderer();
    let theme = Theme::dark();
    let area = Rect::new(0.0, 0.0, 1000.0, 760.0);
    let mut view = ReaderView::new();
    view.set_document("feeds:article-1", long_article(), None);
    paint(&mut view, &mut text, &theme, area);
    view.show_place(ReadingPlace {
        block: 33,
        fraction: 0.5,
        progress: 0.5,
    });
    paint(&mut view, &mut text, &theme, area);
    let kept = view.place().expect("a place");
    assert_eq!(kept.block, 33);

    // Closed and opened again in a new window.
    let mut reopened = ReaderView::new();
    reopened.set_document("feeds:article-1", long_article(), Some(kept));
    paint(&mut reopened, &mut text, &theme, area);
    let shown = reopened.place().expect("a place");
    assert_eq!(shown.block, kept.block);
    assert!((shown.fraction - kept.fraction).abs() < 0.05);
    assert!((reopened.offset() - view.offset()).abs() < 1.0);

    // Another document opens at its own top.
    reopened.set_document("feeds:article-2", long_article(), None);
    paint(&mut reopened, &mut text, &theme, area);
    assert!(reopened.offset().abs() < 0.5);
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([200, 80, 40, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("encode");
    bytes
}

/// A picture's box is reserved when the text is laid out (from its size, or a fixed shape when
/// the source gives none), and the picture is fitted inside it when it arrives; every line
/// stays where it was.
#[test]
fn images_never_shift_the_text_when_they_load() {
    let mut text = renderer();
    let theme = Theme::light();
    let area = Rect::new(0.0, 0.0, 1100.0, 800.0);
    let mut blocks = vec![paragraph("Before the pictures.")];
    for (src, width, height) in [
        ("https://example.org/known.png", Some(640), Some(360)),
        ("https://example.org/unknown.png", None, None),
    ] {
        blocks.push(Block::Figure {
            image: Image {
                src: src.into(),
                alt: "A picture".into(),
                width,
                height,
            },
            caption: vec![Span::plain("A caption under it.")],
        });
        blocks.push(paragraph("Text after the picture, which must not move."));
    }
    let document = Rc::new(Document {
        title: "Pictures".into(),
        blocks,
        ..Document::default()
    });
    let mut view = ReaderView::new();
    view.set_document("pictures", document, None);
    paint(&mut view, &mut text, &theme, area);
    let tops = |view: &ReaderView| -> Vec<(f32, f32)> {
        view.layout()
            .expect("laid out")
            .blocks
            .iter()
            .map(|b| (b.top, b.bottom))
            .collect()
    };
    let before = tops(&view);

    let bytes = png(400, 300);
    let fetch: crate::image_store::Fetcher = Arc::new(move |_url: &str| Some(bytes.clone()));
    let images = crate::image_store::ImageStore::with_max_dimension(&fetch, 1024);
    view.set_images(images.clone());
    paint(&mut view, &mut text, &theme, area);
    images.block_until_idle(crate::time::Duration::from_secs(5));
    paint(&mut view, &mut text, &theme, area);
    assert!(
        images.get("https://example.org/known.png").is_some(),
        "loaded"
    );
    assert_eq!(
        tops(&view),
        before,
        "the text never moves when pictures arrive"
    );

    // The reserved box follows the source's size and keeps a fixed shape
    // without one.
    let known = Image {
        width: Some(640),
        height: Some(360),
        ..Image::default()
    };
    let (w, h) = flow::figure_box(&known, 600.0, 900.0);
    assert!((w - 640.0).abs() < 0.5 && (h - 360.0).abs() < 0.5);
    let (w, h) = flow::figure_box(&Image::default(), 600.0, 900.0);
    assert!((w - 600.0).abs() < 0.5 && (h - 400.0).abs() < 0.5);
}

/// Space moves down by the screen's height less one line of body text, so the last line read is
/// the first line seen; Shift+Space moves back by the same.
#[test]
fn space_pages_by_one_screen_minus_one_line() {
    let _settled = crate::motion::testing::settled();
    let mut text = renderer();
    let theme = Theme::light();
    let area = Rect::new(0.0, 0.0, 900.0, 640.0);
    let mut view = ReaderView::new();
    view.set_document("article", long_article(), None);
    paint(&mut view, &mut text, &theme, area);
    let line = view.line_height();
    assert!(line > 0.0);
    assert!(view.key(
        &Key::Named(NamedKey::Space),
        ModifiersState::empty(),
        &theme
    ));
    paint(&mut view, &mut text, &theme, area);
    assert!(
        (view.offset() - (area.height() - line)).abs() < 0.5,
        "one screen less a line: {} vs {}",
        view.offset(),
        area.height() - line
    );
    assert!(view.key(
        &Key::Named(NamedKey::Space),
        ModifiersState::empty(),
        &theme
    ));
    paint(&mut view, &mut text, &theme, area);
    assert!((view.offset() - 2.0 * (area.height() - line)).abs() < 0.5);
    assert!(view.key(&Key::Named(NamedKey::Space), ModifiersState::SHIFT, &theme));
    paint(&mut view, &mut text, &theme, area);
    assert!((view.offset() - (area.height() - line)).abs() < 0.5);
}

#[test]
fn lowering_keeps_lists_quotes_code_figures_and_tables() {
    let html = r#"
        <p>Three things:</p>
        <ol start="1"><li>First <em>one</em></li><li>Second<ol><li>Nested</li></ol></li><li>Third</li></ol>
        <ul><li>A point</li></ul>
        <blockquote><p>Quoted words.</p><p>More of them.</p></blockquote>
        <pre class="language-rust"><code>fn main() {
    println!("hi");
}</code></pre>
        <figure><img src="/a.jpg" width="800" height="400" alt="A view"><figcaption>The view.</figcaption></figure>
        <table><tr><th>Name</th><th>Size</th></tr><tr><td>One</td><td>1</td></tr></table>
    "#;
    let document = extract::fragment(
        html,
        extract::Options {
            base_url: Some("https://example.org/post/"),
            ..extract::Options::default()
        },
    );
    let items: Vec<(String, bool, usize, u8)> = document
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::ListItem {
                spans,
                ordered,
                index,
                depth,
            } => Some((spans_text(spans), *ordered, *index, *depth)),
            _ => None,
        })
        .collect();
    assert_eq!(
        items,
        vec![
            ("First one".to_owned(), true, 1, 0),
            ("Second".to_owned(), true, 2, 0),
            ("Nested".to_owned(), true, 1, 1),
            ("Third".to_owned(), true, 3, 0),
            ("A point".to_owned(), false, 1, 0),
        ]
    );
    let quotes = document
        .blocks
        .iter()
        .filter(|b| matches!(b, Block::Quote { .. }))
        .count();
    assert_eq!(quotes, 2);
    assert!(document.blocks.iter().any(|b| matches!(
        b,
        Block::Code { text, language: Some(language) }
            if language == "rust" && text.contains("    println!(\"hi\");")
    )));
    assert!(document.blocks.iter().any(|b| matches!(
        b,
        Block::Figure { image, caption }
            if image.src == "https://example.org/a.jpg"
                && image.width == Some(800)
                && spans_text(caption) == "The view."
    )));
    assert!(document.blocks.iter().any(|b| matches!(
        b,
        Block::Table { rows } if rows.len() == 2 && rows[0].cells[0].header
    )));
}

#[test]
fn an_article_is_found_in_its_page_without_the_chrome_around_it() {
    let paragraph = "This paragraph is the article's own prose, long enough to count, \
                     with commas, clauses, and the kind of sentences people write.";
    let body: String = (0..8).map(|_| format!("<p>{paragraph}</p>")).collect();
    let html = format!(
        r##"<html lang="en"><head><title>The Title | Example</title>
        <meta property="og:site_name" content="Example">
        <meta name="author" content="Ada Lovelace"></head>
        <body><nav><a href="/">Home</a><a href="/about">About</a></nav>
        <div class="sidebar"><p>Subscribe to our newsletter for more, every week, free.</p></div>
        <article><h1>The Title</h1>{body}
        <p>A note<sup><a href="#fn1" id="r1">1</a></sup> here.</p>
        <div class="share-buttons"><a href="https://x.example/share">Share</a></div>
        <section class="footnotes"><ol><li id="fn1">The note's own text. <a href="#r1" class="footnote-backref">↩</a></li></ol></section>
        </article>
        <footer><p>Copyright notice that is not the article at all.</p></footer></body></html>"##
    );
    let document = extract::article(&html, "https://www.example.org/2026/title").expect("found");
    assert_eq!(document.title, "The Title");
    assert_eq!(document.byline, "Ada Lovelace");
    assert_eq!(document.site, "Example");
    assert_eq!(document.language, "en");
    let text: Vec<String> = document.blocks.iter().map(Block::plain_text).collect();
    assert!(text.iter().all(|t| !t.contains("newsletter")), "{text:?}");
    assert!(text.iter().all(|t| !t.contains("Copyright")), "{text:?}");
    assert!(text.iter().all(|t| !t.contains("Share")), "{text:?}");
    assert!(
        text.iter().all(|t| !t.contains("note's own text")),
        "{text:?}"
    );
    // The footnote opens in place: its reference names it.
    assert_eq!(document.footnotes.len(), 1);
    assert_eq!(
        spans_text(&document.footnotes[0].spans),
        "The note’s own text."
    );
    assert!(document.blocks.iter().any(|block| {
        block
            .spans()
            .iter()
            .any(|span| span.note == Some(0) && span.baseline == Baseline::Super)
    }));
    // The header says the title once.
    let presented = document.presented_blocks(None);
    let titles = presented
        .iter()
        .filter(|b| b.plain_text() == "The Title")
        .count();
    assert_eq!(titles, 1);
}

#[test]
fn an_excerpt_never_starts_with_its_title() {
    let title = "The AI Pascal's Wager";
    assert_eq!(
        strip_leading_title("The AI Pascal's Wager Should you bet on it?", title),
        "Should you bet on it?"
    );
    assert_eq!(
        strip_leading_title("the ai pascal’s wager: should you", title),
        "should you"
    );
    assert_eq!(
        strip_leading_title("Should you bet on the AI?", title),
        "Should you bet on the AI?"
    );
    assert_eq!(
        strip_leading_title("The AI and more", title),
        "The AI and more"
    );
}

#[test]
fn typewriter_punctuation_is_set_properly_outside_code() {
    let document = extract::fragment(
        r#"<p>"Quoted" -- and it's done... <code>"raw" -- x</code></p>"#,
        extract::Options::default(),
    );
    let spans = document.blocks[0].spans();
    let prose: String = spans
        .iter()
        .filter(|s| !s.code)
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(prose.contains("\u{201c}Quoted\u{201d}"), "{prose}");
    assert!(prose.contains('\u{2013}'), "{prose}");
    assert!(prose.contains("it\u{2019}s"), "{prose}");
    assert!(prose.contains('\u{2026}'), "{prose}");
    let code: Vec<&str> = spans
        .iter()
        .filter(|s| s.code)
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(code, vec!["\"raw\" -- x"]);
}

#[test]
fn the_parser_closes_what_html_leaves_open() {
    let dom = html::Dom::parse("<p>one<p>two<ul><li>a<li>b</ul><p>caf&eacute; &amp; &#8217;");
    let paragraphs = dom.find_all(html::Dom::root(), "p");
    assert_eq!(paragraphs.len(), 3);
    assert_eq!(dom.collapsed_text(paragraphs[0]), "one");
    assert_eq!(dom.find_all(html::Dom::root(), "li").len(), 2);
    assert_eq!(dom.collapsed_text(paragraphs[2]), "café & \u{2019}");
    // Scripts and styles never reach the text.
    let dom = html::Dom::parse("<p>a<script>var x = '<p>';</script>b</p><style>p{}</style>");
    assert_eq!(dom.collapsed_text(html::Dom::root()), "ab");
}

#[test]
fn the_warm_paper_follows_the_system_polarity_and_keeps_its_ink_readable() {
    for theme in [Theme::light(), Theme::dark()] {
        let system = ReaderPalette::resolve(Paper::System, &theme);
        assert!(system.page.is_none());
        let warm = ReaderPalette::resolve(Paper::Warm, &theme);
        assert!(warm.page.is_some());
        assert_eq!(warm.is_dark(), system.is_dark(), "{}", theme.name);
        for palette in [system, warm] {
            assert!(crate::a11y::contrast_ratio(palette.ink, palette.ground) >= 4.5);
            assert!(crate::a11y::contrast_ratio(palette.ink_dim, palette.ground) >= 4.5);
        }
    }
    // High contrast reaches the page.
    let contrast = Theme::dark().with_high_contrast();
    assert!(
        ReaderPalette::resolve(Paper::Warm, &contrast)
            .page
            .is_none()
    );
}

#[test]
fn find_in_place_highlights_every_match_and_reveals_the_current_one() {
    use crate::widget::find_bar::{FindOptions, Findable};
    let mut text = renderer();
    let theme = Theme::light();
    let area = Rect::new(0.0, 0.0, 900.0, 600.0);
    let mut view = ReaderView::new();
    view.set_document("article", long_article(), None);
    paint(&mut view, &mut text, &theme, area);
    let found = view.find("breathe", FindOptions::default());
    assert_eq!(found.len(), 60);
    view.reveal(40);
    paint(&mut view, &mut text, &theme, area);
    assert!(
        view.offset() > 0.0,
        "the current match is brought into view"
    );
    view.clear_find();
}
