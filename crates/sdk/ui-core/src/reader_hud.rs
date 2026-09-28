//! Shared reading-surface chrome for the document readers (ebook, `pdf_viewer`),
//! so both present the *same* selection affordances with the same spacing,
//! hover, and selected-state treatment:
//!
//! - [`SelectionHud`] — the floating toolbar shown while a passage is
//!   selected. Presentation + hit-testing only: each app owns the selection
//!   model and maps the emitted [`HudAction`] to its own command.
//! - [`MarkerDrag`] — the grab/track/threshold state a reader embeds to let
//!   an annotation marker be *dragged*, agnostic of the coordinate space
//!   (page points vs. screen-y).
//!
//! Nothing here reaches into a document — it draws from a resolved
//! [`ReaderHudStyle`] the caller lifts from its `Theme` + reading palette,
//! and reports hits back.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use crate::paint::{fill_round, stroke_round};
use crate::text_render::{TextRenderer, TextWeight};

/// The swatch index in [`HudAction::Highlight`] indexes into the `swatches`
/// slice passed to [`SelectionHud::draw`]; the caller maps it back to its own colour model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HudAction {
    Copy,
    Highlight(usize),
    Note,
    /// Removes the mark the selection covers.
    Delete,
}

/// One highlight colour offered in the HUD's swatch row.
#[derive(Clone, Copy, Debug)]
pub struct HudSwatch {
    pub rgb: [u8; 3],
    /// The reader's currently-armed colour; draws a ring.
    pub selected: bool,
}

impl HudSwatch {
    pub fn new(rgb: [u8; 3], selected: bool) -> Self {
        Self { rgb, selected }
    }
}

/// Which controls a [`SelectionHud`] offers besides the always-present Copy:
/// the highlight swatch row, and the Note and Delete controls when enabled.
#[derive(Clone, Copy, Debug)]
pub struct HudControls<'a> {
    pub swatches: &'a [HudSwatch],
    pub show_note: bool,
    pub show_delete: bool,
}

/// Where a [`SelectionHud`] floats: `anchor` is the horizontal centre and
/// top-y of the selection's first band, `bounds` the area the bar must stay
/// inside, both in the drawing transform's space.
#[derive(Clone, Copy, Debug)]
pub struct HudPlacement {
    pub anchor: Point,
    pub bounds: Rect,
}

/// Passing resolved colours rather than a `Theme` keeps the component usable
/// from a reader whose *page* palette differs from OS chrome.
#[derive(Clone, Copy, Debug)]
pub struct ReaderHudStyle {
    pub surface: Color,
    pub border: Color,
    /// A token rather than a colour: strength, offset, and blur are one
    /// decision, and a caller handed only a colour has no choice but to
    /// invent the other two — which is how this came to paint a hard-edged
    /// dark copy of the pill peeking out from under it.
    pub elevation: crate::tokens::Shadow,
    pub text: Color,
    pub hover: Color,
    /// Resting fill of the Copy / Note pills.
    pub pill: Color,
    pub ring: Color,
    /// Emphasis for the delete control on hover.
    pub danger: Color,
}

/// A drawn [`SelectionHud`]: its bounding rect plus the hit rects of its
/// controls. Built fresh each frame by [`SelectionHud::draw`]; the caller either
/// hit-tests it directly via [`SelectionHud::hit`] or folds its buttons into its
/// own hit list via [`SelectionHud::buttons`].
#[derive(Clone, Debug, Default)]
pub struct SelectionHud {
    rect: Rect,
    buttons: Vec<(Rect, HudAction)>,
    shown: bool,
}

/// Geometry constants, shared so both readers get pixel-identical bars.
mod dims {
    pub const PAD: f64 = 12.0;
    pub const HEIGHT: f64 = 42.0;
    pub const DOT_R: f64 = 9.0;
    pub const DOT_GAP: f64 = 10.0;
    pub const PILL_GAP: f64 = 8.0;
    pub const SEP_W: f64 = 13.0;
    pub const TRASH: f64 = 24.0;
    /// Gap between the anchor point and the bar when floating above.
    pub const ANCHOR_GAP: f64 = 12.0;
}

/// Geometry [`SelectionHud::measure`] computes once and the draw helpers
/// below consume — split out of [`SelectionHud::draw`] so that function
/// stays under the line-count limit without changing any of its arithmetic.
struct HudMeasurements<'a> {
    controls: HudControls<'a>,
    rect: Rect,
    hud_x: f64,
    cyc: f64,
    dots_w: f64,
    sep_w: f64,
    copy_w: f64,
    note_w: f64,
}

