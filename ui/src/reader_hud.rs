//! Shared reading-surface chrome for the document readers (ebook, pdf_viewer).
//!
//! Two pieces the readers used to each hand-roll now live here, so both surfaces
//! present the *same* selection affordances with the same spacing, hover, and
//! selected-state treatment:
//!
//! - [`SelectionHud`] — the floating toolbar shown while a passage is selected:
//!   a row of highlight swatches, a **Copy** action, a **Note** action, and a
//!   conditional **delete** (shown when the selection already covers a mark).
//!   It is presentation + hit-testing only: each app owns the selection model
//!   and maps the emitted [`HudAction`] to its own command. Copy is wired to
//!   [`crate::clipboard_copy`] by both apps.
//! - [`MarkerDrag`] — the tiny grab/track/threshold state a reader embeds to let
//!   an annotation marker be *dragged*: the pdf reader repositions a sticky note
//!   in page space; the ebook reader slides a margin marker to re-anchor it to
//!   another paragraph.
//!
//! Nothing here reaches into a document — it draws from a resolved
//! [`ReaderHudStyle`] (colours the caller lifts from its `Theme` + reading
//! palette) and reports hits back. This keeps the component theme-driven (no
//! hard-coded colours) while staying agnostic of either reader's data model.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::text_render::{TextRenderer, TextWeight};

/// An action one of the HUD's controls emits when clicked. The swatch index in
/// [`HudAction::Highlight`] indexes into the `swatches` slice passed to
/// [`SelectionHud::draw`], so the caller maps it back to its own colour model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HudAction {
    /// Copy the selected passage to the clipboard.
    Copy,
    /// Apply/toggle the highlight colour at this swatch index.
    Highlight(usize),
    /// Attach a note to the selection.
    Note,
    /// Remove the mark the selection covers.
    Delete,
}

/// One highlight colour offered in the HUD's swatch row.
#[derive(Clone, Copy, Debug)]
pub struct HudSwatch {
    /// The swatch's on-screen sRGB fill.
    pub rgb: [u8; 3],
    /// Whether this is the reader's currently-armed colour (draws a ring).
    pub selected: bool,
}

impl HudSwatch {
    pub fn new(rgb: [u8; 3], selected: bool) -> Self {
        Self { rgb, selected }
    }
}

