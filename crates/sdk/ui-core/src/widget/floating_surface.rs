//! The one body of every floating Panel: popovers, menus, modal sheets and
//! toasts.
//!
//! A floating Panel is the panel radius (`radii.panel`), the blurred
//! material with its opaque fallback, the derived panel edge plus the 1 px
//! inner light edge on a dark material, and one soft shadow
//! (`elevation.floating`). The overlays differ in placement and
//! interaction; they never differ in this.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Shape};
use vello::peniko::{Color, Fill};

use crate::theme::Theme;

/// The corner radius of a floating Panel.
#[must_use]
pub fn radius(theme: &Theme) -> f64 {
    f64::from(theme.tokens.radii.panel)
}

/// How far a floating Panel's shadow reaches past its rect on any side:
/// what a host repaints around a panel that appears, moves or changes.
#[must_use]
pub fn shadow_reach(theme: &Theme) -> f64 {
    let elevation = &theme.tokens.elevation.floating;
    f64::from(elevation.blur) * 2.0 + f64::from(elevation.offset_y).abs()
}

/// Every pixel a floating Panel in `rect` paints, its shadow included: what
/// a host repaints where one appears, changes or goes. Tighter than
/// [`shadow_reach`] on every side, as it follows the shadow's real kernel
/// and offset ([`crate::surface::shadow_bounds`]).
#[must_use]
pub fn painted_bounds(rect: Rect, theme: &Theme) -> Rect {
    crate::surface::shadow_bounds(rect, &theme.tokens.elevation.floating)
        .map_or(rect, |shadow| shadow.union(rect))
}

/// Paint a floating Panel's body in `rect`: shadow, material, edge. Content
/// is drawn on top by the caller.
pub fn paint(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    paint_rounded(scene, transform, rect, radius(theme), theme);
}

/// The radius of a menu panel holding a single row: the row's item radius
/// plus the menu inset, concentric with the row, so one row never sits in
/// a near-pill.
#[must_use]
pub fn single_row_radius(theme: &Theme) -> f64 {
    let radii = &theme.tokens.radii;
    (f64::from(radii.item) + menu_inset(theme)).min(f64::from(radii.panel))
}

/// [`paint`] with an explicit corner `radius` (a single-row menu's
/// [`single_row_radius`]).
pub fn paint_rounded(scene: &mut Scene, transform: Affine, rect: Rect, radius: f64, theme: &Theme) {
    // A blurred filled shape also covers its interior. Keep that shadow
    // outside the panel: the material below records this scene for blur,
    // and must not sample its own shadow as a dimmed foreground.
    let elevation = &theme.tokens.elevation.floating;
    let reach = shadow_reach(theme);
    let mut exterior = rect.inflate(reach, reach).to_path(0.1);
    exterior.extend(RoundedRect::from_rect(rect, radius).path_elements(0.1));
    scene.push_clip_layer(Fill::EvenOdd, transform, &exterior);
    crate::surface::shadow(scene, transform, rect, radius, elevation);
    scene.pop_layer();
    crate::surface::panel(scene, transform, rect, radius, theme);
}

/// How far a menu's rows sit inside its Panel, on every side: the `s2`
/// step. Popover menus, dropdown lists and context menus share it, so the
/// armed row's corners run concentric with the panel in all of them.
#[must_use]
pub fn menu_inset(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s2)
}

/// How many surface steps the armed row of a menu stands off its panel.
///
/// A selected row is a lift (`docs/design-language.md`, "No brand accent";
/// HIG §9.3). On a floating panel one step measured ΔL* ≈ 11 and read as
/// "no highlighting" on a dark desktop, so a panel takes a stronger lift
/// step: half a step more, which clears ΔL* 15 on every material
/// (`tests/menu_highlight_render.rs`) while staying a quiet grey on white,
/// like the sidebar lift.
pub const OVERLAY_LIFT: f32 = 1.5;

