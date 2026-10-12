//! Tilo Sans' distinct letters at UI sizes, rendered by Tilo's own text
//! stack from the shipped font catalog: what "Always distinguish similar
//! letters" changes, for review on a high-resolution screen.
//!
//! ```bash
//! cargo run -p ui-examples \
//!     --example distinct-letters-specimen -- --out target/look/distinct-letters-specimen
//! ```
//!
//! Writes, for each theme (`light`, `dark`):
//!
//! - `specimen-<theme>.png`: confusable strings at 12, 13, 14 and 17 px in
//!   Regular, Bold and Condensed Bold, upright and italic; the setting off on
//!   the left, on on the right; laid out at those sizes and drawn at 4x
//!   device scale, as a `HiDPI` display draws them.
//! - `glyphs-<theme>.png`: I l 1 | 0 O at 14 px drawn at 16x, so every glyph
//!   is rasterised from its outlines at 224 px; the default forms (old) on
//!   the left, the distinct forms (new) on the right.

use std::path::{Path, PathBuf};

use tilo_ui::State;
use tilo_ui::font_catalog::FontCatalog;
use tilo_ui::headless::FrameHarness;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::permissions::UserId;
use tilo_ui::text_render::{OpenTypeTag, TextRenderer, TextWeight};
use tilo_ui::theme::Theme;
use tilo_ui::tokens::TextRole;
use tilo_ui::widget::{Column, Container, Element, Row, Text};

/// One style of the specimen: label, weight, width axis, italic.
const STYLES: [(&str, TextWeight, f32, bool); 6] = [
    ("Regular", TextWeight::Regular, 100.0, false),
    ("Bold", TextWeight::Bold, 100.0, false),
    ("Condensed Bold", TextWeight::Bold, 75.0, false),
    ("Italic", TextWeight::Regular, 100.0, true),
    ("Bold Italic", TextWeight::Bold, 100.0, true),
    ("Condensed Bold Italic", TextWeight::Bold, 75.0, true),
];
const SIZES: [f32; 4] = [12.0, 13.0, 14.0, 17.0];
/// Confusables, a word, a run of l's and I's, a serial, a URL and a password.
const SPECIMEN: &str =
    "Il1| O0o   Illinois   ll1Il   SN 0O1I-l0O8   paypa1.com/l0gin?id=I0O   Il1|O0o-lI0";
const GLYPHS: &str = "I l 1 | 0 O";

fn styled(text: &str, size: f32, weight: TextWeight, width: f32, italic: bool) -> Text {
    Text::new(text)
        .with_content_size(size)
        .with_weight(weight)
        .with_italic(italic)
        .with_font_variation(OpenTypeTag(*b"wdth"), width)
}

fn label(text: &str, width: f32, size: f32) -> Element {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(width)
            .with_child(Box::new(
                Text::new(text)
                    .with_role(TextRole::Caption)
                    .with_content_size(size)
                    .dimmed(),
            )),
    )
}

fn row(name: Element, content: Element) -> Element {
    Box::new(
        Row::new()
            .no_wrap()
            .with_gap(12.0)
            .with_align_items(taffy::AlignItems::Center)
            .push(name)
            .push(content),
    )
}

fn page(column: Column) -> Element {
    Box::new(
        Container::new()
            .with_no_padding()
            .filling()
            .with_child(Box::new(column)),
    )
}

fn specimen(heading: &str) -> Element {
    let mut column = Column::new()
        .with_gap(4.0)
        .with_vertical_padding(20.0)
        .with_horizontal_padding(24.0, 24.0)
        .push(Box::new(Text::new(heading).with_role(TextRole::Subheading)));
    for (name, weight, width, italic) in STYLES {
        column = column.push(Box::new(
            Text::new(name).with_role(TextRole::Caption).dimmed(),
        ));
        for size in SIZES {
            column = column.push(row(
                label(&format!("{size:.0} px"), 40.0, 10.0),
                Box::new(styled(SPECIMEN, size, weight, width, italic)),
            ));
        }
    }
    page(column)
}

fn glyphs(heading: &str) -> Element {
    let mut column = Column::new()
        .with_gap(0.0)
        .with_vertical_padding(6.0)
        .with_horizontal_padding(8.0, 8.0)
        .push(label(heading, 180.0, 4.0));
    for (name, weight, width, italic) in STYLES {
        column = column.push(row(
            label(name, 44.0, 3.0),
            Box::new(styled(GLYPHS, 14.0, weight, width, italic)),
        ));
    }
    page(column)
}

fn arg(name: &str) -> Option<String> {
    let mut args = std::env::args().skip_while(|a| a != name);
    args.next()?;
    args.next()
}

/// Lay `left` and `right` out with the setting off and on, and draw them
/// side by side at `scale` device pixels per logical pixel.
fn sheet(
    catalog: &FontCatalog,
    dark: bool,
    viewport: (u32, u32),
    scale: u32,
    build: fn(&str) -> Element,
    headings: [&str; 2],
    out: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Each frame is drawn at the device scale, as a HiDPI window draws it:
    // text is rasterised from its outlines at that size.
    tilo_ui::offscreen::set_capture_device_scale(scale);
    let (width, height) = (
        viewport.0.saturating_mul(scale),
        viewport.1.saturating_mul(scale),
    );
    // Vello renders at most 256 bins of 256 x 256 px per target: each side
    // is its own target, joined pixel for pixel.
    if width.div_ceil(256).saturating_mul(height.div_ceil(256)) > 256 {
        return Err(format!("{width}x{height} is larger than one Vello target").into());
    }
    let mut sheet = image::RgbaImage::new(width.saturating_mul(2), height);
    for ((on, heading), left) in [false, true]
        .into_iter()
        .zip(headings)
        .zip([0, i64::from(width)])
    {
        let mut theme = if dark { Theme::dark() } else { Theme::light() };
        theme.tokens.typography.distinct_letters = on;
        theme.document.type_.distinct_letters = Some(on);
        let ground = theme.tokens.colors.surfaces.content;
        let harness = FrameHarness::new_with_controlled_inputs(
            State::new(),
            build(heading),
            viewport,
            theme,
            TextRenderer::with_font_catalog(catalog)?,
            UserId("specimen".into()),
        );
        let rgba = OffscreenRenderer::new(width, height).render_resilient(harness.scene(), ground);
        let side = image::RgbaImage::from_raw(width, height, rgba).ok_or("short frame")?;
        image::imageops::replace(&mut sheet, &side, left, 0);
    }
    sheet.save(out)?;
    println!("{}", out.display());
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = PathBuf::from(
        arg("--out").unwrap_or_else(|| "target/look/distinct-letters-specimen".into()),
    );
    std::fs::create_dir_all(&dir)?;
    let catalog = FontCatalog::controlled_fixture(tilo_ui::font_catalog::checkout_root(), "en-US");
    for (theme, dark) in [("light", false), ("dark", true)] {
        sheet(
            &catalog,
            dark,
            (960, 820),
            4,
            specimen,
            [
                "Always distinguish similar letters: off",
                "Always distinguish similar letters: on",
            ],
            &dir.join(format!("specimen-{theme}.png")),
        )?;
        sheet(
            &catalog,
            dark,
            (160, 132),
            16,
            glyphs,
            ["Old: the default forms", "New: the distinct forms"],
            &dir.join(format!("glyphs-{theme}.png")),
        )?;
    }
    Ok(())
}