impl SelectionHud {
    /// The bar centres on `placement.anchor.x` and prefers just above
    /// `placement.anchor.y`, dropping below if that would leave
    /// `placement.bounds`. `cursor` drives the hover treatment.
    pub fn draw(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        style: &ReaderHudStyle,
        controls: &HudControls<'_>,
        placement: HudPlacement,
        cursor: Point,
    ) -> Self {
        let m = Self::measure(tr, *controls, placement);
        let mut hud = SelectionHud {
            rect: m.rect,
            shown: true,
            ..SelectionHud::default()
        };

        let radius = dims::HEIGHT / 2.0;
        // Shared painter, so the HUD is lifted by the same material as every other surface.
        crate::surface::shadow(scene, transform, m.rect, radius, &style.elevation);
        fill_round(scene, transform, m.rect, radius, style.surface);
        stroke_round(scene, transform, m.rect, radius, style.border, 1.0);

        hud.draw_swatches(scene, transform, style, cursor, &m);
        hud.draw_pills(scene, transform, tr, style, cursor, &m);

        hud
    }

    /// The sizing/positioning half of [`Self::draw`] — every value here is
    /// pixel-comparing-test-sensitive, so this is pure code motion out of
    /// `draw`, not a rewrite.
    fn measure<'a>(
        tr: &mut TextRenderer,
        controls: HudControls<'a>,
        placement: HudPlacement,
    ) -> HudMeasurements<'a> {
        use dims::{ANCHOR_GAP, DOT_GAP, DOT_R, HEIGHT, PAD, PILL_GAP, SEP_W, TRASH};
        let HudControls {
            swatches,
            show_note,
            show_delete,
        } = controls;
        let HudPlacement { anchor, bounds } = placement;

        // Swatch counts are always small (a handful of highlight colours);
        // this cast can't lose meaningful precision, and the layout formula
        // it feeds must stay byte-for-byte identical to avoid a
        // pixel-snapshot regression.
        #[allow(
            clippy::cast_precision_loss,
            reason = "swatch counts are always small; this feeds pixel-snapshot-sensitive layout math that must not change"
        )]
        let n = swatches.len().max(1) as f64;
        let dots_w = n * (DOT_R * 2.0) + (n - 1.0).max(0.0) * DOT_GAP;
        let (cw, _) = tr.measure_weighted("Copy", 13.0, None, TextWeight::Medium);
        let copy_w = f64::from(cw) + 24.0;
        let (nw, _) = tr.measure_weighted("Note", 13.0, None, TextWeight::Medium);
        let note_w = f64::from(nw) + 24.0;
        let note_extra = if show_note { PILL_GAP + note_w } else { 0.0 };
        let trash_extra = if show_delete { TRASH + 8.0 } else { 0.0 };
        let sep_w = if swatches.is_empty() { 0.0 } else { SEP_W };
        let hud_w = PAD * 2.0 + dots_w + sep_w + copy_w + note_extra + trash_extra;

        let hud_x = (anchor.x - hud_w / 2.0).clamp(
            bounds.x0 + 8.0,
            (bounds.x1 - 8.0 - hud_w).max(bounds.x0 + 8.0),
        );
        let above = anchor.y - HEIGHT - ANCHOR_GAP;
        let hud_y = if above >= bounds.y0 + 6.0 {
            above
        } else {
            (anchor.y + 30.0)
                .min(bounds.y1 - HEIGHT - 6.0)
                .max(bounds.y0 + 6.0)
        };
        let rect = Rect::new(hud_x, hud_y, hud_x + hud_w, hud_y + HEIGHT);
        let cyc = hud_y + HEIGHT / 2.0;
        HudMeasurements {
            controls,
            rect,
            hud_x,
            cyc,
            dots_w,
            sep_w,
            copy_w,
            note_w,
        }
    }

    /// The swatch-dot row + its divider, the second half of [`Self::draw`].
    fn draw_swatches(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        style: &ReaderHudStyle,
        cursor: Point,
        m: &HudMeasurements<'_>,
    ) {
        use dims::{DOT_GAP, DOT_R, PAD};
        let swatches = m.controls.swatches;

        let mut x = m.hud_x + PAD + DOT_R;
        for (i, sw) in swatches.iter().enumerate() {
            let center = Point::new(x, m.cyc);
            let dot = Rect::new(
                center.x - DOT_R,
                center.y - DOT_R,
                center.x + DOT_R,
                center.y + DOT_R,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                Color::from_rgb8(sw.rgb[0], sw.rgb[1], sw.rgb[2]),
                None,
                &Circle::new(center, DOT_R),
            );
            if sw.selected {
                stroke_round(
                    scene,
                    transform,
                    dot.inflate(2.5, 2.5),
                    DOT_R + 2.5,
                    style.ring,
                    1.8,
                );
            } else if dot.inflate(3.0, 3.0).contains(cursor) {
                stroke_round(
                    scene,
                    transform,
                    dot.inflate(2.5, 2.5),
                    DOT_R + 2.5,
                    style.border,
                    1.2,
                );
            }
            self.buttons
                .push((dot.inflate(3.0, 3.0), HudAction::Highlight(i)));
            x += DOT_R * 2.0 + DOT_GAP;
        }

        let dots_end = m.hud_x + PAD + m.dots_w;
        if !swatches.is_empty() {
            let div_x = dots_end + m.sep_w / 2.0;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                style.border,
                None,
                &Line::new(
                    Point::new(div_x, m.cyc - 10.0),
                    Point::new(div_x, m.cyc + 10.0),
                ),
            );
        }
    }

    /// The Copy / Note / Delete pills, the third half of [`Self::draw`].
    fn draw_pills(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        style: &ReaderHudStyle,
        cursor: Point,
        m: &HudMeasurements<'_>,
    ) {
        use dims::{PAD, PILL_GAP, TRASH};
        let HudControls {
            show_note,
            show_delete,
            ..
        } = m.controls;

        let dots_end = m.hud_x + PAD + m.dots_w;
        let copy_x = dots_end + m.sep_w;
        let copy_rect = Rect::new(copy_x, m.cyc - 14.0, copy_x + m.copy_w, m.cyc + 14.0);
        let copy_hover = copy_rect.contains(cursor);
        fill_round(
            scene,
            transform,
            copy_rect,
            14.0,
            if copy_hover { style.hover } else { style.pill },
        );
        // `Affine`'s `Mul` composes two transforms; it has no failure mode, but
        // clippy can't see that for a non-primitive operator overload.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine's Mul composes transform matrices and cannot overflow/panic"
        )]
        let copy_label_transform = transform * Affine::translate((copy_x + 12.0, m.cyc - 7.0));
        tr.draw_text_weighted(
            scene,
            "Copy",
            13.0,
            style.text,
            copy_label_transform,
            None,
            TextWeight::Medium,
        );
        self.buttons.push((copy_rect, HudAction::Copy));

        let mut cursor_x = copy_x + m.copy_w;
        if show_note {
            let note_x = cursor_x + PILL_GAP;
            let note_rect = Rect::new(note_x, m.cyc - 14.0, note_x + m.note_w, m.cyc + 14.0);
            let note_hover = note_rect.contains(cursor);
            fill_round(
                scene,
                transform,
                note_rect,
                14.0,
                if note_hover { style.hover } else { style.pill },
            );
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine's Mul composes transform matrices and cannot overflow/panic"
            )]
            let note_label_transform = transform * Affine::translate((note_x + 12.0, m.cyc - 7.0));
            tr.draw_text_weighted(
                scene,
                "Note",
                13.0,
                style.text,
                note_label_transform,
                None,
                TextWeight::Medium,
            );
            self.buttons.push((note_rect, HudAction::Note));
            cursor_x = note_x + m.note_w;
        }

        if show_delete {
            let tx = cursor_x + 8.0;
            let trash = Rect::new(tx, m.cyc - TRASH / 2.0, tx + TRASH, m.cyc + TRASH / 2.0);
            let thov = trash.contains(cursor);
            if thov {
                fill_round(scene, transform, trash, 6.0, style.hover);
            }
            draw_trash(
                scene,
                transform,
                trash,
                if thov { style.danger } else { style.text },
                1.5,
            );
            self.buttons.push((trash, HudAction::Delete));
        }
    }

    /// The control rects + their actions, for a caller that folds them into its
    /// own hit list (the ebook records `Action`s this way).
    pub fn buttons(&self) -> &[(Rect, HudAction)] {
        &self.buttons
    }

    /// Hit-test a pointer against the drawn controls (topmost/last wins).
    pub fn hit(&self, p: Point) -> Option<HudAction> {
        self.buttons
            .iter()
            .rev()
            .find(|(r, _)| r.contains(p))
            .map(|(_, a)| *a)
    }

    /// The bar's bounding rect (a caller can swallow taps on it).
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Whether the pointer is anywhere over the bar body.
    pub fn contains(&self, p: Point) -> bool {
        self.shown && self.rect.contains(p)
    }

    pub fn is_shown(&self) -> bool {
        self.shown
    }
}

