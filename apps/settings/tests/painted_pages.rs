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

/// Pixels of a row looked at together before any is looked at alone.
const SPAN: usize = 64;

/// The largest run of pure black pixels joined edge to edge, and the first
/// pixel of it a scan from the top left meets: `(pixels, x, y)`.
///
/// A black pixel is three zero bytes and a full alpha, so a span of a row
/// without a zero byte holds none and is passed over in one search of its
/// bytes; nearly every span of a painted page is one. The pixels of the
/// other spans are visited as before, in the same order.
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
        let row = &rgba[y * width * 4..(y + 1) * width * 4];
        for (span, bytes) in row.chunks(SPAN * 4).enumerate() {
            if !bytes.contains(&0) {
                continue;
            }
            for x in span * SPAN..span * SPAN + bytes.len() / 4 {
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
    }
    largest
}

const BLACK: [u8; 4] = [0, 0, 0, 255];
/// A ground no channel of which is zero: its spans are passed over whole.
const PAPER: [u8; 4] = [255, 255, 255, 255];
/// A ground with a zero channel that is not black: its pixels are each looked at.
const INK: [u8; 4] = [0, 10, 20, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

struct Frame {
    name: &'static str,
    size: (usize, usize),
    ground: [u8; 4],
    black: &'static [(usize, usize)],
    /// Black pixels after the last whole row: a row cut short is no row.
    spare: usize,
    largest: (usize, usize, usize),
}

const fn frame(
    name: &'static str,
    size: (usize, usize),
    ground: [u8; 4],
    black: &'static [(usize, usize)],
    largest: (usize, usize, usize),
) -> Frame {
    Frame {
        name,
        size,
        ground,
        black,
        spare: 0,
        largest,
    }
}

const FRAMES: &[Frame] = &[
    frame("no pixels", (0, 0), PAPER, &[], (0, 0, 0)),
    frame("no rows", (4, 0), PAPER, &[], (0, 0, 0)),
    frame("paper alone", (8, 4), PAPER, &[], (0, 0, 0)),
    frame("ink alone", (8, 4), INK, &[], (0, 0, 0)),
    frame(
        "clear black is not painted black",
        (8, 4),
        CLEAR,
        &[],
        (0, 0, 0),
    ),
    frame("one pixel", (8, 4), PAPER, &[(5, 2)], (1, 5, 2)),
    frame("one pixel on ink", (8, 4), INK, &[(5, 2)], (1, 5, 2)),
    frame(
        "a run across two spans",
        (130, 3),
        PAPER,
        &[(62, 1), (63, 1), (64, 1), (65, 1), (66, 1)],
        (5, 62, 1),
    ),
    frame(
        "a run into the last, short span",
        (130, 2),
        PAPER,
        &[(126, 0), (127, 0), (128, 0), (129, 0)],
        (4, 126, 0),
    ),
    frame(
        "a column at the right edge",
        (70, 5),
        INK,
        &[(69, 0), (69, 1), (69, 2), (69, 3), (69, 4)],
        (5, 69, 0),
    ),
    frame(
        "the larger of two",
        (8, 4),
        PAPER,
        &[(0, 0), (3, 1), (4, 1), (3, 2)],
        (3, 3, 1),
    ),
    frame(
        "the first of two alike",
        (8, 4),
        PAPER,
        &[(6, 0), (1, 2)],
        (1, 6, 0),
    ),
    frame(
        "corners do not join",
        (8, 4),
        PAPER,
        &[(1, 1), (2, 2)],
        (1, 1, 1),
    ),
    frame(
        "an L",
        (8, 4),
        PAPER,
        &[(5, 0), (5, 1), (4, 1), (3, 1)],
        (4, 5, 0),
    ),
    frame(
        "a U is counted once",
        (8, 4),
        PAPER,
        &[(0, 0), (0, 1), (1, 1), (2, 1), (2, 0)],
        (5, 0, 0),
    ),
    frame("all black", (3, 3), BLACK, &[], (9, 0, 0)),
    Frame {
        spare: 2,
        ..frame("a row cut short is no row", (4, 2), PAPER, &[], (0, 0, 0))
    },
];

/// The oracle the page test reads its frames with, on frames small enough
/// to count by hand: the empty frame, the single pixel, runs that cross and
/// end a span, two runs, and the shapes a flood can count twice or miss.
#[test]
fn the_largest_black_block_is_found_whole_and_once() {
    for case in FRAMES {
        let (width, height) = case.size;
        let mut rgba = case.ground.repeat(width * height);
        for &(x, y) in case.black {
            rgba[(y * width + x) * 4..][..4].copy_from_slice(&BLACK);
        }
        rgba.extend(BLACK.repeat(case.spare));
        assert_eq!(
            largest_black_block(&rgba, width),
            case.largest,
            "{}",
            case.name
        );
    }
}

#[test]
fn no_page_shows_a_block_nothing_painted() {
    let mut worst = (0, String::from("no page"));
    for &(width, height) in &VIEWPORTS {
        for scale in SCALES {
            ui::offscreen::set_capture_device_scale(scale);
            // `render_rgba` makes a renderer, and Vello's shaders with it, for
            // each frame it draws. The frames of one size are drawn by one
            // renderer instead, made as `render_rgba` makes its own, over the
            // same ground.
            let mut renderer = ui::offscreen::OffscreenRenderer::new(
                (width as u32) * scale,
                (height as u32) * scale,
            );
            for &category in Category::ALL {
                for (look, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
                    let mut app = crate::support::window_themed(category, (width, height), theme);
                    let harness = app.harness();
                    let ground = harness.theme().tokens.colors.surfaces.content;
                    let rgba = harness.render_rgba_with(&mut renderer, ground);
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
                    // The last page of a size is drawn once more, in each
                    // look, by a renderer of its own: every page before it
                    // went through the kept one, the dark frame after a light
                    // one and the light frame after a dark one, and none of
                    // them may show in what it draws now.
                    if Category::ALL.last() == Some(&category) {
                        assert!(
                            harness.render_rgba() == rgba,
                            "{what}: the renderer kept across pages draws what a new one draws"
                        );
                    }
                }
            }
        }
    }
    ui::offscreen::set_capture_device_scale(1);
    println!("largest black run: {} px ({})", worst.0, worst.1);
}
