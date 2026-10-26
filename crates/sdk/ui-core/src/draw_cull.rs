//! Drawing only what a frame repaints.
//!
//! A host that presents each frame over the pixels of the one before, makes
//! its scroll copies and repaints only its damage from the scene, never
//! shows what the scene holds outside that damage. Such a host draws the
//! widgets inside a [`Frame`], bound to the scene they draw into. A scroller
//! then says where the frame keeps the pixels of its content ([`keeping`]):
//! where a copy moves what it showed, or, when it did not move, where it
//! is. There, a widget with nothing new to paint whose paint lies wholly
//! inside is not drawn at all ([`kept`]), and a widget that paints a long
//! run of rows itself (a code editor) paints only those outside it
//! ([`kept_band`]). A scroll then encodes the strip it exposed
//! and what changed, not every row in view.
//!
//! The frame's damage is planned only once its widgets are drawn (a draw
//! may move what they damage), so what is left undrawn rests on a guess:
//! where the copies land, less what the last frame that copied repainted
//! besides ([`Frame::expect`]), which is where overlays, a scroll bar and
//! the edges of what surrounds a scroller repaint again. The host checks
//! the guess once the damage is planned ([`Frame::holds`]): every place left
//! undrawn must lie outside the damage it repaints from this scene, or it
//! draws the widgets again in full. The damage is planned exactly as for a
//! full draw, so it is never less than one would report.
//!
//! Only the scene the frame is bound to is ever left short. A widget's own
//! cache, a row's scene, a capture and any host that splits scenes into
//! damage parts ([`crate::scene_parts`]) are always drawn whole.

use std::cell::RefCell;

use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};

/// How far past the bounds a widget gives its paint may reach in scene
/// pixels once rasterized (antialiasing), and how far a damage rectangle's
/// repaint reaches past it (whole pixels).
const RASTER_REACH: f64 = 1.0;

/// How far from a whole scene pixel a scroll's step may be and still copy.
const WHOLE_PIXEL: f64 = 1e-3;

thread_local! {
    static FRAME: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// The frame being drawn, while a host culls it.
struct State {
    /// The scene the frame's widgets draw into, by address only, while
    /// they draw ([`bind`]): nothing drawn into another scene is left out.
    scene: Option<*const Scene>,
    /// Where the last frame that copied repainted, in scene pixels: kept
    /// pixels are not expected there.
    expected: Vec<Rect>,
    /// The scrollers being drawn, innermost last: where each keeps its
    /// content's pixels, or `None` where nothing is kept.
    keeps: Vec<Option<Keep>>,
    /// What was left undrawn, in scene pixels, reach included.
    skipped: Vec<Rect>,
}

/// Where a frame keeps the pixels of a scroller's content, in scene pixels.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Keep {
    /// Where the content shows: what it paints anywhere else is clipped
    /// away.
    clip: Rect,
    /// Where the pixels stay, or land after the copy.
    region: Rect,
    /// What paints over the content there without moving with it, and so
    /// repaints.
    overlays: Vec<Rect>,
    /// Whether the content moved since the frame on screen.
    moving: bool,
}

impl Keep {
    /// The content shown in `clip` (logical pixels, drawn with
    /// `transform`) and moved by `delta` (logical pixels, on screen) since
    /// the frame on screen, with `overlays` (logical) painted over it
    /// without moving. `None` when the step is not a whole number of scene
    /// pixels, which no copy makes.
    pub(crate) fn scrolled(
        transform: Affine,
        clip: Rect,
        delta: Vec2,
        overlays: &[Rect],
    ) -> Option<Self> {
        let shown = transform.transform_rect_bbox(clip);
        let step = transform * delta.to_point() - transform * vello::kurbo::Point::ZERO;
        let whole = |value: f64| (value - value.round()).abs() <= WHOLE_PIXEL;
        if !shown.is_finite() || !whole(step.x) || !whole(step.y) {
            return None;
        }
        let step = Vec2::new(step.x.round(), step.y.round());
        let inside = Rect::new(
            shown.x0.ceil(),
            shown.y0.ceil(),
            shown.x1.floor(),
            shown.y1.floor(),
        );
        let region = inside.intersect(inside + step);
        if !(region.width() > 0.0 && region.height() > 0.0) {
            return None;
        }
        Some(Self {
            clip: shown,
            region,
            overlays: overlays
                .iter()
                .map(|rect| transform.transform_rect_bbox(*rect))
                .collect(),
            moving: step != Vec2::ZERO,
        })
    }

    /// Whether the content moved since the frame on screen.
    pub(crate) fn moving(&self) -> bool {
        self.moving
    }