/// Resolved colours for the HUD, lifted by the caller from its `Theme` (chrome)
/// and reading palette (accent). Passing colours rather than a `Theme` keeps the
/// component usable from a reader whose *page* palette differs from OS chrome.
#[derive(Clone, Copy, Debug)]
pub struct ReaderHudStyle {
    /// The bar's body fill (a raised surface).
    pub surface: Color,
    /// The bar's hairline border.
    pub border: Color,
    /// Drop-shadow colour (already alpha-premultiplied by the caller if needed).
    pub shadow: Color,
    /// Primary label ink.
    pub text: Color,
    /// Fill under a hovered control.
    pub hover: Color,
    /// Resting fill of the Copy / Note pills.
    pub pill: Color,
    /// Ring drawn around the armed swatch.
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

impl SelectionHud {
    /// Compute, draw, and return the selection HUD.
    ///
    /// - `swatches` — the highlight colours to offer (indices flow back through
    ///   [`HudAction::Highlight`]).
    /// - `show_note` / `show_delete` — whether those controls appear. Copy is
    ///   always present.
    /// - `cursor` — pointer position, for hover feedback.
    /// - `anchor` — the point the bar floats above (the horizontal centre and
    ///   the top-y of the selection's first band, in the same space as
    ///   `transform`). The bar is centred on `anchor.x` and prefers to sit just
    ///   above `anchor.y`, dropping below if it would leave `bounds`.
    /// - `bounds` — the region to keep the bar inside (the reading viewport).
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        style: &ReaderHudStyle,
        swatches: &[HudSwatch],
        show_note: bool,
        show_delete: bool,
        cursor: Point,
        anchor: Point,
        bounds: Rect,
    ) -> Self {
        use dims::*;
        let mut hud = SelectionHud::default();

        // ---- width ----------------------------------------------------------
        let n = swatches.len().max(1) as f64;
        let dots_w = n * (DOT_R * 2.0) + (n - 1.0).max(0.0) * DOT_GAP;
        let (cw, _) = tr.measure_weighted("Copy", 13.0, None, TextWeight::Medium);
        let copy_w = cw as f64 + 24.0;
        let (nw, _) = tr.measure_weighted("Note", 13.0, None, TextWeight::Medium);
        let note_w = nw as f64 + 24.0;
        let note_extra = if show_note { PILL_GAP + note_w } else { 0.0 };
        let trash_extra = if show_delete { TRASH + 8.0 } else { 0.0 };
        let sep_w = if swatches.is_empty() { 0.0 } else { SEP_W };
        let hud_w = PAD * 2.0 + dots_w + sep_w + copy_w + note_extra + trash_extra;

        // ---- position: centred on anchor.x, floating above anchor.y ---------
        let hud_x = (anchor.x - hud_w / 2.0).clamp(bounds.x0 + 8.0, (bounds.x1 - 8.0 - hud_w).max(bounds.x0 + 8.0));
        let above = anchor.y - HEIGHT - ANCHOR_GAP;
        let hud_y = if above >= bounds.y0 + 6.0 {
            above
        } else {
            (anchor.y + 30.0).min(bounds.y1 - HEIGHT - 6.0).max(bounds.y0 + 6.0)
        };
        let rect = Rect::new(hud_x, hud_y, hud_x + hud_w, hud_y + HEIGHT);
        hud.rect = rect;
        hud.shown = true;

        // ---- body -----------------------------------------------------------
        let radius = HEIGHT / 2.0;
        let shadow = Rect::new(rect.x0 - 1.0, rect.y0 + 3.0, rect.x1 + 1.0, rect.y1 + 5.0);
        fill_round(scene, transform, shadow, radius, style.shadow);
        fill_round(scene, transform, rect, radius, style.surface);
        stroke_round(scene, transform, rect, radius, style.border, 1.0);

        let cyc = hud_y + HEIGHT / 2.0;

        // ---- swatch row -----------------------------------------------------
        let mut x = hud_x + PAD + DOT_R;
        for (i, sw) in swatches.iter().enumerate() {
            let center = Point::new(x, cyc);
            let dot = Rect::new(center.x - DOT_R, center.y - DOT_R, center.x + DOT_R, center.y + DOT_R);
            scene.fill(
                Fill::NonZero,
                transform,
                Color::from_rgb8(sw.rgb[0], sw.rgb[1], sw.rgb[2]),
                None,
                &Circle::new(center, DOT_R),
            );
            if sw.selected {
                stroke_round(scene, transform, dot.inflate(2.5, 2.5), DOT_R + 2.5, style.ring, 1.8);
            } else if dot.inflate(3.0, 3.0).contains(cursor) {
                stroke_round(scene, transform, dot.inflate(2.5, 2.5), DOT_R + 2.5, style.border, 1.2);
            }
            hud.buttons.push((dot.inflate(3.0, 3.0), HudAction::Highlight(i)));
            x += DOT_R * 2.0 + DOT_GAP;
        }

        // ---- divider --------------------------------------------------------
        let dots_end = hud_x + PAD + dots_w;
        if !swatches.is_empty() {
            let div_x = dots_end + sep_w / 2.0;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                style.border,
                None,
                &Line::new(Point::new(div_x, cyc - 10.0), Point::new(div_x, cyc + 10.0)),
            );
        }

        // ---- Copy pill ------------------------------------------------------
        let copy_x = dots_end + sep_w;
        let copy_rect = Rect::new(copy_x, cyc - 14.0, copy_x + copy_w, cyc + 14.0);
        let copy_hover = copy_rect.contains(cursor);
        fill_round(scene, transform, copy_rect, 14.0, if copy_hover { style.hover } else { style.pill });
        tr.draw_text_weighted(
            scene,
            "Copy",
            13.0,
            style.text,
            transform * Affine::translate((copy_x + 12.0, cyc - 7.0)),
            None,
            TextWeight::Medium,
        );
        hud.buttons.push((copy_rect, HudAction::Copy));

        // ---- Note pill ------------------------------------------------------
        let mut cursor_x = copy_x + copy_w;
        if show_note {
            let note_x = cursor_x + PILL_GAP;
            let note_rect = Rect::new(note_x, cyc - 14.0, note_x + note_w, cyc + 14.0);
            let note_hover = note_rect.contains(cursor);
            fill_round(scene, transform, note_rect, 14.0, if note_hover { style.hover } else { style.pill });
            tr.draw_text_weighted(
                scene,
                "Note",
                13.0,
                style.text,
                transform * Affine::translate((note_x + 12.0, cyc - 7.0)),
                None,
                TextWeight::Medium,
            );
            hud.buttons.push((note_rect, HudAction::Note));
            cursor_x = note_x + note_w;
        }

        // ---- delete ---------------------------------------------------------
        if show_delete {
            let tx = cursor_x + 8.0;
            let trash = Rect::new(tx, cyc - TRASH / 2.0, tx + TRASH, cyc + TRASH / 2.0);
            let thov = trash.contains(cursor);
            if thov {
                fill_round(scene, transform, trash, 6.0, style.hover);
            }
            draw_trash(scene, transform, trash, if thov { style.danger } else { style.text }, 1.5);
            hud.buttons.push((trash, HudAction::Delete));
        }

