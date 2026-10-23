//! Shared reading-surface chrome for the document readers (ebook, `pdf_viewer`),
//! so both present the *same* selection affordances with the same spacing,
//! hover, and selected-state treatment:
//!
//! - [`SelectionHud`] — the floating toolbar shown while a passage is
//!   selected: a floating Panel ([`crate::widget::floating_surface`]) with
//!   the highlight colours as [`ColorSwatchFace`]s and Copy / Note / Delete
//!   as plain buttons (and Select all, with shortcuts where a keyboard is
//!   the way in). Presentation + hit-testing only: each app owns the
//!   selection model and maps the emitted [`HudAction`] to its own command.
//! - [`MarkerDrag`] — the grab/track/threshold state a reader embeds to let
//!   an annotation marker be *dragged*, agnostic of the coordinate space
//!   (page points vs. screen-y).
//!
//! Nothing here reaches into a document. The HUD is chrome, so it reads the
//! window's theme end to end; the reading palette stays on the page.

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::Color;

use crate::icons::Icon;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::color_swatch::ColorSwatchFace;
use crate::widget::{Button, ButtonFace, ButtonRole, ControlSize, floating_surface};

/// The swatch index in [`HudAction::Highlight`] indexes into the `swatches`
/// slice passed to [`SelectionHud::draw`]; the caller maps it back to its own colour model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HudAction {
    Copy,
    Highlight(usize),
    Note,
    /// Removes the mark the selection covers.
    Delete,
    /// Selects everything the selection could cover.
    SelectAll,
}

/// One highlight colour offered in the HUD's swatch row.
#[derive(Clone, Copy, Debug)]
pub struct HudSwatch {
    pub rgb: [u8; 3],
    /// The reader's currently-armed colour; draws a ring.
    pub selected: bool,
    /// What the colour is called ("Yellow"): its tooltip and accessible name.
    pub name: &'static str,
}

impl HudSwatch {
    pub fn new(rgb: [u8; 3], selected: bool) -> Self {
        Self {
            rgb,
            selected,
            name: "",
        }
    }

    /// Name the colour for its tooltip and screen readers.
    #[must_use]
    pub fn named(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }
}

/// Which controls a [`SelectionHud`] offers besides the always-present Copy:
/// the highlight swatch row, and the Note and Delete controls when enabled.
#[derive(Clone, Copy, Debug)]
pub struct HudControls<'a> {
    pub swatches: &'a [HudSwatch],
    pub show_note: bool,
    pub show_delete: bool,
    /// A Select all button after Copy (and Note).
    pub show_select_all: bool,
    /// Copy and Select all carry their keyboard shortcuts, where a
    /// keyboard is how the reader works.
    pub shortcuts: bool,
}

/// Which side of the selection a [`SelectionHud`] prefers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HudSide {
    /// Above the selection's first line, below it only when there is no
    /// room (the default: the bar never covers what is being read next).
    #[default]
    Above,
    /// Below the selection's last line (a page whose own chrome sits above
    /// the text).
    Below,
}

/// Where a [`SelectionHud`] floats: `anchor` is the horizontal centre and
/// top-y of the selection's first band, `bounds` the area the bar must stay
/// inside, both in the drawing transform's space.
#[derive(Clone, Copy, Debug)]
pub struct HudPlacement {
    pub anchor: Point,
    pub bounds: Rect,
    pub side: HudSide,
    /// The bottom y of the selection's last band, for a bar placed below.
    /// `None` uses the anchor.
    pub below: Option<f64>,
    /// A rect the bar must not cover (a note card, a popover); it moves to
    /// the other side when it would.
    pub avoid: Option<Rect>,
}

impl HudPlacement {
    /// Above `anchor` (falling back below it), inside `bounds`.
    #[must_use]
    pub fn new(anchor: Point, bounds: Rect) -> Self {
        Self {
            anchor,
            bounds,
            side: HudSide::Above,
            below: None,
            avoid: None,
        }
    }

