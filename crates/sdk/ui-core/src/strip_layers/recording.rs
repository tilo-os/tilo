//! Drawing an app's frame as layers: a pannable view drawn while a
//! [`LayerRecording`] is open draws its content on its own, in canvas
//! coordinates ([`draw_strip`]), so a presenter keeping the layers pans it
//! by copying ([`super::LayerPresenter`]) instead of rasterising it again.
//!
//! What was drawn before the view is the backdrop, what is drawn after it
//! (its scroll bars, the window's chrome, popups) the chrome. A view is cut
//! out only where that split keeps the frame's picture: no clip or layer is
//! open around it, it is shown on whole device pixels and panned by whole
//! device pixels. Anywhere else it draws as it would without a recording.

use std::cell::RefCell;

use vello::kurbo::{Affine, Rect, Vec2};
use vello::{FineDamageRect, Scene};

use super::{ChromeDamage, FrameCache, FrameLayers, StripBlit, compose};

/// The frame being drawn as layers.
struct Drawing {
    /// What the frame drew before its strip.
    backdrop: Scene,
    strip: Option<(StripBlit, Scene)>,
}

thread_local! {
    static DRAWING: RefCell<Option<Drawing>> = const { RefCell::new(None) };
    /// Where the last view that can be a strip showed, and how far it was
    /// panned; and whether a view panned since [`take_panned`].
    static LAST_VIEW: std::cell::Cell<(Rect, Vec2, bool)> =
        const { std::cell::Cell::new((Rect::ZERO, Vec2::ZERO, false)) };
}

/// Whether a view that can be drawn as a strip ([`draw_strip`]) panned in a
/// frame drawn since the last call: a host draws the next frame as layers.
pub fn take_panned() -> bool {
    LAST_VIEW.with(|last| {
        let (viewport, scroll, panned) = last.get();
        last.set((viewport, scroll, false));
        panned
    })
}

/// How far a value may be from a whole device pixel and still be one.
const WHOLE: f64 = 1e-3;

fn whole(value: f64) -> Option<u32> {
    let rounded = value.round();
    ((value - rounded).abs() <= WHOLE && rounded >= 0.0 && rounded <= f64::from(u32::MAX))
        .then(|| crate::num::u32_of_f64(rounded))
}

/// One frame drawn as layers, on this thread, from [`Self::begin`] until
/// [`Self::finish`] (or the guard's drop, which discards it).
pub struct LayerRecording {
    previous: Option<Drawing>,
}

impl LayerRecording {
    #[must_use]
    pub fn begin() -> Self {
        let previous = DRAWING.with_borrow_mut(|drawing| {
            drawing.replace(Drawing {
                backdrop: Scene::new(),
                strip: None,
            })
        });
        Self { previous }
    }

    /// The frame `scene` ends: the layers it was drawn as, its `generation`
    /// telling them apart from others, on an output `width` pixels wide;
    /// `chrome_damage` says where the chrome changed since an earlier
    /// drawing when that is known. `scene` is then the frame's whole
    /// picture again ([`compose`]), for a host that does not present layers.
    /// `None`, `scene` untouched, when no view was drawn as a strip.
    pub fn finish(
        mut self,
        scene: &mut Scene,
        width: u32,
        generation: u64,
        chrome_damage: Option<ChromeDamage>,
    ) -> Option<FrameCache> {
        let drawing =
            DRAWING.with_borrow_mut(|drawing| std::mem::replace(drawing, self.previous.take()))?;
        let (blit, strip) = drawing.strip?;
        let cache = FrameCache {
            width,
            strips: vec![blit],
            layers: FrameLayers {
                backdrop: drawing.backdrop,
                workspace: Scene::new(),
                strips: vec![strip],
                strip_parts: vec![None],
                chrome: std::mem::take(scene),
            },
            generation,
            chrome_damage,
        };
        *scene = compose(&cache);
        Some(cache)
    }
}

impl Drop for LayerRecording {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            DRAWING.with_borrow_mut(|drawing| *drawing = Some(previous));
        }
    }
}

/// Whether a [`LayerRecording`] is open and has no strip yet.
#[must_use]
pub fn recording() -> bool {
    DRAWING.with_borrow(|drawing| drawing.as_ref().is_some_and(|d| d.strip.is_none()))
}

/// Draw a pannable view's content as the frame's strip, when a
/// [`LayerRecording`] is open: `viewport` (where the content shows) and
/// `scroll` (how far it is panned, `transform` mapping both to device
/// pixels) are whole device pixels, nothing is open around the view in
/// `scene`, and no other view is the frame's strip. `draw` then draws the
/// content where it would show with no pan, `scroll` right of and below
/// where it shows: the strip's canvas. Returns `false`, having drawn
/// nothing, otherwise: the caller draws its content as usual.
pub fn draw_strip(
    scene: &mut Scene,
    transform: Affine,
    viewport: Rect,
    scroll: Vec2,
    draw: impl FnOnce(&mut Scene),
) -> bool {
    LAST_VIEW.with(|last| {
        let (shown, panned, earlier) = last.get();
        last.set((
            viewport,
            scroll,
            earlier || (shown == viewport && panned != scroll),
        ));
    });
    if !recording() || scene.encoding().n_open_clips != 0 {
        return false;
    }
    let [a, b, c, d, _, _] = transform.as_coeffs();
    if b != 0.0 || c != 0.0 || a <= 0.0 || d <= 0.0 {
        return false;
    }
    let device = transform.transform_rect_bbox(viewport);
    let (Some(x0), Some(y0), Some(x1), Some(y1), Some(scroll_x), Some(scroll_y)) = (
        whole(device.x0),
        whole(device.y0),
        whole(device.x1),
        whole(device.y1),
        whole(scroll.x * a),
        whole(scroll.y * d),
    ) else {
        return false;
    };
    if x1 <= x0 || y1 <= y0 {
        return false;
    }
    let blit = StripBlit {
        dest_x: x0,
        dest_y: y0,
        dest_w: x1 - x0,
        dest_h: y1 - y0,
        scroll_x,
        scroll_y,
    };
    let mut canvas = Scene::new();
    draw(&mut canvas);
    let below = std::mem::take(scene);
    DRAWING.with_borrow_mut(|drawing| {
        if let Some(drawing) = drawing.as_mut() {
            drawing.backdrop = below;
            drawing.strip = Some((blit, canvas));
        }
    });
    true
}