        hud
    }

    /// The control rects + their actions, for a caller that folds them into its
    /// own hit list (the ebook records `Action`s this way).
    pub fn buttons(&self) -> &[(Rect, HudAction)] {
        &self.buttons
    }

    /// Hit-test a pointer against the drawn controls (topmost/last wins).
    pub fn hit(&self, p: Point) -> Option<HudAction> {
        self.buttons.iter().rev().find(|(r, _)| r.contains(p)).map(|(_, a)| *a)
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

// ===========================================================================
// Draggable annotation markers
// ===========================================================================

/// The grab/track state of a marker drag. A reader begins one when a press lands
/// on a marker it owns, tracks the pointer while held, and reads back the marker
/// origin on release to persist the new anchor. It stays agnostic of the
/// coordinate space (page points for the pdf reader, screen-y for the ebook
/// margin) — the caller decides how to interpret the returned origin.
#[derive(Clone, Debug)]
pub struct MarkerDrag {
    /// Which annotation is being dragged (opaque to this type).
    pub id: String,
    /// Pointer offset from the marker origin captured at grab, so the marker
    /// doesn't jump to the cursor on the first move.
    grab_off: (f64, f64),
    /// Whether the pointer has moved past [`Self::THRESHOLD`] since the grab —
    /// below it, a release is a click, not a move.
    moved: bool,
    /// The marker origin as of the last [`Self::update`].
    origin: (f64, f64),
}

impl MarkerDrag {
    /// Movement (in the caller's units) past which a press becomes a drag.
    pub const THRESHOLD: f64 = 4.0;

    /// Begin dragging marker `id` whose origin is `origin`, grabbed at `pointer`.
    pub fn begin(id: impl Into<String>, pointer: (f64, f64), origin: (f64, f64)) -> Self {
        Self {
            id: id.into(),
            grab_off: (pointer.0 - origin.0, pointer.1 - origin.1),
            moved: false,
            origin,
        }
    }

    /// Track the pointer to a new position; returns the marker's new origin
    /// (pointer minus the grab offset). Latches [`Self::moved`] once the origin
    /// has travelled past the threshold from where it started.
    pub fn update(&mut self, pointer: (f64, f64)) -> (f64, f64) {
        let new = (pointer.0 - self.grab_off.0, pointer.1 - self.grab_off.1);
        if (new.0 - self.origin.0).hypot(new.1 - self.origin.1) > Self::THRESHOLD {
            self.moved = true;
        }
        // Note: origin is only advanced enough to latch `moved`; callers read the
        // returned value each move. Keep the initial origin as the travel datum.
        new
    }

    /// Whether this drag has moved far enough to count as a reposition (vs a
    /// plain click that should fall through to selection).
    pub fn moved(&self) -> bool {
        self.moved
    }
}

// ===========================================================================
// Small vector helpers (kept local so the module has no app dependency)
// ===========================================================================

fn fill_round(scene: &mut Scene, t: Affine, r: Rect, radius: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &RoundedRect::from_rect(r, radius));
}

fn stroke_round(scene: &mut Scene, t: Affine, r: Rect, radius: f64, color: Color, w: f64) {
    scene.stroke(&Stroke::new(w), t, color, None, &RoundedRect::from_rect(r, radius));
}

