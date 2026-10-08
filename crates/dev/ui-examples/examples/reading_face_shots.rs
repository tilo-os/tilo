#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    reason = "a capture lays one fixed page out in small, bounded pixel math"
)]

//! A Books-style chapter page set in each reading face through the real
//! toolkit: the Reading role's optical cuts (title, body, footnote and
//! superscripts at their own sizes), small caps, italics, old-style figures
//! and a Greek and a Cyrillic line, on a light and a dark page.
//!
//! ```bash
//! cargo run -p ui-examples --example reading-face-shots -- \
//!     --shots target/look/reading/page.png --scale 2
//! ```
//!
//! Writes `<stem>-<face>-<theme>.png` next to the `--shots` path for every
//! [`ReadingFace`] and the themes `light` and `dark`, plus
//! `<stem>-cuts-<theme>.png`: one line of New Computer Modern at each size
//! from 9 to 48 px, so the switch between its cuts can be seen.

use std::path::{Path, PathBuf};

use tilo_ui::font_catalog::FontCatalog;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::text_render::{OpenTypeTag, OpticalSizing, TextRenderer, TextStyle};
use tilo_ui::theme::Theme;
use tilo_ui::tokens::{ReadingFace, TextRole};
use vello::Scene;
use vello::kurbo::{Affine, Line, Rect, Stroke};
use vello::peniko::{Color, Fill};

const WIDTH: f64 = 1280.0;
const HEIGHT: f64 = 1000.0;
const COLUMN: f64 = 640.0;
const TEXT_PX: f32 = 19.5;
const TITLE_PX: f32 = 48.0;
const LEADING: f32 = 1.62;

/// One run of a word: text, style and baseline raise in px.
struct Run {
    text: String,
    style: TextStyle,
    raise: f64,
}

/// A paragraph: words (each one or more runs set without a space), its first
/// line indent, and whether it is justified.
struct Paragraph {
    words: Vec<Vec<Run>>,
    indent: f64,
    justify: bool,
    leading: f64,
    ink: Color,
}

struct Page<'a> {
    scene: &'a mut Scene,
    text: &'a mut TextRenderer,
    transform: Affine,
}

impl Page<'_> {
    fn run_width(&mut self, run: &Run) -> f64 {
        f64::from(self.text.measure_style(&run.text, None, &run.style).0)
    }

    fn word_width(&mut self, word: &[Run]) -> f64 {
        word.iter().map(|run| self.run_width(run)).sum()
    }

    /// Set `paragraph` from `top` at `x`, `width` wide; returns the y below
    /// its last line.
    fn paragraph(&mut self, paragraph: &Paragraph, x: f64, top: f64, width: f64) -> f64 {
        // The paragraph's own style is its largest run's: a leading
        // superscript does not set the spacing.
        let Some(largest) = paragraph
            .words
            .iter()
            .flatten()
            .max_by(|a, b| a.style.size.total_cmp(&b.style.size))
        else {
            return top;
        };
        let base = largest.style.clone();
        let space = f64::from(self.text.measure_style("a a", None, &base).0)
            - f64::from(self.text.measure_style("aa", None, &base).0);
        let widths: Vec<f64> = paragraph
            .words
            .iter()
            .map(|word| self.word_width(word))
            .collect();
        let mut lines: Vec<std::ops::Range<usize>> = Vec::new();
        let mut start = 0;
        let mut used = paragraph.indent;
        for (index, w) in widths.iter().enumerate() {
            let next = if index == start {
                used + w
            } else {
                used + space + w
            };
            if next > width && index > start {
                lines.push(start..index);
                start = index;
                used = *w;
            } else {
                used = next;
            }
        }
        lines.push(start..widths.len());
        let ascent = f64::from(self.text.first_baseline_style("x", None, &base));
        let line_box = f64::from(base.size) * paragraph.leading;
        let half_leading = (line_box - f64::from(base.size) * 1.2) / 2.0;
        let mut y = top;
        let count = lines.len();
        for (number, range) in lines.into_iter().enumerate() {
            let indent = if number == 0 { paragraph.indent } else { 0.0 };
            let natural: f64 = widths
                .get(range.clone())
                .unwrap_or_default()
                .iter()
                .sum::<f64>()
                + space * (range.len().saturating_sub(1)) as f64;
            let gaps = range.len().saturating_sub(1).max(1) as f64;
            let gap = if paragraph.justify && number + 1 < count {
                space + (width - indent - natural) / gaps
            } else {
                space
            };
            let baseline = y + half_leading.max(0.0) + ascent;
            let mut pen = x + indent;
            for index in range {
                for run in paragraph.words.get(index).into_iter().flatten() {
                    let run_baseline =
                        f64::from(self.text.first_baseline_style(&run.text, None, &run.style));
                    let at = Affine::translate((pen, baseline - run.raise - run_baseline));
                    self.text.draw_style(
                        self.scene,
                        &run.text,
                        paragraph.ink,
                        self.transform * at,
                        None,
                        &run.style,
                    );
                    pen += self.run_width(run);
                }
                pen += gap;
            }
            y += line_box;
        }
        y
    }

    fn line(&mut self, text: &str, style: &TextStyle, ink: Color, x: f64, top: f64) {
        self.text.draw_style(
            self.scene,
            text,
            ink,
            self.transform * Affine::translate((x, top)),
            None,
            style,
        );
    }
}