    /// Prefer the side below the selection, whose last line ends at
    /// `bottom`.
    #[must_use]
    pub fn below(mut self, bottom: f64) -> Self {
        self.side = HudSide::Below;
        self.below = Some(bottom);
        self
    }

    /// Stay clear of `rect`.
    #[must_use]
    pub fn avoiding(mut self, rect: Rect) -> Self {
        self.avoid = Some(rect);
        self
    }
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

/// The HUD's geometry, from the theme: a panel `control_inset` around one
/// row of Small controls, swatches a Small control square less the item
/// inset, `s1` between controls and a hairline between the groups.
struct HudMetrics {
    pad: f64,
    control_h: f64,
    swatch: f64,
    gap: f64,
    gap_from_anchor: f64,
}

impl HudMetrics {
    fn of(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let control_h = ControlSize::Small.height(theme);
        Self {
            pad: floating_surface::control_inset(theme),
            control_h,
            swatch: (control_h - f64::from(spacing.s2)).max(f64::from(spacing.s4)),
            gap: f64::from(spacing.s1),
            gap_from_anchor: f64::from(spacing.s2),
        }
    }

    fn height(&self) -> f64 {
        self.control_h + self.pad * 2.0
    }
}

/// The bar's rect: centred on the anchor, on the preferred side of the
/// selection, inside `bounds` by the panel margin; the other side when the
/// preferred one has no room or covers `avoid`.
fn place(m: &HudMetrics, width: f64, placement: HudPlacement) -> Rect {
    let HudPlacement {
        anchor,
        bounds,
        side,
        below,
        avoid,
    } = placement;
    let h = m.height();
    let margin = m.gap_from_anchor;
    let x0 = (anchor.x - width / 2.0).clamp(
        bounds.x0 + margin,
        (bounds.x1 - margin - width).max(bounds.x0 + margin),
    );
    let bottom = below.unwrap_or(anchor.y);
    let above_y = anchor.y - h - m.gap_from_anchor;
    let below_y = bottom + m.gap_from_anchor;
    let fits_above = above_y >= bounds.y0 + margin;
    let fits_below = below_y + h <= bounds.y1 - margin;
    let at = |y: f64| Rect::new(x0, y, x0 + width, y + h);
    let clear = |y: f64| avoid.is_none_or(|a| at(y).intersect(a).is_zero_area());
    let order = match side {
        HudSide::Above => [(above_y, fits_above), (below_y, fits_below)],
        HudSide::Below => [(below_y, fits_below), (above_y, fits_above)],
    };
    let y = order
        .iter()
        .find(|(y, fits)| *fits && clear(*y))
        .or_else(|| order.iter().find(|(_, fits)| *fits))
        .map_or_else(
            || below_y.min(bounds.y1 - margin - h).max(bounds.y0 + margin),
            |(y, _)| *y,
        );
    at(y)
}

/// A plain Small bar button, carrying `key`'s command shortcut when given.
fn hud_button<'a>(rect: Rect, label: &'a str, key: Option<&str>) -> ButtonFace<'a> {
    let face = ButtonFace::new(rect, label)
        .role(ButtonRole::Plain)
        .control_size(ControlSize::Small);
    match key {
        Some(key) => face.shortcut_hint(crate::platform::Shortcut::command(key)),
        None => face,
    }
}

impl SelectionHud {
    /// The bar's height in `theme`, so a reader can keep a slot for it.
    pub fn height(theme: &Theme) -> f64 {
        HudMetrics::of(theme).height()
    }