/// The fill of a menu's armed row (the row Enter or a click activates) and
/// the ink on it: the panel ground's overlay lift ([`OVERLAY_LIFT`]) with
/// primary ink measured on it. Hue-free under the default emphasis.
#[must_use]
pub fn armed_row(theme: &Theme) -> (Color, Color) {
    let colors = &theme.tokens.colors;
    let fill = colors.lift_on(crate::chrome::panel_ground(theme), OVERLAY_LIFT);
    let ink = colors.ink_on_any(
        &[fill],
        colors.text_primary,
        crate::tokens::colors::ON_FILL_MIN_CONTRAST,
    );
    (fill, ink)
}

/// Paint the armed row of a menu: the overlay lift of [`armed_row`],
/// concentric inside the panel (HIG §9.3). `row` is the row already inset
/// by `inset` from the panel's edge ([`menu_inset`] for a menu built on
/// it). Returns the ink for the row's label and glyphs.
pub fn paint_armed_row(
    scene: &mut Scene,
    transform: Affine,
    row: Rect,
    inset: f64,
    theme: &Theme,
) -> Color {
    paint_armed_row_in(scene, transform, row, radius(theme), inset, theme)
}

/// [`paint_armed_row`] inside a panel of `panel_radius` (a single-row
/// menu's [`single_row_radius`]).
pub fn paint_armed_row_in(
    scene: &mut Scene,
    transform: Affine,
    row: Rect,
    panel_radius: f64,
    inset: f64,
    theme: &Theme,
) -> Color {
    let radii = &theme.tokens.radii;
    let radius = f64::from(radii.concentric(
        crate::num::f32_of_f64(panel_radius),
        crate::num::f32_of_f64(inset),
    ))
    .min(row.height() / 2.0);

    let (fill, ink) = armed_row(theme);
    scene.fill(
        Fill::NonZero,
        transform,
        fill,
        None,
        &vello::kurbo::RoundedRect::from_rect(row, radius),
    );
    ink
}

/// How far every anchored floating Panel (popovers, menus, dropdown lists)
/// stays from the window's edges: the `s2` step, the menu's own inset, so
/// a panel pushed against an edge keeps the same air as its rows.
#[must_use]
pub fn viewport_margin(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s2)
}

/// The room an anchored floating Panel is placed in: `window` less the
/// [`viewport_margin`] on every side.
#[must_use]
pub fn panel_room(window: Rect, theme: &Theme) -> Rect {
    let margin = viewport_margin(theme)
        .min(window.width() / 4.0)
        .min(window.height() / 4.0)
        .max(0.0);
    window.inset(-margin)
}

/// The inset of a control inside a floating Panel that holds a row of
/// controls (a media control bar, a prompt): the panel radius less the
/// control radius, so the control's corners run concentric with the
/// panel's.
#[must_use]
pub fn control_inset(theme: &Theme) -> f64 {
    let radii = &theme.tokens.radii;
    f64::from((radii.panel - radii.control).max(0.0))
}

/// Which edge of its anchor a floating Panel lines up with on the cross
/// axis.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PanelAlign {
    /// The panel's leading edge on the anchor's leading edge (a menu under
    /// a toolbar button at the start of a row).
    #[default]
    Leading,
    /// The panel's trailing edge on the anchor's trailing edge (a menu under
    /// a button at the end of a header row).
    Trailing,
}

/// Which side of its anchor a floating Panel opens on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PopoverSide {
    Above,
    #[default]
    Below,
    Left,
    Right,
}