/// A reader begins a drag when a press lands on a marker it owns, tracks the
/// pointer while held, and reads back the marker origin on release to
/// persist the new anchor. Agnostic of the coordinate space (page points vs.
/// screen-y) — the caller decides how to interpret the returned origin.
#[derive(Clone, Debug)]
pub struct MarkerDrag {
    /// Opaque to this type.
    pub id: String,
    /// Captured at grab, so the marker doesn't jump to the cursor on the first move.
    grab_off: (f64, f64),
    /// Past [`Self::THRESHOLD`] since the grab — below it, a release is a click, not a move.
    moved: bool,
    /// As of the last [`Self::update`].
    origin: (f64, f64),
}

impl MarkerDrag {
    /// Movement (in the caller's units) past which a press becomes a drag.
    pub const THRESHOLD: f64 = 4.0;

    pub fn begin(id: impl Into<String>, pointer: (f64, f64), origin: (f64, f64)) -> Self {
        Self {
            id: id.into(),
            grab_off: (pointer.0 - origin.0, pointer.1 - origin.1),
            moved: false,
            origin,
        }
    }

    /// Returns the marker's new origin (pointer minus the grab offset).
    /// Latches [`Self::moved`] once travelled past the threshold from where it started.
    pub fn update(&mut self, pointer: (f64, f64)) -> (f64, f64) {
        let new = (pointer.0 - self.grab_off.0, pointer.1 - self.grab_off.1);
        if (new.0 - self.origin.0).hypot(new.1 - self.origin.1) > Self::THRESHOLD {
            self.moved = true;
        }
        // `self.origin` is never advanced — it stays the travel datum; callers
        // read the returned value each move.
        new
    }