/// `damage` (device pixels) on an output of `size` in whole 16-pixel
/// tiles, as [`ChromeDamage::rects`] names where a chrome changed.
#[must_use]
pub fn tiles(damage: &[Rect], (width, height): (u32, u32)) -> Vec<FineDamageRect> {
    let edge = f64::from(TILE);
    let at = |value: f64, round: fn(f64) -> f64, high: u32| {
        crate::num::u32_of_f64((round(value / edge) * edge).clamp(0.0, f64::from(high)))
    };
    damage
        .iter()
        .filter_map(|rect| {
            let (x0, y0) = (
                at(rect.x0, f64::floor, width),
                at(rect.y0, f64::floor, height),
            );
            let (x1, y1) = (
                at(rect.x1, f64::ceil, width),
                at(rect.y1, f64::ceil, height),
            );
            (x1 > x0 && y1 > y0).then(|| FineDamageRect {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            })
        })
        .collect()
}

/// Vello's tile edge in device pixels.
const TILE: u32 = 16;

/// Put what `cache` draws under its chrome below every surface of
/// `backdrop`: a floating surface drawn in the chrome recorded only the
/// chrome drawn before it as what is behind it.
pub fn put_under(cache: &FrameCache, backdrop: &mut crate::backdrop::BackdropFrame) {
    if backdrop.is_empty() {
        return;
    }
    let mut under = cache.layers.backdrop.clone();
    super::append_panned(&mut under, &cache.layers, &cache.strips);
    for region in &mut backdrop.regions {
        let mut below = under.clone();
        below.append(&region.below, None);
        region.below = below;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::peniko::{Color, Fill};

    fn fill(scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
        scene.fill(Fill::NonZero, transform, color, None, &rect);
    }

    #[test]
    fn a_view_drawn_in_a_recording_is_cut_out_as_the_strip() {
        let recording = LayerRecording::begin();
        let mut scene = Scene::new();
        let transform = Affine::scale(2.0);
        fill(
            &mut scene,
            transform,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Color::WHITE,
        );
        let viewport = Rect::new(10.0, 10.0, 60.0, 60.0);
        let drew = draw_strip(
            &mut scene,
            transform,
            viewport,
            Vec2::new(5.0, 7.5),
            |canvas| {
                fill(
                    canvas,
                    transform,
                    Rect::new(0.0, 0.0, 400.0, 400.0),
                    Color::BLACK,
                );
            },
        );
        assert!(drew);
        fill(
            &mut scene,
            transform,
            Rect::new(0.0, 0.0, 10.0, 10.0),
            Color::WHITE,
        );
        let cache = recording
            .finish(&mut scene, 200, 1, None)
            .expect("the view is the strip");
        assert_eq!(
            cache.strips,
            vec![StripBlit {
                dest_x: 20,
                dest_y: 20,
                dest_w: 100,
                dest_h: 100,
                scroll_x: 10,
                scroll_y: 15,
            }]
        );
        assert_eq!(cache.layers.backdrop.encoding().n_paths, 1);
        assert_eq!(cache.layers.chrome.encoding().n_paths, 1);
        assert!(scene.encoding_eq(&compose(&cache)));
    }

    #[test]
    fn a_view_inside_a_clip_or_between_pixels_draws_as_usual() {
        let recording = LayerRecording::begin();
        let mut scene = Scene::new();
        let viewport = Rect::new(0.0, 0.0, 50.0, 50.0);
        scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &viewport);
        assert!(!draw_strip(
            &mut scene,
            Affine::IDENTITY,
            viewport,
            Vec2::ZERO,
            |_| {}
        ));
        scene.pop_layer();
        assert!(!draw_strip(
            &mut scene,
            Affine::scale(1.5),
            viewport,
            Vec2::new(0.5, 0.0),
            |_| {}
        ));
        assert!(recording.finish(&mut scene, 50, 1, None).is_none());
        assert!(!recording_open(), "the recording is closed");
    }

    #[test]
    fn a_view_shown_again_panned_says_so_once() {
        let _ = take_panned();
        let viewport = Rect::new(0.0, 0.0, 50.0, 50.0);
        let mut scene = Scene::new();
        let mut show = |scroll| {
            assert!(!draw_strip(
                &mut scene,
                Affine::IDENTITY,
                viewport,
                scroll,
                |_| {}
            ));
        };
        show(Vec2::ZERO);
        show(Vec2::ZERO);
        assert!(!take_panned(), "a view at rest did not pan");
        show(Vec2::new(0.0, 10.0));
        assert!(take_panned());
        assert!(!take_panned(), "taken once");
    }

    fn recording_open() -> bool {
        DRAWING.with_borrow(Option::is_some)
    }
}