impl PopoverSide {
    #[must_use]
    pub fn opposite(self) -> Self {
        match self {
            Self::Above => Self::Below,
            Self::Below => Self::Above,
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    fn vertical(self) -> bool {
        matches!(self, Self::Above | Self::Below)
    }
}

/// The room a Panel has on `side` of `anchor`, `gap` away, inside
/// `viewport`: the most a menu opening there can grow before it scrolls.
#[must_use]
pub fn anchored_room(anchor: Rect, side: PopoverSide, gap: f64, viewport: Rect) -> f64 {
    let anchor = clamp_anchor(anchor, viewport);
    match side {
        PopoverSide::Below => viewport.y1 - anchor.y1 - gap,
        PopoverSide::Above => anchor.y0 - gap - viewport.y0,
        PopoverSide::Right => viewport.x1 - anchor.x1 - gap,
        PopoverSide::Left => anchor.x0 - gap - viewport.x0,
    }
    .max(0.0)
}

/// `anchor` moved inside `viewport`.
fn clamp_anchor(anchor: Rect, viewport: Rect) -> Rect {
    let x = |v: f64| v.clamp(viewport.x0, viewport.x1.max(viewport.x0));
    let y = |v: f64| v.clamp(viewport.y0, viewport.y1.max(viewport.y0));
    Rect::new(x(anchor.x0), y(anchor.y0), x(anchor.x1), y(anchor.y1))
}

/// Where a floating Panel of `size` goes against `anchor` (the control that
/// opened it, or a zero-size pointer position) inside `viewport`: on `side`
/// of the anchor, `gap` away, its leading edge on the anchor's leading
/// edge. It **flips** to the other side when `side` has no room and the
/// other side has more, flips its alignment to the anchor's trailing edge
/// when it would run past the viewport's trailing edge, and then **shifts**
/// along the anchor into the viewport. Returns the panel and the side it
/// opened on.
///
/// Every anchored floating Panel (context menus, popovers, dropdown lists)
/// places itself through here, so a menu always opens at its trigger.
#[must_use]
pub fn place_anchored(
    anchor: Rect,
    size: (f64, f64),
    side: PopoverSide,
    gap: f64,
    viewport: Rect,
) -> (Rect, PopoverSide) {
    place_aligned(anchor, size, side, PanelAlign::Leading, gap, viewport)
}

/// [`place_anchored`] with the cross-axis edge it lines up with: a panel
/// aligned [`PanelAlign::Trailing`] hangs from the anchor's trailing edge
/// and flips to the leading edge only when it would run past the
/// viewport's leading edge.
#[must_use]
pub fn place_aligned(
    anchor: Rect,
    size: (f64, f64),
    side: PopoverSide,
    align: PanelAlign,
    gap: f64,
    viewport: Rect,
) -> (Rect, PopoverSide) {
    if align == PanelAlign::Trailing {
        // Mirror the cross axis, place leading, and mirror back.
        let (mirror_x, mirror_y) = (side.vertical(), !side.vertical());
        let flip = |r: Rect| {
            let (x0, x1) = if mirror_x {
                (-r.x1, -r.x0)
            } else {
                (r.x0, r.x1)
            };
            let (y0, y1) = if mirror_y {
                (-r.y1, -r.y0)
            } else {
                (r.y0, r.y1)
            };
            Rect::new(x0, y0, x1, y1)
        };
        let (rect, side) = place_aligned(
            flip(anchor),
            size,
            side,
            PanelAlign::Leading,
            gap,
            flip(viewport),
        );
        return (flip(rect), side);
    }
    let (w, h) = (size.0.max(0.0), size.1.max(0.0));
    // An anchor past the viewport's edge (a pointer outside it) opens the
    // panel from the nearest point on the edge.
    let anchor = clamp_anchor(anchor, viewport);
    let room = |side: PopoverSide| anchored_room(anchor, side, gap, viewport);
    let need = if side.vertical() { h } else { w };
    let side = if room(side) >= need || room(side) >= room(side.opposite()) {
        side
    } else {
        side.opposite()
    };
    // Cross axis: leading-aligned with the anchor, or trailing-aligned when
    // the leading alignment runs past the viewport and that has more room.
    let cross = |a0: f64, a1: f64, len: f64, v0: f64, v1: f64| {
        if a0 + len <= v1 || (a1 - len < v0 && v1 - a0 >= a1 - v0) {
            a0
        } else {
            a1 - len
        }
    };
    let (x0, y0) = match side {
        PopoverSide::Below => (
            cross(anchor.x0, anchor.x1, w, viewport.x0, viewport.x1),
            anchor.y1 + gap,
        ),
        PopoverSide::Above => (
            cross(anchor.x0, anchor.x1, w, viewport.x0, viewport.x1),
            anchor.y0 - gap - h,
        ),
        PopoverSide::Right => (
            anchor.x1 + gap,
            cross(anchor.y0, anchor.y1, h, viewport.y0, viewport.y1),
        ),
        PopoverSide::Left => (
            anchor.x0 - gap - w,
            cross(anchor.y0, anchor.y1, h, viewport.y0, viewport.y1),
        ),
    };
    // Shift into the viewport on the cross axis only: on the main axis the
    // panel never slides over its own anchor. A caller whose panel can be
    // taller than either side's room caps it with [`anchored_room`].
    let (x0, y0) = if side.vertical() {
        (x0.min(viewport.x1 - w).max(viewport.x0), y0)
    } else {
        (x0, y0.min(viewport.y1 - h).max(viewport.y0))
    };
    (Rect::new(x0, y0, x0 + w, y0 + h), side)
}

/// One control of a [`ControlBar`]: an icon button over media, named for
/// assistive technology and its tooltip.
#[derive(Clone, Copy, Debug)]
pub struct BarControl<'a> {
    pub icon: crate::icons::Icon,
    pub name: &'a str,
    /// A toggle that is on (the grid overlay, the mute) shows the lift.
    pub toggled: bool,
    pub enabled: bool,
}

impl<'a> BarControl<'a> {
    #[must_use]
    pub fn new(icon: crate::icons::Icon, name: &'a str) -> Self {
        Self {
            icon,
            name,
            toggled: false,
            enabled: true,
        }
    }