    /// The kept pixels of a scroller drawn inside `outer`'s content: both
    /// must keep them, and two scrolls at once keep nothing (the planner
    /// copies one viewport, not one inside another).
    fn within(self, outer: &Self) -> Option<Self> {
        if self.moving && outer.moving {
            return None;
        }
        let region = self.region.intersect(outer.region);
        if !(region.width() > 0.0 && region.height() > 0.0) {
            return None;
        }
        let mut overlays = self.overlays;
        overlays.extend_from_slice(&outer.overlays);
        Some(Self {
            clip: self.clip.intersect(outer.clip),
            region,
            overlays,
            moving: self.moving || outer.moving,
        })
    }

    /// Whether `bounds` (scene pixels, reach included) lie wholly where the
    /// pixels are kept and meet nothing that repaints.
    fn holds(&self, bounds: Rect, expected: &[Rect]) -> bool {
        let meets = |rect: &Rect| {
            let common = rect.intersect(bounds);
            common.width() > 0.0 && common.height() > 0.0
        };
        bounds.is_finite()
            && self.region.contains_rect(bounds)
            && !self.overlays.iter().any(meets)
            && !expected.iter().any(meets)
    }
}

/// What a host that culls its frames keeps from one to the next.
#[derive(Default)]
pub(crate) struct Culling {
    /// The scenes a culled frame draws into.
    pub(crate) scenes: Scenes,
    /// Where the last frame that copied repainted, in scene pixels.
    expected: Vec<Rect>,
    /// Whether the last frame showed a secret: what a frame leaves out
    /// still shows it.
    secret: bool,
    /// How many frames left anything out, and how many of those had their
    /// widgets drawn again in full.
    #[cfg(test)]
    pub(crate) tally: (usize, usize),
}

/// The scenes of a culled frame: the widgets', and what is painted over
/// them, apart, so the widgets can be drawn again in full before the two
/// are composed.
#[derive(Default)]
pub(crate) struct Scenes {
    pub(crate) widgets: Scene,
    pub(crate) layers: Scene,
}

impl Culling {
    /// Begin culling a frame, with empty scenes to draw it into.
    pub(crate) fn begin(&mut self) -> Frame {
        self.scenes.widgets.reset();
        self.scenes.layers.reset();
        Frame::begin(&self.expected)
    }

    /// The frame just drawn repainted `damage` (scene pixels), and copied
    /// if `copied`: the next culled frame expects a copy's frame to repaint
    /// there again.
    pub(crate) fn repainted(&mut self, damage: &[Rect], copied: bool) {
        if copied {
            self.expected.clear();
            self.expected.extend_from_slice(damage);
        }
    }

    /// A frame that left something out (`left_out`) and whether its
    /// widgets were drawn again in full, for tests to count.
    #[cfg_attr(
        not(test),
        expect(clippy::needless_pass_by_ref_mut, reason = "only tests keep the count")
    )]
    pub(crate) fn tally(&mut self, left_out: bool, redrawn: bool) {
        #[cfg(test)]
        {
            self.tally.0 += usize::from(left_out);
            self.tally.1 += usize::from(redrawn);
        }
        let _ = (left_out, redrawn);
    }

    /// Whether the frame showed a secret, given that its draw saw one
    /// (`drawn`) and whether it left anything out.
    pub(crate) fn shows_secret(&mut self, drawn: bool, left_out: bool) -> bool {
        self.secret = drawn || (left_out && self.secret);
        self.secret
    }
}

/// Culling one frame's draw; ends when dropped.
pub(crate) struct Frame {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Frame {
    /// Leave out what this frame keeps of the scene its widgets draw into
    /// ([`bind`]), until the frame is dropped. `expected` is where the last
    /// frame that copied repainted (scene pixels).
    fn begin(expected: &[Rect]) -> Self {
        let state = State {
            scene: None,
            expected: expected.to_vec(),
            keeps: Vec::new(),
            skipped: Vec::new(),
        };
        FRAME.with_borrow_mut(|frame| *frame = Some(state));
        Self {
            _not_send: std::marker::PhantomData,
        }
    }

    /// Whether the draw left anything out.
    pub(crate) fn skipped_any(&self) -> bool {
        FRAME.with_borrow(|frame| {
            frame
                .as_ref()
                .is_some_and(|state| !state.skipped.is_empty())
        })
    }