/// A minimal trash glyph inside `r` (a lid + a can), matching the readers' own
/// iconography closely enough to read as "delete".
fn draw_trash(scene: &mut Scene, t: Affine, r: Rect, color: Color, w: f64) {
    let cx0 = r.x0 + r.width() * 0.22;
    let cx1 = r.x1 - r.width() * 0.22;
    let top = r.y0 + r.height() * 0.30;
    let bot = r.y1 - r.height() * 0.18;
    let stroke = Stroke::new(w);
    // lid
    scene.stroke(&stroke, t, color, None, &Line::new(Point::new(r.x0 + r.width() * 0.14, top), Point::new(r.x1 - r.width() * 0.14, top)));
    // handle
    scene.stroke(&stroke, t, color, None, &Line::new(Point::new(cx0 + 2.0, top), Point::new(cx0 + 2.0, top - r.height() * 0.12)));
    scene.stroke(&stroke, t, color, None, &Line::new(Point::new(cx1 - 2.0, top), Point::new(cx1 - 2.0, top - r.height() * 0.12)));
    scene.stroke(&stroke, t, color, None, &Line::new(Point::new(cx0 + 2.0, top - r.height() * 0.12), Point::new(cx1 - 2.0, top - r.height() * 0.12)));
    // can sides + bottom
    scene.stroke(&stroke, t, color, None, &Line::new(Point::new(cx0, top), Point::new(cx0 + 1.0, bot)));
    scene.stroke(&stroke, t, color, None, &Line::new(Point::new(cx1, top), Point::new(cx1 - 1.0, bot)));
    scene.stroke(&stroke, t, color, None, &Line::new(Point::new(cx0 + 1.0, bot), Point::new(cx1 - 1.0, bot)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> ReaderHudStyle {
        let c = Color::from_rgb8(0x22, 0x22, 0x22);
        ReaderHudStyle {
            surface: c,
            border: c,
            shadow: c,
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

    /// Draw the HUD into a throwaway scene and return it, so we assert on the
    /// real geometry the readers will hit-test — not a re-derived layout.
    fn draw_hud(show_note: bool, show_delete: bool, anchor: Point, bounds: Rect) -> SelectionHud {
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        SelectionHud::draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &style(),
            &swatches(),
            show_note,
            show_delete,
            Point::new(-1000.0, -1000.0),
            anchor,
            bounds,
        )
    }

    #[test]
    fn hud_lays_out_copy_and_every_swatch() {
        let hud = draw_hud(true, false, Point::new(400.0, 300.0), Rect::new(0.0, 0.0, 800.0, 600.0));
        assert!(hud.is_shown());
        // Every swatch + Copy + Note is a hittable button.
        let highlights: Vec<usize> = hud
            .buttons()
            .iter()
            .filter_map(|(_, a)| if let HudAction::Highlight(i) = a { Some(*i) } else { None })
            .collect();
        assert_eq!(highlights, vec![0, 1, 2], "one hit per swatch, in order");
        assert!(hud.buttons().iter().any(|(_, a)| *a == HudAction::Copy), "Copy present");
        assert!(hud.buttons().iter().any(|(_, a)| *a == HudAction::Note), "Note present");
        assert!(!hud.buttons().iter().any(|(_, a)| *a == HudAction::Delete), "no delete without a mark");
    }

    #[test]
    fn delete_appears_only_when_asked() {
        let hud = draw_hud(true, true, Point::new(400.0, 300.0), Rect::new(0.0, 0.0, 800.0, 600.0));
        assert!(hud.buttons().iter().any(|(_, a)| *a == HudAction::Delete));
    }

    #[test]
    fn hit_returns_the_control_under_the_point() {
        let hud = draw_hud(true, false, Point::new(400.0, 300.0), Rect::new(0.0, 0.0, 800.0, 600.0));
        // The centre of the Copy pill hit-tests to Copy.
        let copy = hud.buttons().iter().find(|(_, a)| *a == HudAction::Copy).unwrap().0;
        assert_eq!(hud.hit(copy.center()), Some(HudAction::Copy));
        // A point well outside the bar hits nothing.
        assert_eq!(hud.hit(Point::new(5.0, 5.0)), None);
    }

    #[test]
    fn hud_stays_inside_bounds_when_anchor_is_at_an_edge() {
        let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
        // Anchor jammed into the top-left corner: the bar must not spill out.
        let hud = draw_hud(true, true, Point::new(2.0, 2.0), bounds);
        let r = hud.rect();
        assert!(r.x0 >= bounds.x0, "left edge kept in bounds: {r:?}");
        assert!(r.x1 <= bounds.x1 + 0.01, "right edge kept in bounds: {r:?}");
        assert!(r.y0 >= bounds.y0, "top edge kept in bounds: {r:?}");
        assert!(r.y1 <= bounds.y1 + 0.01, "bottom edge kept in bounds: {r:?}");
    }

    #[test]
    fn hud_floats_below_when_there_is_no_room_above() {
        let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
        // Anchor near the top → the bar drops below it rather than off-screen.
        let hud = draw_hud(true, false, Point::new(400.0, 10.0), bounds);
        assert!(hud.rect().y0 >= 10.0, "bar sits at/below the anchor near the top");
    }

    #[test]
    fn marker_drag_latches_moved_past_threshold() {
        let mut d = MarkerDrag::begin("n1", (100.0, 100.0), (90.0, 95.0));
        // A grab keeps the offset so the first tiny move doesn't jump.
        let o = d.update((101.0, 101.0));
        assert!(!d.moved(), "a sub-threshold nudge is still a click");
        assert!((o.0 - 91.0).abs() < 1e-6 && (o.1 - 96.0).abs() < 1e-6, "origin tracks pointer minus grab offset");
        let o2 = d.update((130.0, 130.0));
        assert!(d.moved(), "a real drag latches moved");
        assert!(o2.0 > o.0 && o2.1 > o.1);
        assert_eq!(d.id, "n1");
    }
}