    /// The bar centres on `placement.anchor.x` on the preferred side of the
    /// selection, moving to the other side when that would leave
    /// `placement.bounds` or cover `placement.avoid`. `cursor` drives the
    /// hover treatment.
    pub fn draw(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        controls: &HudControls<'_>,
        placement: HudPlacement,
        cursor: Point,
    ) -> Self {
        let m = HudMetrics::of(theme);
        let on_panel = theme.on_ground(crate::chrome::panel_ground(theme));
        let face = |rect: Rect, label: &'static str, key: Option<&'static str>| {
            hud_button(rect, label, key.filter(|_| controls.shortcuts))
        };
        let copy_w = face(Rect::ZERO, "Copy", Some("C")).intrinsic_width(tr, &on_panel);
        let note_w = face(Rect::ZERO, "Note", None).intrinsic_width(tr, &on_panel);
        let all_w = face(Rect::ZERO, "Select all", Some("A")).intrinsic_width(tr, &on_panel);
        let swatches = controls.swatches;
        #[allow(clippy::cast_precision_loss, reason = "a handful of highlight colours")]
        let n = swatches.len() as f64;
        let swatch_cell = m.control_h;
        let dots_w = n * swatch_cell;
        let sep = if swatches.is_empty() {
            0.0
        } else {
            m.gap * 2.0 + 1.0
        };
        let note_extra = if controls.show_note {
            m.gap + note_w
        } else {
            0.0
        };
        let trash_extra = if controls.show_delete {
            m.gap + m.control_h
        } else {
            0.0
        };
        let all_extra = if controls.show_select_all {
            m.gap + all_w
        } else {
            0.0
        };
        let width = m.pad * 2.0 + dots_w + sep + copy_w + note_extra + all_extra + trash_extra;
        let rect = place(&m, width, placement);
        let mut hud = SelectionHud {
            rect,
            shown: true,
            ..SelectionHud::default()
        };
        floating_surface::paint(scene, transform, rect, theme);

        let row = Rect::new(
            rect.x0 + m.pad,
            rect.y0 + m.pad,
            rect.x1 - m.pad,
            rect.y1 - m.pad,
        );
        let mut x = row.x0;
        for (i, sw) in swatches.iter().enumerate() {
            let cell = Rect::new(x, row.y0, x + swatch_cell, row.y1);
            let inset = (swatch_cell - m.swatch) / 2.0;
            let dot = cell.inset(-inset);
            let name = if sw.name.is_empty() {
                "Highlight colour"
            } else {
                sw.name
            };
            ColorSwatchFace::new(dot, Color::from_rgb8(sw.rgb[0], sw.rgb[1], sw.rgb[2]), name)
                .selected(sw.selected)
                .hovered(cell.contains(cursor))
                .target_rect(cell)
                .paint(scene, transform, tr, &on_panel);
            hud.buttons.push((cell, HudAction::Highlight(i)));
            x += swatch_cell;
        }
        if !swatches.is_empty() {
            let div_x = x + m.gap + 0.5;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                crate::chrome::chrome_hairline(&on_panel),
                None,
                &Line::new(
                    Point::new(div_x, row.y0 + m.gap),
                    Point::new(div_x, row.y1 - m.gap),
                ),
            );
            x += sep;
        }
        let place_button = |x: &mut f64, w: f64| {
            let r = Rect::new(*x, row.y0, *x + w, row.y1);
            *x += w + m.gap;
            r
        };
        let copy_rect = place_button(&mut x, copy_w);
        let note_rect = controls.show_note.then(|| place_button(&mut x, note_w));
        let all_rect = controls
            .show_select_all
            .then(|| place_button(&mut x, all_w));
        let trash_rect = controls
            .show_delete
            .then(|| place_button(&mut x, m.control_h));
        let copy_face = face(copy_rect, "Copy", Some("C")).hovered(copy_rect.contains(cursor));
        Button::paint(scene, transform, tr, &on_panel, &copy_face);
        hud.buttons.push((copy_rect, HudAction::Copy));
        if let Some(r) = note_rect {
            let note_face = face(r, "Note", None).hovered(r.contains(cursor));
            Button::paint(scene, transform, tr, &on_panel, &note_face);
            hud.buttons.push((r, HudAction::Note));
        }
        if let Some(r) = all_rect {
            let all_face = face(r, "Select all", Some("A")).hovered(r.contains(cursor));
            Button::paint(scene, transform, tr, &on_panel, &all_face);
            hud.buttons.push((r, HudAction::SelectAll));
        }
        if let Some(r) = trash_rect {
            let face = ButtonFace::icon_only(r, Icon::Trash2, "Delete highlight")
                .role(ButtonRole::Destructive)
                .control_size(ControlSize::Small)
                .hovered(r.contains(cursor));
            Button::paint(scene, transform, tr, &on_panel, &face);
            hud.buttons.push((r, HudAction::Delete));
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

#[cfg(test)]
mod tests {
    use super::*;

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
            &crate::theme::Theme::dark(),
            &HudControls {
                swatches: &swatches(),
                show_note,
                show_delete,
                show_select_all: false,
                shortcuts: false,
            },
            HudPlacement::new(anchor, bounds),
            Point::new(-1000.0, -1000.0),
        )
    }