    /// Whether this frame, repainting `damage` (scene pixels) from the
    /// scene over its last pixels moved by its copies, shows only what was
    /// drawn: nothing left out meets the damage. A frame repainted whole
    /// holds only when nothing was left out.
    pub(crate) fn holds(&self, damage: Option<&[Rect]>) -> bool {
        FRAME.with_borrow(|frame| {
            let Some(state) = frame.as_ref() else {
                return true;
            };
            let Some(damage) = damage else {
                return state.skipped.is_empty();
            };
            !state.skipped.iter().any(|skipped| {
                damage.iter().any(|rect| {
                    let repainted = rect.inflate(RASTER_REACH, RASTER_REACH);
                    let common = repainted.intersect(*skipped);
                    common.width() > 0.0 && common.height() > 0.0
                })
            })
        })
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        FRAME.with_borrow_mut(|frame| *frame = None);
    }
}

/// The frame being culled draws its widgets into `scene` from now on.
pub(crate) fn bind(scene: &Scene) {
    FRAME.with_borrow_mut(|frame| {
        if let Some(state) = frame.as_mut() {
            state.scene = Some(std::ptr::from_ref(scene));
        }
    });
}

/// The frame being culled is done drawing its widgets.
pub(crate) fn unbind() {
    FRAME.with_borrow_mut(|frame| {
        if let Some(state) = frame.as_mut() {
            state.scene = None;
        }
    });
}

/// Whether a frame being culled draws its widgets into `scene`: a cache
/// recorded around them then would hold what the frame leaves out.
pub(crate) fn culls_into(scene: &Scene) -> bool {
    FRAME.with_borrow(|frame| frame.as_ref().is_some_and(|state| bound_to(state, scene)))
}

/// Whether `scene` is the one a frame being culled draws its widgets into.
fn bound_to(state: &State, scene: &Scene) -> bool {
    state.scene.is_some_and(|bound| std::ptr::eq(bound, scene))
}

/// Draw a scroller's content (`draw`) into `scene` with the frame keeping
/// its pixels where `keep` says (`None`: nowhere, its content repaints).
/// Outside a culled frame, or into another scene, this only draws.
pub(crate) fn keeping<T>(
    scene: &mut Scene,
    keep: Option<Keep>,
    draw: impl FnOnce(&mut Scene) -> T,
) -> T {
    let _scope = keep_scope(scene, keep);
    draw(scene)
}

/// [`keeping`] for a scroller that draws its content itself: what it draws
/// into `scene` while the returned scope lives.
#[must_use]
pub(crate) fn keep_scope(scene: &Scene, keep: Option<Keep>) -> Option<KeepScope> {
    FRAME.with_borrow_mut(|frame| {
        let state = frame.as_mut().filter(|state| bound_to(state, scene))?;
        let keep = match state.keeps.last() {
            None => keep,
            Some(None) => None,
            Some(Some(outer)) => keep.and_then(|keep| keep.within(outer)),
        };
        state.keeps.push(keep);
        Some(KeepScope)
    })
}

/// A scroller's [`keep_scope`], ended when dropped, even by an unwinding
/// draw.
pub(crate) struct KeepScope;

impl Drop for KeepScope {
    fn drop(&mut self) {
        FRAME.with_borrow_mut(|frame| {
            if let Some(state) = frame.as_mut() {
                state.keeps.pop();
            }
        });
    }
}

/// Whether a widget painting within `bounds` (logical pixels, drawn with
/// `transform` into `scene`) is kept on screen by the frame being culled:
/// if so, it is noted as left out and must not be drawn. Only a widget with
/// nothing new to paint may ask.
pub(crate) fn kept(scene: &Scene, transform: Affine, bounds: Rect) -> bool {
    FRAME.with_borrow_mut(|frame| {
        let Some(state) = frame.as_mut().filter(|state| bound_to(state, scene)) else {
            return false;
        };
        let Some(Some(keep)) = state.keeps.last() else {
            return false;
        };
        // What the widget paints past the scroller's clip is clipped away.
        let reach = transform
            .transform_rect_bbox(bounds)
            .inflate(RASTER_REACH, RASTER_REACH)
            .intersect(keep.clip);
        if !keep.holds(reach, &state.expected) {
            return false;
        }
        state.skipped.push(reach);
        true
    })
}

/// Where the frame being culled keeps the pixels of what `scene` is being
/// drawn with `transform` inside `visible` (logical pixels), as the logical
/// rectangle a widget that paints its own rows may leave them out in,
/// asking for each run it leaves out ([`kept`]). `None` outside a culled
/// frame or where nothing is kept.
pub(crate) fn kept_band(scene: &Scene, transform: Affine, visible: Rect) -> Option<Rect> {
    FRAME.with_borrow(|frame| {
        let state = frame.as_ref().filter(|state| bound_to(state, scene))?;
        let keep = state.keeps.last()?.as_ref()?;
        let inverse = transform.inverse();
        let region = keep.region.inset(-RASTER_REACH);
        let band = inverse.transform_rect_bbox(region).intersect(visible);
        (band.width() > 0.0 && band.height() > 0.0).then_some(band)
    })
}

#[cfg(test)]
#[path = "draw_cull_tests.rs"]
mod tests;