    #[must_use]
    pub fn toggled(mut self, on: bool) -> Self {
        self.toggled = on;
        self
    }

    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// The floating bar of controls over media (a camera's capture controls, a
/// player's transport): one floating Panel shaped to a row of Regular icon
/// buttons, `control_inset` inside it so the buttons' corners run
/// concentric with the panel's, `s1` between them. It owns the panel, the
/// gaps, the hover and press faces and the hit rects; the host says which
/// controls stand in it and where, and acts on the index it reports.
#[derive(Clone, Debug, Default)]
pub struct ControlBar {
    rect: Rect,
    controls: Vec<Rect>,
    enabled: Vec<bool>,
    cursor: Option<Point>,
    pressed: Option<usize>,
}

impl ControlBar {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The size of a bar of `count` controls.
    #[must_use]
    pub fn size(count: usize, theme: &Theme) -> (f64, f64) {
        let control = crate::widget::ControlSize::Regular.height(theme);
        let gap = f64::from(theme.tokens.spacing.s1);
        let inset = control_inset(theme);
        #[allow(
            clippy::cast_precision_loss,
            reason = "a control bar holds a handful of controls"
        )]
        let n = count.max(1) as f64;
        (
            n * control + (n - 1.0) * gap + inset * 2.0,
            control + inset * 2.0,
        )
    }

    /// Where a bar of `count` controls stands in `area`: centred, one
    /// content gutter above its bottom edge.
    #[must_use]
    pub fn bottom_centre(area: Rect, count: usize, theme: &Theme) -> Rect {
        let (w, h) = Self::size(count, theme);
        let gutter = f64::from(theme.tokens.spacing.s4);
        let x0 = area.center().x - w / 2.0;
        let y1 = area.y1 - gutter;
        Rect::new(x0, y1 - h, x0 + w, y1)
    }

