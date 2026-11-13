//! Pointing at what is wrong: the window dims, the part under the pointer
//! lights up with its bounds, and the click that follows picks it for the
//! send-to box that opens beside it. Escape leaves without picking.
//!
//! What can be picked is read from the window's retained widget tree
//! ([`crate::pick`]), never from pixels: the person points at a control, a
//! row or a line, and the box says which one it is about.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Shape, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::floating_surface::Recipient;
use crate::shared_selection::SharedSelection;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;

/// How far a highlight stands off the part it outlines.
const OUTSET: f64 = 3.0;
const RADIUS: f64 = 6.0;
/// How much of the theme's modal scrim the window dims with.
const DIM_SHARE: f32 = 0.75;
/// The hint's distance from the window's top.
const HINT_TOP: f64 = 10.0;

/// A window asked to point at what a report is about.
pub(crate) struct Pointing {
    /// Whom the box the pick opens goes to, as the session named them.
    to: Vec<Recipient>,
    /// What lies under the pointer, outermost first and deepest last.
    chain: Vec<crate::pick::PickTarget>,
}

impl Pointing {
    pub(crate) fn new(to: Vec<Recipient>) -> Self {
        Self {
            to,
            chain: Vec::new(),
        }
    }

    /// The part under the pointer, as the highlight and the caption show it.
    fn lit(&self) -> Option<&crate::pick::PickTarget> {
        self.chain.last()
    }

    /// Where the pointer is now, in the window's logical pixels.
    pub(crate) fn hover(&mut self, root: &dyn crate::widget::Widget, at: Point) {
        self.chain = crate::pick::targets_at(root, at);
    }

    /// What the person pointed at, as the box's subject: their selection when
    /// they pointed inside it, else the part under the pointer.
    fn subject(&self, focused: Option<SharedSelection>) -> Option<SharedSelection> {
        let lit = self.lit();
        let at = lit.map(|lit| lit.rect.center());
        if let Some(selection) = crate::shared_selection::pointed_selection(focused, at) {
            return Some(selection);
        }
        lit.map(subject_of)
    }

    /// The window's whole answer to `event` while pointing: whether it took it.
    /// `focused` is what the window holds selected, which a pointer inside its
    /// bounds takes as the subject.
    pub(crate) fn event(
        &mut self,
        event: &WindowEvent,
        root: &dyn crate::widget::Widget,
        scale: f64,
        focused: Option<SharedSelection>,
    ) -> Taken {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.hover(root, Point::new(position.x / scale, position.y / scale));
                Taken::Redraw
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.subject(focused).map_or(Taken::Redraw, |about| {
                let bounds = about
                    .protocol_bounds()
                    .or_else(|| self.lit().map(|lit| rect_array(lit.rect)));
                Taken::Picked(about, bounds)
            }),
            WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => Taken::Left,
            _ => Taken::Ignored,
        }
    }

    /// Whom the box the pick opens goes to.
    pub(crate) fn to(&self) -> &[Recipient] {
        &self.to
    }

    /// Dim the window, light the part under the pointer with its name above
    /// it, and say what a click does.
    pub(crate) fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        bounds: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let lit = self.lit().map(|lit| lit.rect);
        let dim = colors
            .surfaces
            .scrim
            .multiply_alpha(colors.surfaces.scrim_strength * DIM_SHARE);
        let holes: Vec<Rect> = lit.into_iter().collect();
        scene.fill(Fill::EvenOdd, transform, dim, None, &dimmed(bounds, &holes));
        let ink = colors.intent.accent_ink;
        if let Some(rect) = lit {
            let outline = round(rect);
            scene.fill(
                Fill::NonZero,
                transform,
                ink.multiply_alpha(0.08),
                None,
                &outline,
            );
            scene.stroke(&Stroke::new(1.5), transform, ink, None, &outline);
            if let Some(target) = self.lit() {
                caption(
                    scene,
                    transform,
                    text,
                    theme,
                    rect,
                    bounds,
                    &describe(target),
                );
            }
        }
        hint(scene, transform, text, theme, bounds, lit);
    }
}

/// What pointing at `event` did.
pub(crate) enum Taken {
    /// Nothing of pointing's.
    Ignored,
    /// It changed what the point shows.
    Redraw,
    /// The person left without picking.
    Left,
    /// The person picked the thing the box is about, where it stands.
    Picked(SharedSelection, Option<[f64; 4]>),
}

/// What a part is called on the box's chip: what kind of thing the app says it
/// is ("Message"), else what it is called there ("Send message"), else its
/// role as assistive technology names it ("ListItem").
fn describe(target: &crate::pick::PickTarget) -> String {
    if let Some(kind) = target
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
    {
        return kind.to_owned();
    }
    if let Some(name) = target
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        return name.to_owned();
    }
    target.role_name()
}