fn words(text: &str, style: &TextStyle) -> Vec<Vec<Run>> {
    text.split(' ')
        .filter(|w| !w.is_empty())
        .map(|w| {
            vec![Run {
                text: w.to_owned(),
                style: style.clone(),
                raise: 0.0,
            }]
        })
        .collect()
}

fn superscript(text: &str, style: &TextStyle) -> Run {
    let size = style.size * 0.62;
    Run {
        text: text.to_owned(),
        style: style.clone().with_content_size(size),
        raise: f64::from(style.size) * 0.36,
    }
}

fn chapter_page(
    scene: &mut Scene,
    text: &mut TextRenderer,
    theme: &Theme,
    label: &str,
    scale: f64,
) {
    let colors = &theme.tokens.colors;
    let transform = Affine::scale(scale);
    let ink = theme.text_primary;
    let ink2 = theme.text_dim;
    let ink3 = theme.text_tertiary;
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surfaces.content,
        None,
        &Rect::new(0.0, 0.0, WIDTH, HEIGHT),
    );
    let hair = theme.border_standard;
    for y in [56.0, HEIGHT - 40.0] {
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            hair,
            None,
            &Line::new((0.0, y), (WIDTH, y)),
        );
    }
    let mut page = Page {
        scene,
        text,
        transform,
    };
    // Chrome, in the UI face.
    let label_style = theme.text(TextRole::Label);
    let caption = theme.text(TextRole::Caption);
    page.line("Pride and Prejudice", &label_style, ink, 52.0, 19.0);
    page.line("Jane Austen", &caption, ink3, 200.0, 20.0);
    let (w, _) = page.text.measure_style("Chapter 1 of 61", None, &caption);
    page.line(
        "Chapter 1 of 61",
        &caption,
        ink3,
        WIDTH - 20.0 - f64::from(w),
        20.0,
    );
    let (w, _) = page.text.measure_style(label, None, &caption);
    page.line(label, &caption, ink3, WIDTH - 24.0 - f64::from(w), 70.0);
    let foot = caption.clone().numeric();
    page.line("Page 3 of 342", &foot, ink3, 24.0, HEIGHT - 28.0);
    let (w, _) = page
        .text
        .measure_style("12 min left in chapter", None, &foot);
    page.line(
        "12 min left in chapter",
        &foot,
        ink3,
        WIDTH - 24.0 - f64::from(w),
        HEIGHT - 28.0,
    );

    // The page, in the Reading role at the book's own sizes.
    let body = theme
        .text(TextRole::Reading)
        .with_content_size(TEXT_PX)
        .with_line_height(LEADING);
    let x = (WIDTH - COLUMN) / 2.0;
    let mut y = 56.0 + 64.0;
    let kicker = body
        .clone()
        .with_content_size(TEXT_PX * 0.95)
        .with_small_caps(true)
        .with_font_feature(OpenTypeTag::ALL_SMALL_CAPS, 1)
        .with_tracking(0.06);
    page.line("Chapter one", &kicker, ink2, x, y);
    y += f64::from(TEXT_PX) * 1.62 + 8.0;
    let title = body
        .clone()
        .with_content_size(TITLE_PX)
        .with_line_height(1.08)
        .with_tracking(-0.005);
    let title_para = Paragraph {
        words: words("A truth universally acknowledged", &title),
        indent: 0.0,
        justify: false,
        leading: 1.08,
        ink,
    };
    y = page.paragraph(&title_para, x, y, COLUMN) + 18.0;

    // Epigraph: Greek and Russian upright, English italic, the source in
    // small caps.
    let epi = body.clone().with_content_size(TEXT_PX * 0.92);
    let epi_x = x + 48.0;
    for line in [
        "Η αρχή είναι το ήμισυ του παντός.",
        "Хорошее начало — половина дела.",
    ] {
        let para = Paragraph {
            words: words(line, &epi),
            indent: 0.0,
            justify: false,
            leading: 1.5,
            ink: ink2,
        };
        y = page.paragraph(&para, epi_x, y, COLUMN - 48.0);
    }
    let mut epi_words = words("Well begun is half done.", &epi.clone().with_italic(true));
    epi_words.push(vec![Run {
        text: "—".into(),
        style: epi.clone(),
        raise: 0.0,
    }]);
    epi_words.push(vec![Run {
        text: "Aristotle".into(),
        style: epi.with_small_caps(true),
        raise: 0.0,
    }]);
    let para = Paragraph {
        words: epi_words,
        indent: 0.0,
        justify: false,
        leading: 1.5,
        ink: ink2,
    };
    y = page.paragraph(&para, epi_x, y, COLUMN - 48.0) + 28.0;

    // Body: the opening words in small caps, italics, old-style figures and
    // a footnote marker.
    let caps = body.clone().with_small_caps(true).with_tracking(0.02);
    let mut first = words("It is a truth universally acknowledged,", &caps);
    first.extend(words(
        "that a single man in possession of a good fortune, must be in want of a wife.",
        &body,
    ));
    let paragraphs: Vec<Vec<Vec<Run>>> = vec![
        first,
        words(
            "However little known the feelings or views of such a man may be on his first \
             entering a neighbourhood, this truth is so well fixed in the minds of the \
             surrounding families, that he is considered the rightful property of some one \
             or other of their daughters.",
            &body,
        ),
        {
            let mut w = words(
                "“My dear Mr. Bennet,” said his lady to him one day, “have you heard that",
                &body,
            );
            w.extend(words("Netherfield Park", &body.clone().with_italic(true)));
            w.extend(words(
                "is let at last?” Mr. Bennet replied that he had not.",
                &body,
            ));
            w
        },
        {
            let mut w = words(
                "“But it is,” returned she; “for Mrs. Long has just been here, and she told me \
                 all about it.” Mr. Bennet made no answer. “Do you not want to know who has \
                 taken it?” cried his wife impatiently. “",
                &body,
            );
            if let Some(last) = w.last_mut() {
                last.push(Run {
                    text: "You".into(),
                    style: body.clone().with_italic(true),
                    raise: 0.0,
                });
            }
            w.extend(words(
                "want to tell me, and I have no objection to hearing it.” This was invitation",
                &body,
            ));
            w.push(vec![
                Run {
                    text: "enough.".into(),
                    style: body.clone(),
                    raise: 0.0,
                },
                superscript("1", &body),
            ]);
            w.extend(words(
                "In 1813 the novel sold its first 1,500 copies within the year.",
                &body,
            ));
            w
        },
    ];
    for (index, words) in paragraphs.into_iter().enumerate() {
        let para = Paragraph {
            words,
            indent: if index == 0 {
                0.0
            } else {
                1.4 * f64::from(TEXT_PX)
            },
            justify: true,
            leading: f64::from(LEADING),
            ink,
        };
        y = page.paragraph(&para, x, y, COLUMN);
    }

    // Footnote.
    y += 26.0;
    page.scene.stroke(
        &Stroke::new(1.0),
        transform,
        hair,
        None,
        &Line::new((x, y), (x + COLUMN * 0.4, y)),
    );
    y += 10.0;
    let note = body.with_content_size(TEXT_PX * 0.74);
    let mut note_words = vec![vec![superscript("1", &note)]];
    note_words.extend(words(
        "First published in three volumes by Thomas Egerton, London, 28 January 1813.",
        &note,
    ));
    let para = Paragraph {
        words: note_words,
        indent: 0.0,
        justify: false,
        leading: 1.45,
        ink: ink2,
    };
    page.paragraph(&para, x, y, COLUMN * 0.4);
}

