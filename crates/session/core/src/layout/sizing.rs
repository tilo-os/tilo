//! How big a place stands: what a click or an agent's step names no size for takes the room it
//! finds, and hugs what it shows up to it (canvas.md §1, §7).

use super::turns::CanvasRect;
use super::{Layout, DEFAULT_HEIGHT, DEFAULT_WIDTH, clamp_coord, clamp_side};

/// How wide a thing standing at canvas x `x` with no rectangle of its own stands as far as the
/// view goes: `want`, or the room from `x` to the trailing edge of what the person sees (`seen`)
/// when that is narrower and still at least a window's least width. Narrower room than that
/// leaves it `want` wide, and the canvas pans to show it.
pub(super) fn width_in_view(x: i64, want: i64, seen: Option<CanvasRect>) -> i64 {
    let Some(seen) = seen else {
        return want;
    };
    let room = seen
        .x
        .saturating_add(seen.width)
        .saturating_sub(x)
        .saturating_sub(i64::from(tilo_desktop_model::CANVAS_GAP));
    if room >= i64::from(tilo_desktop_model::WINDOW_MIN_WIDTH) {
        want.min(room)
    } else {
        want
    }
}

impl Layout {
    /// The size a thing found by text stands at, and whether it hugs what it shows: the rectangle
    /// the person dragged (`size`), kept exactly; or, at a click, as tall as what it shows up to
    /// a window's height (`place_at`), and its own width as far as the free room to its right
    /// allows — up to the next place, and the edge of what the person sees — so it stands at the
    /// point, in view (canvas.md §1, §7 "Everything stays in view").
    pub(super) fn found_size(
        &self,
        at: (i64, i64),
        size: Option<(i64, i64)>,
        seen: Option<CanvasRect>,
    ) -> (i64, i64, bool) {
        match size {
            Some((width, height)) => (width, height, false),
            None => (
                self.width_at_click(at, DEFAULT_WIDTH, DEFAULT_HEIGHT, seen),
                DEFAULT_HEIGHT,
                true,
            ),
        }
    }

    /// How wide a thing put at a click at `at`, at most `height` tall, stands (canvas.md §1, a
    /// click places what is found at the click): its own width `want`, narrowed to the room to
    /// the edge of what the person sees ([`width_in_view`]) and to the free room before the next
    /// place to its right (`free_width_at`), down to a window's least width, so it stands at the
    /// click. Where not even that is free, its width in view, and the place action stands it in
    /// the free spot nearest the click (`free_place_near`).
    pub(super) fn width_at_click(
        &self,
        at: (i64, i64),
        want: i64,
        height: i64,
        seen: Option<CanvasRect>,
    ) -> i64 {
        let in_view = width_in_view(at.0, want, seen);
        self.handle
            .read(|tree| {
                tree.active_workspace()?.layout.free_width_at(
                    (clamp_coord(at.0), clamp_coord(at.1)),
                    clamp_side(in_view),
                    clamp_side(height),
                )
            })
            .map_or(in_view, i64::from)
    }
}

/// The size a place asked for at `width` by `height` takes on the canvas, and whether it hugs what
/// it shows. A place asked to hug (`hugs`: one put with no size of its own, at a click or beside an
/// agent's turn) takes `height` as the most it takes, and one asked with no height a window's;
/// what it shows hugs what it holds up to it (`CanvasPlace::hugs`, canvas.md §7): the free space
/// it is given is that most.
pub(super) fn canvas_size(width: i64, height: i64, hugs: bool) -> (u32, u32, bool) {
    let hugs = hugs || height <= 0;
    let height = if height <= 0 { DEFAULT_HEIGHT } else { height };
    (clamp_side(width), clamp_side(height), hugs)
}