fn subject_of(target: &crate::pick::PickTarget) -> SharedSelection {
    SharedSelection::of_text(&describe(target)).within(target.rect)
}

fn rect_array(rect: Rect) -> [f64; 4] {
    [rect.x0, rect.y0, rect.x1, rect.y1]
}

fn round(rect: Rect) -> RoundedRect {
    RoundedRect::from_rect(rect.inflate(OUTSET, OUTSET), RADIUS)
}

/// The window with a hole for each of `holes`; one inside another is left out,
/// since an even-odd fill would dim it again.
fn dimmed(bounds: Rect, holes: &[Rect]) -> BezPath {
    let mut path = bounds.to_path(0.1);
    for (index, hole) in holes.iter().enumerate() {
        let inside = holes.iter().enumerate().any(|(other, around)| {
            other != index && around.contains_rect(*hole) && (*around != *hole || other < index)
        });
        if !inside {
            path.extend(round(*hole).path_elements(0.1));
        }
    }
    path
}

/// What is lit, just above it (inside its top when it touches the window's
/// top).
fn caption(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    bounds: Rect,
    caption: &str,
) {
    let style = theme.text(TextRole::Label);
    let shown = text.elide_to_width_style(caption, 320.0, &style);
    let (w, h) = text.measure_style(&shown, None, &style);
    let (pad_x, pad_y) = (8.0, 4.0);
    let chip_w = f64::from(w) + 2.0 * pad_x;
    let chip_h = f64::from(h) + 2.0 * pad_y;
    let x = (rect.x0 - OUTSET).clamp(bounds.x0 + 4.0, (bounds.x1 - chip_w - 4.0).max(bounds.x0));
    let above = rect.y0 - OUTSET - chip_h - 4.0;
    let y = if above >= bounds.y0 + 4.0 {
        above
    } else {
        rect.y0 + OUTSET + 4.0
    };
    let chip = Rect::new(x, y, x + chip_w, y + chip_h);
    let on = crate::widget::paint_surface(
        scene,
        transform,
        chip,
        crate::widget::SurfaceLevel::Floating,
        theme,
    );
    text.draw_style(
        scene,
        &shown,
        on.tokens.colors.text_primary,
        transform.pre_translate((x + pad_x, y + pad_y).into()),
        None,
        &style,
    );
}

/// The calm line saying what to do, over the window's top, out of the way of
/// what is lit.
fn hint(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    bounds: Rect,
    lit: Option<Rect>,
) {
    let style = theme.text(TextRole::Label);
    let quiet = theme.text(TextRole::Caption);
    let room = (bounds.width() - 64.0).max(0.0) as f32;
    let line = crate::i18n::I18nManager::lookup_plain("send-to-point-hint");
    let leaving = crate::i18n::I18nManager::lookup_plain("send-to-point-escape");
    let shown = text.elide_to_width_style(&line, room, &style);
    let leaving = text.elide_to_width_style(&leaving, room, &quiet);
    let (w, h) = text.measure_style(&shown, None, &style);
    let (leaving_w, leaving_h) = text.measure_style(&leaving, None, &quiet);
    let (w, h, leaving_w, leaving_h) = (
        f64::from(w),
        f64::from(h),
        f64::from(leaving_w),
        f64::from(leaving_h),
    );
    let (pad, gap) = (14.0, 2.0);
    let pill_h = h + gap + leaving_h + 12.0;
    let pill_w = w.max(leaving_w) + 2.0 * pad;
    let x0 = bounds.center().x - pill_w / 2.0;
    let high = Rect::new(
        x0,
        bounds.y0 + HINT_TOP,
        x0 + pill_w,
        bounds.y0 + HINT_TOP + pill_h,
    );
    let pill = match lit {
        Some(rect) if rect.intersect(high.inflate(0.0, 12.0)).area() > 0.0 => Rect::new(
            x0,
            rect.y1 + 12.0 + OUTSET,
            x0 + pill_w,
            rect.y1 + 12.0 + OUTSET + pill_h,
        ),
        _ => high,
    };
    let on = crate::widget::paint_surface(
        scene,
        transform,
        pill,
        crate::widget::SurfaceLevel::Floating,
        theme,
    );
    text.draw_style(
        scene,
        &shown,
        on.tokens.colors.text_primary,
        transform.pre_translate((pill.x0 + pad, pill.y0 + 6.0).into()),
        None,
        &style,
    );
    text.draw_style(
        scene,
        &leaving,
        on.tokens.colors.text_dim,
        transform.pre_translate((pill.x0 + pad, pill.y0 + 6.0 + h + gap).into()),
        None,
        &quiet,
    );
}