    /// Paint the bar with `controls` at `rect` (from [`Self::size`] or
    /// [`Self::bottom_centre`]), and keep their hit rects.
    pub fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &Theme,
        rect: Rect,
        controls: &[BarControl<'_>],
    ) {
        self.rect = rect;
        self.controls.clear();
        self.enabled = controls.iter().map(|c| c.enabled).collect();
        paint(scene, transform, rect, theme);
        let on_panel = theme.on_ground(crate::chrome::panel_ground(theme));
        let side = crate::widget::ControlSize::Regular.height(theme);
        let gap = f64::from(theme.tokens.spacing.s1);
        let inset = control_inset(theme);
        let mut x = rect.x0 + inset;
        let y = rect.y0 + inset;
        for (i, control) in controls.iter().enumerate() {
            let r = Rect::new(x, y, x + side, y + side);
            let over = self.cursor.is_some_and(|p| r.contains(p));
            let face = crate::widget::ButtonFace::icon_only(r, control.icon, control.name)
                .role(crate::widget::ButtonRole::Plain)
                .toggled(control.toggled)
                .disabled(!control.enabled)
                .hovered(over && control.enabled)
                .pressed(over && control.enabled && self.pressed == Some(i));
            crate::widget::Button::paint(scene, transform, text, &on_panel, &face);
            self.controls.push(r);
            x += side + gap;
        }
    }

    /// The bar's rect at the last paint.
    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Control `index`'s rect at the last paint.
    #[must_use]
    pub fn control_rect(&self, index: usize) -> Option<Rect> {
        self.controls.get(index).copied()
    }

    /// The control under `point`, if any.
    #[must_use]
    pub fn hit(&self, point: Point) -> Option<usize> {
        self.controls.iter().position(|r| r.contains(point))
    }

    /// Whether `point` is on the bar (a press there is the bar's).
    #[must_use]
    pub fn contains(&self, point: Point) -> bool {
        self.rect.contains(point)
    }

    /// Route `event`; returns the control a click activated. A press on
    /// the bar but between controls is still the bar's: check
    /// [`Self::contains`] to swallow it.
    pub fn handle(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
    ) -> Option<usize> {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Some(Point::new(
                    position.x / scale_factor,
                    position.y / scale_factor,
                ));
                None
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor = None;
                self.pressed = None;
                None
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.pressed = self
                    .cursor
                    .and_then(|p| self.hit(p))
                    .filter(|&i| self.enabled.get(i).copied().unwrap_or(false));
                None
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let pressed = self.pressed.take()?;
                (self.cursor.and_then(|p| self.hit(p)) == Some(pressed)).then_some(pressed)
            }
            _ => None,
        }
    }
}

/// A Panel's content laid out in its own tree: a floating Panel is placed
/// against an anchor or the window, never by its parent's flexbox, so its
/// content has no parent tree to be laid out in. The tree is kept so events
/// reach the content with a valid node.
pub(crate) struct DetachedContent {
    pub taffy: taffy::TaffyTree<()>,
    pub node: taffy::NodeId,
}

impl DetachedContent {
    /// Lay `content` out in `available` with the production solver.
    pub(crate) fn lay_out(
        content: &mut dyn crate::widget::Widget,
        theme: &Theme,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut crate::text_render::TextRenderer,
    ) -> Self {
        let mut taffy = taffy::TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        let node = content.build_layout(&mut taffy, theme, &mut nodes);
        let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
        // SAFETY: `nodes` describes the tree just built from `content`, which
        // the caller holds exclusively for the whole call; no pointer escapes.
        let _ = unsafe {
            bindings.rebuild(content, &taffy, node, &nodes);
            crate::widget::compute_widget_layout(
                &mut taffy,
                node,
                available,
                &nodes,
                &bindings,
                text_renderer,
            )
        };
        Self { taffy, node }
    }

