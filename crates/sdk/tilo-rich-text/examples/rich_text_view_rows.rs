//! What showing rich text read-only costs per row: many short messages, each
//! its own [`RichTextView`], laid out and drawn as a timeline would, against
//! shaping the same text as one plain layout.
//!
//! ```text
//! cargo run -p tilo-rich-text --release --example rich_text_view_rows
//! ```

use std::time::Instant;

use tilo_rich_text::RichTextView;
use ui::kurbo::{Affine, Rect};
use ui::vello::Scene;
use ui::widget::BlockContent;
use ui::widget::marks::{Mark, MarkSpan};
use ui::{TextRenderer, Theme};

const MESSAGE: &str =
    "sure, send it over and I'll take a look after lunch — the numbers looked fine yesterday";

fn row(index: usize) -> Vec<BlockContent> {
    let mut block = BlockContent::paragraph(format!("m{index}"), MESSAGE);
    block.marks = vec![MarkSpan::new(0..4, Mark::Bold)];
    vec![block]
}

#[allow(clippy::cast_precision_loss, reason = "timings never approach 2^53 µs")]
fn ms(us: u128) -> f64 {
    us as f64 / 1000.0
}

fn main() {
    ui::i18n::I18nManager::init("en-US");
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    for rows in [50usize, 200] {
        let mut views: Vec<RichTextView> = (0..rows).map(|_| RichTextView::new()).collect();
        let started = Instant::now();
        for (index, view) in views.iter_mut().enumerate() {
            view.show(&row(index));
        }
        let shown = started.elapsed().as_micros();
        let mut frames = Vec::new();
        for _ in 0..5 {
            let started = Instant::now();
            let mut y = 0.0;
            let mut scene = Scene::new();
            for view in &mut views {
                y += view.layout(Rect::new(0.0, y, 600.0, y), &theme, &mut text);
                view.draw(&mut scene, Affine::IDENTITY, &mut text, &theme);
            }
            frames.push(started.elapsed().as_micros());
        }
        let started = Instant::now();
        let mut scene = Scene::new();
        let mut y = 0.0f64;
        for index in 0..rows {
            let spans = [ui::richtext::RichSpan::new(format!("{index} {MESSAGE}"))];
            let layout = ui::richtext::RichTextLayout::shape_styled(
                &mut text.font_system,
                &spans,
                &ui::richtext::RichTextLayout::block_style(
                    false,
                    15.0,
                    ui::richtext::FontFamily::Sans,
                    1.3,
                ),
                Some(600.0),
                ui::richtext::TextAlign::Left,
                Vec::new(),
                Vec::new(),
            );
            text.draw_rich(
                &mut scene,
                &layout,
                Affine::translate((0.0, y)),
                ui::peniko::Color::WHITE,
            );
            y += f64::from(layout.height());
        }
        let plain = started.elapsed().as_micros();
        println!(
            "{rows} rows: show {:.2} ms, first frame {:.2} ms, steady frame {:.2} ms; plain shaping {:.2} ms",
            ms(shown),
            ms(frames.first().copied().unwrap_or(0)),
            ms(*frames.last().unwrap_or(&0)),
            ms(plain),
        );
    }
}