    pub fn moved(&self) -> bool {
        self.moved
    }
}

// Vector helpers below are kept local so the module has no app dependency.

/// A minimal trash glyph (lid + can) inside `r`.
fn draw_trash(scene: &mut Scene, t: Affine, r: Rect, color: Color, w: f64) {
    let cx0 = r.x0 + r.width() * 0.22;
    let cx1 = r.x1 - r.width() * 0.22;
    let top = r.y0 + r.height() * 0.30;
    let bot = r.y1 - r.height() * 0.18;
    let stroke = Stroke::new(w);
    // lid
    scene.stroke(
        &stroke,
        t,
        color,
        None,
        &Line::new(
            Point::new(r.x0 + r.width() * 0.14, top),
            Point::new(r.x1 - r.width() * 0.14, top),
        ),
    );
    // handle
    scene.stroke(
        &stroke,
        t,
        color,
        None,
        &Line::new(
            Point::new(cx0 + 2.0, top),
            Point::new(cx0 + 2.0, top - r.height() * 0.12),
        ),
    );
    scene.stroke(
        &stroke,
        t,
        color,
        None,
        &Line::new(
            Point::new(cx1 - 2.0, top),
            Point::new(cx1 - 2.0, top - r.height() * 0.12),
        ),
    );
    scene.stroke(
        &stroke,
        t,
        color,
        None,
        &Line::new(
            Point::new(cx0 + 2.0, top - r.height() * 0.12),
            Point::new(cx1 - 2.0, top - r.height() * 0.12),
        ),
    );
    // can sides + bottom
    scene.stroke(
        &stroke,
        t,
        color,
        None,
        &Line::new(Point::new(cx0, top), Point::new(cx0 + 1.0, bot)),
    );
    scene.stroke(
        &stroke,
        t,
        color,
        None,
        &Line::new(Point::new(cx1, top), Point::new(cx1 - 1.0, bot)),
    );
    scene.stroke(
        &stroke,
        t,
        color,
        None,
        &Line::new(Point::new(cx0 + 1.0, bot), Point::new(cx1 - 1.0, bot)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> ReaderHudStyle {
        let c = Color::from_rgb8(0x22, 0x22, 0x22);
        ReaderHudStyle {
            surface: c,
            border: c,
            elevation: crate::theme::Theme::dark().tokens.elevation.e2,
            text: c,
            hover: c,
            pill: c,
            ring: c,
            danger: c,
        }
    }

    fn swatches() -> Vec<HudSwatch> {
        vec![
            HudSwatch::new([250, 204, 21], true),
            HudSwatch::new([52, 211, 153], false),
            HudSwatch::new([96, 165, 250], false),
        ]
    }

    /// Asserts on the real geometry the readers will hit-test, not a re-derived layout.
    fn draw_hud(show_note: bool, show_delete: bool, anchor: Point, bounds: Rect) -> SelectionHud {
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        SelectionHud::draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &style(),
            &HudControls {
                swatches: &swatches(),
                show_note,
                show_delete,
            },
            HudPlacement { anchor, bounds },
            Point::new(-1000.0, -1000.0),
        )
    }

    #[test]
    fn hud_layout_places_every_control_inside_bounds() {
        // Case `hud_lays_out_copy_and_every_swatch`.
        {
            let hud = draw_hud(
                true,
                false,
                Point::new(400.0, 300.0),
                Rect::new(0.0, 0.0, 800.0, 600.0),
            );
            assert!(hud.is_shown());
            // Every swatch + Copy + Note is a hittable button.
            let highlights: Vec<usize> = hud
                .buttons()
                .iter()
                .filter_map(|(_, a)| {
                    if let HudAction::Highlight(i) = a {
                        Some(*i)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(highlights, vec![0, 1, 2], "one hit per swatch, in order");
            assert!(
                hud.buttons().iter().any(|(_, a)| *a == HudAction::Copy),
                "Copy present"
            );
            assert!(
                hud.buttons().iter().any(|(_, a)| *a == HudAction::Note),
                "Note present"
            );
            assert!(
                !hud.buttons().iter().any(|(_, a)| *a == HudAction::Delete),
                "no delete without a mark"
            );
        }

        // Case `delete_appears_only_when_asked`.
        {
            let hud = draw_hud(
                true,
                true,
                Point::new(400.0, 300.0),
                Rect::new(0.0, 0.0, 800.0, 600.0),
            );
            assert!(hud.buttons().iter().any(|(_, a)| *a == HudAction::Delete));
        }

        // Case `hud_stays_inside_bounds_when_anchor_is_at_an_edge`.
        {
            let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
            // Anchor jammed into the top-left corner: the bar must not spill out.
            let hud = draw_hud(true, true, Point::new(2.0, 2.0), bounds);
            let r = hud.rect();
            assert!(r.x0 >= bounds.x0, "left edge kept in bounds: {r:?}");
            assert!(r.x1 <= bounds.x1 + 0.01, "right edge kept in bounds: {r:?}");
            assert!(r.y0 >= bounds.y0, "top edge kept in bounds: {r:?}");
            assert!(
                r.y1 <= bounds.y1 + 0.01,
                "bottom edge kept in bounds: {r:?}"
            );
        }

        // Case `hud_floats_below_when_there_is_no_room_above`.
        {
            let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
            // Anchor near the top → the bar drops below it rather than off-screen.
            let hud = draw_hud(true, false, Point::new(400.0, 10.0), bounds);
            assert!(
                hud.rect().y0 >= 10.0,
                "bar sits at/below the anchor near the top"
            );
        }
    }

    #[test]
    fn hud_hits_controls_and_latches_marker_drags() {
        // Case `hit_returns_the_control_under_the_point`.
        {
            let hud = draw_hud(
                true,
                false,
                Point::new(400.0, 300.0),
                Rect::new(0.0, 0.0, 800.0, 600.0),
            );
            let copy = hud
                .buttons()
                .iter()
                .find(|(_, a)| *a == HudAction::Copy)
                .unwrap()
                .0;
            assert_eq!(hud.hit(copy.center()), Some(HudAction::Copy));
            assert_eq!(hud.hit(Point::new(5.0, 5.0)), None);
        }

        // Case `marker_drag_latches_moved_past_threshold`.
        {
            let mut d = MarkerDrag::begin("n1", (100.0, 100.0), (90.0, 95.0));
            // A grab keeps the offset so the first tiny move doesn't jump.
            let o = d.update((101.0, 101.0));
            assert!(!d.moved(), "a sub-threshold nudge is still a click");
            assert!(
                (o.0 - 91.0).abs() < 1e-6 && (o.1 - 96.0).abs() < 1e-6,
                "origin tracks pointer minus grab offset"
            );
            let o2 = d.update((130.0, 130.0));
            assert!(d.moved(), "a real drag latches moved");
            assert!(o2.0 > o.0 && o2.1 > o.1);
            assert_eq!(d.id, "n1");
        }
    }
}