/// One line of the default face per size: the 8 pt cut, Book and Regular
/// in turn as the size grows.
fn cuts_page(scene: &mut Scene, text: &mut TextRenderer, theme: &Theme, scale: f64) {
    let transform = Affine::scale(scale);
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.content,
        None,
        &Rect::new(0.0, 0.0, WIDTH, HEIGHT),
    );
    let reading = theme.text(TextRole::Reading);
    let caption = theme.text(TextRole::Caption).numeric();
    // Columns: what the size picks, then each cut pinned by its design size.
    let columns: [(&str, Option<f32>, f64); 4] = [
        ("automatic", None, 90.0),
        ("8 pt", Some(10.0), 390.0),
        ("Book", Some(16.0), 690.0),
        ("Regular", Some(40.0), 990.0),
    ];
    for (name, _, x) in columns {
        text.draw_style(
            scene,
            name,
            theme.text_tertiary,
            transform * Affine::translate((x, 16.0)),
            None,
            &caption,
        );
    }
    let mut y = 44.0;
    for size in [
        9.0, 11.0, 12.0, 13.0, 14.0, 16.0, 19.5, 22.0, 24.0, 26.0, 28.0, 32.0, 36.0,
    ] {
        text.draw_style(
            scene,
            &format!("{size} px"),
            theme.text_tertiary,
            transform * Affine::translate((24.0, y)),
            None,
            &caption,
        );
        for (_, design, x) in columns {
            let mut style = reading.clone().with_content_size(size);
            if let Some(design) = design {
                style = style.with_optical_sizing(OpticalSizing::Value(design));
            }
            text.draw_style(
                scene,
                "Hamburg 1813",
                theme.text_primary,
                transform * Affine::translate((x, y)),
                None,
                &style,
            );
        }
        y += f64::from(size) * 1.4 + 14.0;
    }
}

