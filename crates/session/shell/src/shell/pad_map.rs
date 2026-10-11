//! What a tablet pad's controls do, shown while a mode button (or the
//! button the pad's settings pick) is held.
//!
//! Each button, ring and strip carries its current action: the label the
//! focused tablet-aware app gave it over `zwp_tablet_pad_v2.set_feedback`,
//! or else what Tilo maps it to. When libwacom knows where the controls
//! sit, the card arranges them around a drawing of the pad; otherwise it
//! lists them. Like the OSD it takes no input and never takes focus.

use settings_store::hardware::{PadDialAction, ShortcutAction};
use ui::num::f32_of_f64;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::InputHint;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};

use crate::shell::i18n::{t, t_args};
use ui::TextRole;

/// One control on a pad, by libinput's number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PadControl {
    Button(u32),
    Ring(u32),
    Strip(u32),
}

impl PadControl {
    /// "Button 1", "Ring 1", "Strip 1".
    pub fn label(self) -> String {
        let (key, index) = match self {
            Self::Button(index) => ("pad-map-button", index),
            Self::Ring(index) => ("pad-map-ring", index),
            Self::Strip(index) => ("pad-map-strip", index),
        };
        t_args(
            key,
            &[("number", i64::from(index).saturating_add(1).into())],
        )
    }

    /// What the keycap reads: a button's number, a dial's full name.
    fn cap(self) -> String {
        match self {
            Self::Button(index) => index.saturating_add(1).to_string(),
            Self::Ring(_) | Self::Strip(_) => self.label(),
        }
    }
}

/// An edge of the pad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadSide {
    Left,
    Right,
    Top,
    Bottom,
}

impl PadSide {
    pub const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];
}

/// The controls along each edge of a pad, top to bottom or left to right.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PadLayout {
    pub left: Vec<PadControl>,
    pub right: Vec<PadControl>,
    pub top: Vec<PadControl>,
    pub bottom: Vec<PadControl>,
}

impl PadLayout {
    pub fn side(&self, side: PadSide) -> &[PadControl] {
        match side {
            PadSide::Left => &self.left,
            PadSide::Right => &self.right,
            PadSide::Top => &self.top,
            PadSide::Bottom => &self.bottom,
        }
    }

    pub fn side_mut(&mut self, side: PadSide) -> &mut Vec<PadControl> {
        match side {
            PadSide::Left => &mut self.left,
            PadSide::Right => &mut self.right,
            PadSide::Top => &mut self.top,
            PadSide::Bottom => &mut self.bottom,
        }
    }

    pub fn side_of(&self, control: PadControl) -> Option<PadSide> {
        PadSide::ALL
            .into_iter()
            .find(|side| self.side(*side).contains(&control))
    }
}

/// What one control does right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PadMapAction {
    /// The focused tablet-aware app's own label for it.
    App(String),
    /// A shortcut the pad's settings map it to.
    Shortcut(ShortcutAction),
    /// What a ring or strip does in its current mode.
    Dial(PadDialAction),
    /// A mode button: its group is in `mode` (from zero) of `modes`.
    ModeSwitch {
        mode: u32,
        modes: u32,
    },
    /// The button that shows this map.
    ShowsMap,
    /// The focused app takes it, and has not said what it does.
    AppDecides,
    Nothing,
}

impl PadMapAction {
    pub fn label(&self) -> String {
        match self {
            Self::App(label) => label.clone(),
            Self::Shortcut(action) => t(&action.label_key()),
            Self::Dial(action) => t(match action {
                PadDialAction::Nothing => "pad-map-nothing",
                PadDialAction::Scroll => "pad-map-dial-scroll",
                PadDialAction::Zoom => "pad-map-dial-zoom",
                PadDialAction::BrushSize => "pad-map-dial-brush-size",
                PadDialAction::Volume => "pad-map-dial-volume",
                PadDialAction::Brightness => "pad-map-dial-brightness",
            }),
            Self::ModeSwitch { mode, modes } => t_args(
                "pad-map-mode-switch",
                &[
                    ("mode", i64::from(*mode).saturating_add(1).into()),
                    ("modes", i64::from(*modes).into()),
                ],
            ),
            Self::ShowsMap => t("pad-map-shows-map"),
            Self::AppDecides => t("pad-map-app-decides"),
            Self::Nothing => t("pad-map-nothing"),
        }
    }

    /// Whether the control does something worth reading first.
    fn is_active(&self) -> bool {
        !matches!(self, Self::Nothing | Self::AppDecides)
            && !matches!(self, Self::Dial(PadDialAction::Nothing))
    }
}

/// One control and what it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PadMapEntry {
    pub control: PadControl,
    pub action: PadMapAction,
}

