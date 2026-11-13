//! Every Settings page as the window paints it: the frame is rendered and read
//! back pixel by pixel, in both looks and at both device scales. A block of
//! pure black where a page's own surface should be is a region no paint reached
//! — the frame left it at the buffer's zero value instead of the page's colour.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use settings::categories::Category;
use ui::Theme;

const VIEWPORTS: [(f32, f32); 2] = [(1200.0, 760.0), (640.0, 400.0)];

const SCALES: [u32; 2] = [1, 2];

/// The largest run of pure black a page paints on purpose, in logical
/// pixels squared: the accessibility previews print black sample text on
/// light paper, 151 px at 1x. A page's ground and its ink are the theme's
/// colours, never black, so anything past this is a region nothing painted.
const PAINTED_BLACK: usize = 200;

fn largest_black_block(rgba: &[u8], width: usize) -> (usize, usize, usize) {
    let height = rgba.len() / 4 / width.max(1);
    let black = |x: usize, y: usize| {
        let at = (y * width + x) * 4;
        rgba.get(at..at + 4) == Some(&[0, 0, 0, 255][..])
    };
    let mut seen = vec![false; width * height];
    let mut largest = (0, 0, 0);
    let mut stack = Vec::new();
    for y in 0..height {
        for x in 0..width {
            if seen[y * width + x] || !black(x, y) {
                continue;
            }
            stack.push((x, y));
            seen[y * width + x] = true;
            let (mut run, origin) = (0, (x, y));
            while let Some((x, y)) = stack.pop() {
                run += 1;
                let neighbours = [
                    (x.wrapping_sub(1), y),
                    (x + 1, y),
                    (x, y.wrapping_sub(1)),
                    (x, y + 1),
                ];
                for (nx, ny) in neighbours {
                    if nx < width && ny < height && !seen[ny * width + nx] && black(nx, ny) {
                        seen[ny * width + nx] = true;
                        stack.push((nx, ny));
                    }
                }
            }
            if run > largest.0 {
                largest = (run, origin.0, origin.1);
            }
        }
    }
    largest
}

#[test]
fn no_page_shows_a_block_nothing_painted() {
    let mut worst = (0, String::from("no page"));
    for &(width, height) in &VIEWPORTS {
        for scale in SCALES {
            ui::offscreen::set_capture_device_scale(scale);
            for &category in Category::ALL {
                for (look, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
                    let mut app = crate::support::window_themed(category, (width, height), theme);
                    let rgba = app.harness().render_rgba();
                    let pixels = (width as usize) * (scale as usize);
                    let (run, x, y) = largest_black_block(&rgba, pixels);
                    let what = format!(
                        "{category:?} at {width}x{height} {scale}x {look}: {run} px from ({x}, {y})"
                    );
                    if run > worst.0 {
                        worst = (run, what.clone());
                    }
                    assert!(
                        run < PAINTED_BLACK * (scale as usize) * (scale as usize),
                        "{what} is black where the page paints its own surface"
                    );
                }
            }
        }
    }
    ui::offscreen::set_capture_device_scale(1);
    println!("largest black run: {} px ({})", worst.0, worst.1);
}