fn arg(name: &str) -> Option<String> {
    let mut args = std::env::args().skip_while(|a| a != name);
    args.next()?;
    args.next()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let shots =
        PathBuf::from(arg("--shots").unwrap_or_else(|| "target/look/reading/page.png".into()));
    let scale: f64 = arg("--scale").and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let dir = shots.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let stem = shots
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("page")
        .to_owned();
    // The checked-in catalog, without host fonts: the faces Tilo ships.
    let catalog = FontCatalog::controlled_fixture(tilo_ui::font_catalog::checkout_root(), "en-US");
    let mut text = TextRenderer::with_font_catalog(&catalog)?;
    // The SDK's standalone catalog, as a third-party app bundles it: no
    // New Computer Modern Regular, so Book sets the title.
    let mut standalone = TextRenderer::with_font_system(
        FontCatalog::product(catalog.root(), "en-US")
            .load_bytes(|name| std::fs::read(catalog.root().join(name)).ok())?,
    );
    let mut renderer = OffscreenRenderer::new((WIDTH * scale) as u32, (HEIGHT * scale) as u32);
    for (theme_name, light) in [("light", true), ("dark", false)] {
        let theme = if light { Theme::light() } else { Theme::dark() };
        let mut scene = Scene::new();
        let label = format!("New Computer Modern · standalone catalog · {theme_name}");
        chapter_page(&mut scene, &mut standalone, &theme, &label, scale);
        let out = dir.join(format!("{stem}-standalone-{theme_name}.png"));
        renderer.render_png(&scene, theme.tokens.colors.surfaces.content, &out);
        println!("{}", out.display());
        for face in ReadingFace::ALL {
            let mut theme = if light { Theme::light() } else { Theme::dark() };
            theme.tokens.typography.set_reading_face(face.name());
            let mut scene = Scene::new();
            let label = format!("{} · {theme_name}", face.name());
            chapter_page(&mut scene, &mut text, &theme, &label, scale);
            let slug = face.name().to_ascii_lowercase().replace(' ', "-");
            let out = dir.join(format!("{stem}-{slug}-{theme_name}.png"));
            renderer.render_png(&scene, theme.tokens.colors.surfaces.content, &out);
            println!("{}", out.display());
        }
        let theme = if light { Theme::light() } else { Theme::dark() };
        let mut scene = Scene::new();
        cuts_page(&mut scene, &mut text, &theme, scale);
        let out = dir.join(format!("{stem}-cuts-{theme_name}.png"));
        renderer.render_png(&scene, theme.tokens.colors.surfaces.content, &out);
        println!("{}", out.display());
    }
    Ok(())
}