/// Everything the card shows for one pad.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PadMap {
    /// The pad's name, as the device reports it.
    pub name: String,
    /// Where the controls sit, when libwacom knows the model.
    pub layout: Option<PadLayout>,
    /// Every control: buttons, then rings, then strips.
    pub entries: Vec<PadMapEntry>,
}

impl PadMap {
    pub fn action(&self, control: PadControl) -> Option<&PadMapAction> {
        self.entries
            .iter()
            .find(|entry| entry.control == control)
            .map(|entry| &entry.action)
    }

    /// The card as one line per control, for the screen reader.
    pub fn narration(&self) -> String {
        self.entries
            .iter()
            .map(|entry| {
                t_args(
                    "pad-map-entry",
                    &[
                        ("control", entry.control.label().into()),
                        ("action", entry.action.label().into()),
                    ],
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn entry(&self, control: PadControl) -> Option<&PadMapEntry> {
        self.entries.iter().find(|entry| entry.control == control)
    }

    /// Paint the card centred in `viewport`.
    pub fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
    ) {
        // Theme type tokens already carry the user's text scale (HIG §7a).
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        let metrics = Metrics::new(theme);
        let body = match &self.layout {
            Some(layout) => self.measure_layout(layout, text_renderer, theme, &metrics),
            None => self.measure_list(text_renderer, theme, &metrics),
        };
        let title = self.name.clone();
        let hint = t("pad-map-hint");
        let (_, title_h) =
            text_renderer.measure_weighted(&title, metrics.title, None, TextWeight::Semibold);
        let (_, hint_h) = text_renderer.measure(&hint, metrics.caption, None);
        let header_h = f64::from(title_h) + metrics.gap + f64::from(hint_h);
        let max_w = (viewport.width() - metrics.margin * 2.0).max(0.0);
        let max_h = (viewport.height() - metrics.margin * 2.0).max(0.0);
        let width = (body.width + metrics.inset * 2.0).clamp(0.0, max_w);
        let height = (header_h + metrics.section + body.height + metrics.inset * 2.0).min(max_h);
        let center = viewport.center();
        let card = Rect::new(
            center.x - width / 2.0,
            center.y - height / 2.0,
            center.x + width / 2.0,
            center.y + height / 2.0,
        );
        ui::chrome::shell::paint_panel(scene, transform, card, theme, 1.0);
        let colors = &theme.tokens.colors;
        let text_w = Some(f32_of_f64(card.width() - metrics.inset * 2.0));
        text_renderer.draw_text_weighted(
            scene,
            &title,
            metrics.title,
            colors.text_primary,
            transform.pre_translate((card.x0 + metrics.inset, card.y0 + metrics.inset).into()),
            text_w,
            TextWeight::Semibold,
        );
        text_renderer.draw_text(
            scene,
            &hint,
            metrics.caption,
            colors.text_dim,
            transform.pre_translate(
                (
                    card.x0 + metrics.inset,
                    card.y0 + metrics.inset + f64::from(title_h) + metrics.gap,
                )
                    .into(),
            ),
            text_w,
        );
        let origin = Point::new(
            card.x0 + metrics.inset,
            card.y0 + metrics.inset + header_h + metrics.section,
        );
        let clip = Rect::new(
            card.x0,
            origin.y,
            card.x1,
            card.y1 - metrics.inset.min(card.height()),
        );
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
        match &self.layout {
            Some(layout) => {
                self.draw_layout(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    &metrics,
                    layout,
                    origin,
                );
            }
            None => self.draw_list(scene, transform, text_renderer, theme, &metrics, origin),
        }
        scene.pop_layer();
        text_renderer.set_text_scale(renderer_scale);
    }

    fn measure_list(
        &self,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        metrics: &Metrics,
    ) -> Size {
        let width = self
            .entries
            .iter()
            .map(|entry| row_width(entry, text_renderer, theme, metrics))
            .fold(0.0, f64::max);
        Size {
            width,
            height: rows_height(self.entries.len(), metrics),
        }
    }

    fn draw_list(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        metrics: &Metrics,
        origin: Point,
    ) {
        let mut y = origin.y;
        for entry in &self.entries {
            draw_row(
                scene,
                transform,
                text_renderer,
                theme,
                metrics,
                entry,
                Point::new(origin.x, y),
                None,
            );
            y += metrics.row + metrics.gap;
        }
    }

    /// Edge columns and rows beside a drawing of the pad, sized to the
    /// longest entry on each.
    fn measure_layout(
        &self,
        layout: &PadLayout,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        metrics: &Metrics,
    ) -> Size {
        let geometry = self.layout_geometry(layout, text_renderer, theme, metrics);
        Size {
            width: geometry.left_w
                + geometry.pad.width()
                + geometry.right_w
                + metrics.section * 2.0,
            height: geometry.top_h + geometry.pad.height() + geometry.bottom_h,
        }
    }

    fn layout_geometry(
        &self,
        layout: &PadLayout,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        metrics: &Metrics,
    ) -> LayoutGeometry {
        let column_width = |side: PadSide, text_renderer: &mut TextRenderer| {
            layout
                .side(side)
                .iter()
                .filter_map(|control| self.entry(*control))
                .map(|entry| row_width(entry, text_renderer, theme, metrics))
                .fold(0.0, f64::max)
        };
        let left_w = column_width(PadSide::Left, text_renderer);
        let right_w = column_width(PadSide::Right, text_renderer);
        let edge_rows = |side: PadSide| {
            let n = layout.side(side).len();
            if n == 0 {
                0.0
            } else {
                rows_height(n, metrics) + metrics.section
            }
        };
        let top_h = edge_rows(PadSide::Top);
        let bottom_h = edge_rows(PadSide::Bottom);
        let side_rows = layout.left.len().max(layout.right.len()).max(3);
        let pad_h = rows_height(side_rows, metrics);
        let pad_w = (pad_h * 1.4).max(metrics.row * 4.0);
        LayoutGeometry {
            left_w,
            right_w,
            top_h,
            bottom_h,
            pad: Rect::new(0.0, 0.0, pad_w, pad_h),
        }
    }

    fn draw_layout(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        metrics: &Metrics,
        layout: &PadLayout,
        origin: Point,
    ) {
        let geometry = self.layout_geometry(layout, text_renderer, theme, metrics);
        let pad = geometry.pad.with_origin(Point::new(
            origin.x + geometry.left_w + metrics.section,
            origin.y + geometry.top_h,
        ));
        // The pad itself: a lifted surface inside the card, its active area
        // lifted less, each with the edge derived from its own fill.
        let colors = &theme.tokens.colors;
        ui::surface::card(
            scene,
            transform,
            pad,
            f64::from(theme.tokens.radii.control),
            colors.lift(0.35),
        );
        let active = pad.inset(-metrics.section);
        if active.width() > 0.0 && active.height() > 0.0 {
            ui::surface::card(
                scene,
                transform,
                active,
                f64::from(theme.tokens.radii.control),
                colors.lift(0.15),
            );
        }

        let column = |side: PadSide| -> Vec<&PadMapEntry> {
            layout
                .side(side)
                .iter()
                .filter_map(|control| self.entry(*control))
                .collect()
        };
        let column_top = |n: usize| pad.center().y - rows_height(n, metrics) / 2.0;
        // The left column hugs the pad: its keycaps sit at its right edge.
        let left = column(PadSide::Left);
        let mut y = column_top(left.len());
        for entry in left {
            let width = row_width(entry, text_renderer, theme, metrics);
            draw_row(
                scene,
                transform,
                text_renderer,
                theme,
                metrics,
                entry,
                Point::new(pad.x0 - metrics.section - width, y),
                Some(Align::CapLast),
            );
            y += metrics.row + metrics.gap;
        }
        let right = column(PadSide::Right);
        let mut y = column_top(right.len());
        for entry in right {
            draw_row(
                scene,
                transform,
                text_renderer,
                theme,
                metrics,
                entry,
                Point::new(pad.x1 + metrics.section, y),
                None,
            );
            y += metrics.row + metrics.gap;
        }
        for (side, top) in [
            (PadSide::Top, origin.y),
            (PadSide::Bottom, pad.y1 + metrics.section),
        ] {
            let mut y = top;
            for entry in column(side) {
                draw_row(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    metrics,
                    entry,
                    Point::new(pad.x0, y),
                    None,
                );
                y += metrics.row + metrics.gap;
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Size {
    width: f64,
    height: f64,
}

struct LayoutGeometry {
    left_w: f64,
    right_w: f64,
    top_h: f64,
    bottom_h: f64,
    pad: Rect,
}

/// The card's rhythm, all from theme tokens.
struct Metrics {
    title: f32,
    body: f32,
    caption: f32,
    gap: f64,
    section: f64,
    inset: f64,
    margin: f64,
    row: f64,
    label_max: f64,
}

impl Metrics {
    fn new(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let scale = &theme.tokens.typography;
        Self {
            title: scale.size(TextRole::Heading),
            body: scale.size(TextRole::Caption),
            caption: scale.size(TextRole::Caption),
            gap: f64::from(spacing.s2),
            section: f64::from(spacing.s4),
            inset: f64::from(spacing.s6),
            margin: f64::from(spacing.s8),
            // A keycap's height, so rows of caps never touch.
            row: f64::from(scale.size(TextRole::Caption)) + f64::from(spacing.s3) * 2.0,
            label_max: f64::from(spacing.s16) * 4.0,
        }
    }
}

fn rows_height(n: usize, metrics: &Metrics) -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "a pad has a few dozen controls")]
    let n = n as f64;
    (n * (metrics.row + metrics.gap) - metrics.gap).max(0.0)
}

#[derive(Clone, Copy)]
enum Align {
    /// Label first, keycap last: a row left of the pad points at it.
    CapLast,
}

fn label_width(entry: &PadMapEntry, text_renderer: &mut TextRenderer, metrics: &Metrics) -> f64 {
    let (width, _) = text_renderer.measure(&entry.action.label(), metrics.body, None);
    f64::from(width).min(metrics.label_max)
}

fn row_width(
    entry: &PadMapEntry,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    metrics: &Metrics,
) -> f64 {
    InputHint::measure_keycap_labels(&[entry.control.cap()], text_renderer, theme)
        + metrics.gap
        + label_width(entry, text_renderer, metrics)
}

fn draw_row(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    metrics: &Metrics,
    entry: &PadMapEntry,
    origin: Point,
    align: Option<Align>,
) {
    let cap = [entry.control.cap()];
    let cap_w = InputHint::measure_keycap_labels(&cap, text_renderer, theme);
    let label = entry.action.label();
    let label_w = label_width(entry, text_renderer, metrics);
    let (_, label_h) = text_renderer.measure(&label, metrics.body, Some(f32_of_f64(label_w)));
    let (cap_x, label_x) = match align {
        Some(Align::CapLast) => (origin.x + label_w + metrics.gap, origin.x),
        None => (origin.x, origin.x + cap_w + metrics.gap),
    };
    let cap_y = origin.y + (metrics.row - cap_height(theme)) / 2.0;
    InputHint::paint_keycap_labels_at(
        scene,
        transform,
        Point::new(cap_x, cap_y),
        &cap,
        text_renderer,
        theme,
    );
    let colors = &theme.tokens.colors;
    let color = if entry.action.is_active() {
        colors.text_primary
    } else {
        colors.text_dim
    };
    text_renderer.draw_text(
        scene,
        &label,
        metrics.body,
        color,
        transform.pre_translate(
            (
                label_x,
                origin.y + ((metrics.row - f64::from(label_h)) / 2.0).max(0.0),
            )
                .into(),
        ),
        Some(f32_of_f64(label_w)),
    );
}

/// The shared keycap's height, as `InputHint` sizes it.
fn cap_height(theme: &Theme) -> f64 {
    28.0_f64.max(f64::from(theme.tokens.typography.size(TextRole::Caption)) + 8.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_name_the_control_and_what_it_does() {
        let map = PadMap {
            name: "Pad".into(),
            layout: None,
            entries: vec![
                PadMapEntry {
                    control: PadControl::Button(0),
                    action: PadMapAction::App("Undo".into()),
                },
                PadMapEntry {
                    control: PadControl::Ring(0),
                    action: PadMapAction::Dial(PadDialAction::Zoom),
                },
                PadMapEntry {
                    control: PadControl::Button(1),
                    action: PadMapAction::ModeSwitch { mode: 1, modes: 4 },
                },
            ],
        };
        assert_eq!(
            map.narration(),
            "Button 1: Undo\nRing 1: Zoom\nButton 2: Next mode (now 2 of 4)"
        );
        assert_eq!(PadControl::Button(4).cap(), "5");
        assert!(!PadMapAction::Nothing.is_active());
        assert!(PadMapAction::ShowsMap.is_active());
    }

    #[test]
    fn the_card_paints_as_a_layout_and_as_a_list_in_every_theme() {
        let entries = vec![
            PadMapEntry {
                control: PadControl::Button(0),
                action: PadMapAction::App("A label long enough to wrap onto a second line".into()),
            },
            PadMapEntry {
                control: PadControl::Button(1),
                action: PadMapAction::Nothing,
            },
            PadMapEntry {
                control: PadControl::Ring(0),
                action: PadMapAction::Dial(PadDialAction::BrushSize),
            },
            PadMapEntry {
                control: PadControl::Strip(0),
                action: PadMapAction::AppDecides,
            },
        ];
        let layout = PadLayout {
            left: vec![PadControl::Button(0), PadControl::Ring(0)],
            right: vec![PadControl::Strip(0)],
            top: vec![PadControl::Button(1)],
            bottom: Vec::new(),
        };
        let mut renderer = TextRenderer::new();
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            for layout in [Some(layout.clone()), None] {
                let map = PadMap {
                    name: "Pad".into(),
                    layout,
                    entries: entries.clone(),
                };
                let mut scene = Scene::new();
                map.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut renderer,
                    &theme,
                    Rect::new(0.0, 0.0, 1280.0, 800.0),
                );
                // A tiny viewport clamps rather than panicking.
                map.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut renderer,
                    &theme,
                    Rect::new(0.0, 0.0, 40.0, 30.0),
                );
                assert!(!scene.encoding().is_empty());
            }
        }
    }
}