    #[test]
    fn hud_layout_places_every_control_inside_bounds() {
        {
            let hud = draw_hud(
                true,
                false,
                Point::new(400.0, 300.0),
                Rect::new(0.0, 0.0, 800.0, 600.0),
            );
            assert!(hud.is_shown());
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

        {
            let hud = draw_hud(
                true,
                true,
                Point::new(400.0, 300.0),
                Rect::new(0.0, 0.0, 800.0, 600.0),
            );
            assert!(hud.buttons().iter().any(|(_, a)| *a == HudAction::Delete));
        }

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
    fn hud_prefers_its_side_and_steps_aside() {
        let theme = crate::theme::Theme::dark();
        let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
        let draw = |placement: HudPlacement| {
            SelectionHud::draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                &HudControls {
                    swatches: &swatches(),
                    show_note: true,
                    show_delete: true,
                    show_select_all: false,
                    shortcuts: false,
                },
                placement,
                Point::new(-1000.0, -1000.0),
            )
        };
        let anchor = Point::new(400.0, 300.0);
        let above = draw(HudPlacement::new(anchor, bounds));
        assert!(above.rect().y1 <= anchor.y, "above by default");
        let below = draw(HudPlacement::new(anchor, bounds).below(320.0));
        assert!(below.rect().y0 >= 320.0, "below the last line when asked");
        let note = Rect::new(0.0, 200.0, 800.0, 300.0);
        let aside = draw(HudPlacement::new(anchor, bounds).avoiding(note));
        assert!(
            aside.rect().intersect(note).is_zero_area(),
            "the bar steps off the rect it must not cover: {:?}",
            aside.rect()
        );
        let inset = crate::widget::floating_surface::control_inset(&theme);
        for (rect, _) in above.buttons() {
            assert!(rect.y0 >= above.rect().y0 + inset - 0.01);
            assert!(rect.y1 <= above.rect().y1 - inset + 0.01);
        }
    }

    #[test]
    fn hud_hits_controls_and_latches_marker_drags() {
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

    #[test]
    fn select_all_follows_copy_and_shortcuts_widen_the_bar_only_for_a_keyboard() {
        let draw = |shortcuts: bool| {
            SelectionHud::draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &crate::theme::Theme::dark(),
                &HudControls {
                    swatches: &swatches(),
                    show_note: true,
                    show_delete: false,
                    show_select_all: true,
                    shortcuts,
                },
                HudPlacement::new(Point::new(400.0, 300.0), Rect::new(0.0, 0.0, 1200.0, 800.0)),
                Point::new(-1000.0, -1000.0),
            )
        };
        let touch = draw(false);
        let actions: Vec<HudAction> = touch.buttons().iter().map(|(_, a)| *a).collect();
        let copy = actions.iter().position(|a| *a == HudAction::Copy).unwrap();
        let all = actions
            .iter()
            .position(|a| *a == HudAction::SelectAll)
            .unwrap();
        assert!(all > copy, "Select all follows Copy: {actions:?}");
        assert!(
            draw(true).rect().width() > touch.rect().width(),
            "the shortcuts take room where they show"
        );
    }
}