    /// Hand `event` to a floating Panel's `content`, in the host's tree at
    /// `hosted` or else in `detached`'s own tree. Reports whether the content
    /// handled it, while leaving `ctx.handled` set if it already was.
    pub(crate) fn forward(
        content: &mut dyn crate::widget::Widget,
        hosted: Option<taffy::NodeId>,
        detached: Option<&Self>,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        let before = std::mem::replace(ctx.handled, false);
        if let Some(node) = hosted {
            content.handle_event(event, &mut ctx.reborrow(ctx.path, node));
        } else if let Some(detached) = detached {
            // A node of the Panel's own tree means nothing to the host; the
            // content keeps its own focus.
            let mut detached_focus = None;
            content.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: ctx.modifiers,
                    state: ctx.state.reborrow(),
                    path: ctx.path,
                    handled: ctx.handled,
                    focus_target: &mut detached_focus,
                    node_id: detached.node,
                    taffy: &detached.taffy,
                },
            );
        }
        let consumed = *ctx.handled;
        *ctx.handled = before || consumed;
        consumed
    }

    /// The laid-out size of the content's root.
    pub(crate) fn size(&self) -> (f64, f64) {
        self.taffy.layout(self.node).map_or((0.0, 0.0), |l| {
            (f64::from(l.size.width), f64::from(l.size.height))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CIE L* of a colour, from the crate's WCAG luminance.
    fn lightness(c: Color) -> f64 {
        let y = crate::a11y::relative_luminance(c);
        let f = if y > 216.0 / 24389.0 {
            y.cbrt()
        } else {
            (841.0 / 108.0) * y + 4.0 / 29.0
        };
        116.0 * f - 16.0
    }

    #[test]
    fn the_armed_row_is_a_lift_that_clears_its_floor_on_every_material() {
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("shipped preset resolves");
            let colors = &theme.tokens.colors;
            let ground = crate::chrome::panel_ground(&theme);
            let (fill, ink) = armed_row(&theme);
            let delta = (lightness(fill) - lightness(ground)).abs();
            // The armed row's floor: CIE ΔL* 15 off its panel.
            assert!(
                delta >= 15.0,
                "{name}: the armed row stands only ΔL* {delta:.1} off its panel"
            );
            // A lift, not an inverted slab: the list lift points the same
            // way and the overlay step is the stronger of the two.
            let list = colors.lift_on(ground, 1.0);
            let list_delta = (lightness(list) - lightness(ground)).abs();
            assert!(
                delta > list_delta,
                "{name}: the overlay lift ({delta:.1}) is stronger than a list lift ({list_delta:.1})"
            );
            assert_ne!(
                fill, colors.intent.inverted,
                "{name}: not the inverted fill"
            );
            assert!(
                crate::a11y::contrast_ratio(ink, fill)
                    >= crate::tokens::colors::ON_FILL_MIN_CONTRAST,
                "{name}: the armed row's label is legible on it"
            );
        }
    }

    const WINDOW: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

    #[test]
    fn an_anchored_panel_opens_at_its_trigger_and_flips_and_shifts_at_the_edges() {
        // Below the trigger, leading edges aligned.
        let button = Rect::new(200.0, 16.0, 232.0, 48.0);
        let (rect, side) = place_anchored(button, (160.0, 240.0), PopoverSide::Below, 4.0, WINDOW);
        assert_eq!(side, PopoverSide::Below);
        assert_eq!((rect.x0, rect.y0), (200.0, 52.0));

        // A header button at the trailing edge: the menu's trailing edge
        // lines up with the button's instead of being pushed off it.
        let more = Rect::new(1088.0, 16.0, 1112.0, 40.0);
        let (rect, _) = place_anchored(more, (220.0, 240.0), PopoverSide::Below, 4.0, WINDOW);
        assert!(
            (rect.x1 - more.x1).abs() < 1e-9,
            "trailing-aligned with its trigger"
        );
        assert!(rect.y0 >= more.y1, "below its trigger, not over it");

        // No room below: flips above.
        let low = Rect::new(400.0, 740.0, 480.0, 772.0);
        let (rect, side) = place_anchored(low, (160.0, 240.0), PopoverSide::Below, 4.0, WINDOW);
        assert_eq!(side, PopoverSide::Above);
        assert!((rect.y1 - (low.y0 - 4.0)).abs() < 1e-9);

        // A pointer in the bottom-trailing corner: flips up and to the
        // pointer's leading side, and stays in the window.
        let point = Rect::new(1270.0, 790.0, 1270.0, 790.0);
        let (rect, _) = place_anchored(point, (200.0, 300.0), PopoverSide::Below, 0.0, WINDOW);
        assert!(rect.x0 >= WINDOW.x0 && rect.y0 >= WINDOW.y0);
        assert!(rect.x1 <= WINDOW.x1 && rect.y1 <= WINDOW.y1);
        assert!((rect.x1 - 1270.0).abs() < 1e-9 && (rect.y1 - 790.0).abs() < 1e-9);

        // Wider than the room either way: shifted into the window.
        let middle = Rect::new(600.0, 100.0, 640.0, 120.0);
        let (rect, _) = place_anchored(middle, (1270.0, 100.0), PopoverSide::Below, 4.0, WINDOW);
        assert!(rect.x0 >= WINDOW.x0 && rect.x1 <= WINDOW.x1);
    }
}

#[cfg(test)]
mod control_bar_tests {
    use super::*;
    use crate::icons::Icon;

    #[test]
    fn a_control_bar_is_concentric_and_reports_its_clicks() {
        let theme = Theme::dark();
        let inset = control_inset(&theme);
        assert!(
            (inset - f64::from(theme.tokens.radii.panel - theme.tokens.radii.control)).abs() < 1e-9
        );
        let area = Rect::new(0.0, 0.0, 800.0, 600.0);
        let rect = ControlBar::bottom_centre(area, 3, &theme);
        assert!((rect.center().x - 400.0).abs() < 1e-9);
        assert!(rect.height() >= 36.0, "a single row never reads as a pill");
        let mut bar = ControlBar::new();
        let controls = [
            BarControl::new(Icon::Camera, "Take photo"),
            BarControl::new(Icon::Mic, "Record sound").toggled(true),
            BarControl::new(Icon::Settings, "Settings").enabled(false),
        ];
        bar.paint(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut crate::text_render::TextRenderer::new(),
            &theme,
            rect,
            &controls,
        );
        let first = bar.control_rect(0).unwrap();
        assert!((first.x0 - (rect.x0 + inset)).abs() < 1e-9);
        assert!((first.y0 - (rect.y0 + inset)).abs() < 1e-9);
        let click = |bar: &mut ControlBar, p: Point| {
            bar.handle(&crate::synthetic_input::cursor_moved(p.x, p.y), 1.0);
            bar.handle(&crate::synthetic_input::mouse_press(), 1.0);
            bar.handle(&crate::synthetic_input::mouse_release(), 1.0)
        };
        assert_eq!(click(&mut bar, first.center()), Some(0));
        let disabled = bar.control_rect(2).unwrap();
        assert_eq!(
            click(&mut bar, disabled.center()),
            None,
            "a disabled control is inert"
        );
        assert!(bar.contains(rect.center()));
    }

    #[test]
    fn trailing_alignment_mirrors_leading() {
        let anchor = Rect::new(600.0, 10.0, 640.0, 42.0);
        let window = Rect::new(0.0, 0.0, 800.0, 600.0);
        let (rect, side) = place_aligned(
            anchor,
            (200.0, 100.0),
            PopoverSide::Below,
            PanelAlign::Trailing,
            4.0,
            window,
        );
        assert_eq!(side, PopoverSide::Below);
        assert!((rect.x1 - anchor.x1).abs() < 1e-9);
        assert!((rect.y0 - (anchor.y1 + 4.0)).abs() < 1e-9);
        let room = panel_room(window, &Theme::dark());
        assert!(
            room.x0 > window.x0 && room.x1 < window.x1,
            "panels keep a margin"
        );
    }
}
