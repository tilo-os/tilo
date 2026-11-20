//! The look picker: each look drawn as a small window in its own colours, its light
//! side beside its dark side (the desktop ground, a sidebar with the selected row, a
//! heading and text lines, a primary button in the accent and a switch that is on),
//! named underneath. A look is painted in the accent the person chose, so a tile shows
//! what choosing it gives.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::theme::{Look, Theme};
use ui::tokens::{Emphasis, PresenceHue};
use ui::widget::{ModelPath, TaffyTreeExt, Widget, card_column_width, card_columns};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::i18n::t;

struct Tile {
    look: Look,
    /// The light side, then the dark side.
    sides: [Theme; 2],
}

/// Every look, painted in `accent` when the person chose one and in the look's own
/// otherwise, for the person whose colour is `you`.
fn tiles(accent: Option<PresenceHue>, you: PresenceHue) -> Vec<Tile> {
    Look::ALL
        .into_iter()
        .map(|look| {
            let side = |dark: bool| {
                let mut document = look.preset(dark).document();
                if let Some(accent) = accent {
                    document.emphasis = Emphasis::Tinted {
                        hue: accent.degrees(),
                    };
                }
                Theme::from_document(&document)
                    .unwrap_or_else(|| Theme::preset(look.preset(dark)))
                    .with_your_colour(you)
            };
            Tile {
                look,
                sides: [side(false), side(true)],
            }
        })
        .collect()
}

const TILE_ASPECT: f64 = 0.66;

fn tile_geometry(width: f64, tiles: usize, gap: f64, narrowest: f64) -> (usize, f64, f64) {
    let columns = card_columns(width, narrowest, gap).clamp(1, tiles.max(1));
    let tile = card_column_width(width, columns, gap);
    (columns, tile, (tile * TILE_ASPECT).round())
}

pub struct ThemeTiles {
    tiles: Vec<Tile>,
    /// The id of the look shown chosen; a theme of the person's own matches none.
    chosen: String,
    accent: Option<PresenceHue>,
    /// The colour the tiles were painted for; the live theme's is the person's.
    you: PresenceHue,
    hover: Option<usize>,
    cursor: usize,
    focused: bool,
    enabled: bool,
    last_cursor: Point,
    action: Option<ui::widget::ActionEmitter<String>>,
    rect: Rect,
    narrowest: f64,
    preferred: f64,
    gap: f64,
    label: f64,

    dirty: bool,
}

impl ThemeTiles {
    /// The picker with the look `chosen` (an id) marked, every tile in `accent`.
    pub fn new(chosen: impl Into<String>, accent: Option<PresenceHue>) -> Self {
        let chosen = chosen.into();
        let you = PresenceHue::Blue;
        let tiles = tiles(accent, you);
        let cursor = tiles
            .iter()
            .position(|tile| tile.look.id() == chosen)
            .unwrap_or_default();
        Self {
            dirty: true,
            tiles,
            chosen,
            accent,
            you,
            hover: None,
            cursor,
            focused: false,
            enabled: true,
            last_cursor: Point::ZERO,
            action: None,
            rect: Rect::ZERO,
            narrowest: 132.0,
            preferred: f64::from(ui::tokens::Metrics::BASE.picture_tile_width),
            gap: 12.0,
            label: 24.0,
        }
    }

    fn geometry(&self) -> (usize, f64, f64) {
        tile_geometry(
            self.rect.width(),
            self.tiles.len(),
            self.gap,
            self.narrowest,
        )
    }

    fn columns(&self) -> usize {
        self.geometry().0
    }

    fn tile_rect(&self, index: usize) -> Rect {
        let (columns, width, height) = self.geometry();
        let (column, line) = (index % columns, index / columns);
        let x = self.rect.x0 + column as f64 * (width + self.gap);
        let y = self.rect.y0 + line as f64 * (height + self.label + self.gap);
        Rect::new(x, y, x + width, y + height)
    }

    fn tile_at(&self, point: Point) -> Option<usize> {
        (0..self.tiles.len()).find(|index| {
            let picture = self.tile_rect(*index);
            Rect::new(picture.x0, picture.y0, picture.x1, picture.y1 + self.label).contains(point)
        })
    }

    fn choose(&mut self, index: usize) {
        let Some(tile) = self.tiles.get(index) else {
            return;
        };
        self.cursor = index;
        if self.chosen == tile.look.id() || !self.enabled {
            return;
        }
        // The ring stays on the stored look until the store says otherwise:
        // the pane rebuilds the tiles when the chosen look changes, so a
        // choice that never lands is never shown as made.
        if let Some(action) = &mut self.action
            && let Err(error) = action.emit(tile.look.id().to_owned())
        {
            tracing::warn!(%error, "the look choice was refused");
        }
    }

    /// The name a person reads for the look stored as `id`; a theme of their own reads
    /// as its id.
    pub(super) fn label_of(id: &str) -> String {
        match Look::find(id) {
            Some(look) => t(&format!("settings-appearance-look-{}", look.id())),
            None => id.strip_prefix("custom:").unwrap_or(id).to_owned(),
        }
    }
}

impl ui::framework::ActionControl<String> for ThemeTiles {
    fn bind_action(mut self, binding: tilo_actions::ActionBinding<String>) -> Self {
        self.action = Some(ui::widget::ActionEmitter::new(binding));
        self
    }

    fn unavailable(mut self) -> Self {
        self.enabled = false;
        self
    }
}

fn paint_window(scene: &mut Scene, transform: Affine, area: Rect, theme: &Theme, radius: f64) {
    let colors = &theme.tokens.colors;
    let surfaces = &colors.surfaces;
    scene.fill(
        Fill::NonZero,
        transform,
        surfaces.base,
        None,
        &RoundedRect::from_rect(area, radius),
    );
    let inset = (area.height() * 0.12).round();
    let window = Rect::new(area.x0 + inset, area.y0 + inset, area.x1, area.y1);
    let corner = (radius * 0.6).max(3.0);
    scene.fill(
        Fill::NonZero,
        transform,
        surfaces.content,
        None,
        &RoundedRect::new(
            window.x0,
            window.y0,
            window.x1,
            window.y1,
            (corner, 0.0, radius, 0.0),
        ),
    );
    let unit = (window.height() / 9.0).max(2.0);
    let sidebar = paint_sidebar(scene, transform, theme, window, (corner, unit));
    paint_tile_content(scene, transform, theme, window, sidebar, unit);
}

fn paint_sidebar(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    window: Rect,
    (corner, unit): (f64, f64),
) -> Rect {
    let colors = &theme.tokens.colors;
    let surfaces = &colors.surfaces;
    let sidebar = Rect::new(
        window.x0,
        window.y0,
        window.x0 + window.width() * 0.32,
        window.y1,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        surfaces.sidebar,
        None,
        &RoundedRect::new(
            sidebar.x0,
            sidebar.y0,
            sidebar.x1,
            sidebar.y1,
            (corner, 0.0, 0.0, 0.0),
        ),
    );
    for row in 0..3 {
        let y = sidebar.y0 + unit * (1.2 + f64::from(row) * 1.6);
        if row == 1 {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.intent.selection,
                None,
                &RoundedRect::new(
                    sidebar.x0 + unit * 0.4,
                    y - unit * 0.45,
                    sidebar.x1 - unit * 0.4,
                    y + unit * 1.15,
                    unit * 0.4,
                ),
            );
        }
        tile_bar(
            scene,
            transform,
            unit,
            sidebar.x0 + unit,
            y,
            sidebar.width() * 0.55,
            colors.text_dim.multiply_alpha(0.7),
        );
    }
    sidebar
}

fn paint_tile_content(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    window: Rect,
    sidebar: Rect,
    unit: f64,
) {
    let colors = &theme.tokens.colors;
    let left = sidebar.x1 + unit * 1.2;
    let width = window.x1 - left - unit * 1.2;
    tile_bar(
        scene,
        transform,
        unit,
        left,
        window.y0 + unit * 1.2,
        width * 0.55,
        colors.text_primary,
    );
    tile_bar(
        scene,
        transform,
        unit,
        left,
        window.y0 + unit * 2.8,
        width * 0.9,
        colors.text_dim.multiply_alpha(0.6),
    );
    tile_bar(
        scene,
        transform,
        unit,
        left,
        window.y0 + unit * 4.0,
        width * 0.7,
        colors.text_dim.multiply_alpha(0.6),
    );
    let control = unit * 1.6;
    let bottom = window.y1 - unit * 1.2;
    scene.fill(
        Fill::NonZero,
        transform,
        colors.intent.accent,
        None,
        &RoundedRect::new(
            left,
            bottom - control,
            left + width * 0.42,
            bottom,
            unit * 0.5,
        ),
    );
    let track = Rect::new(
        window.x1 - unit * 1.2 - control * 1.8,
        bottom - control,
        window.x1 - unit * 1.2,
        bottom,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        colors.intent.on_state,
        None,
        &RoundedRect::from_rect(track, control / 2.0),
    );
    scene.fill(
        Fill::NonZero,
        transform,
        colors.intent.on_state_mark,
        None,
        &Circle::new(
            (track.x1 - control / 2.0, track.center().y),
            control / 2.0 - 1.5,
        ),
    );
}

fn tile_bar(
    scene: &mut Scene,
    transform: Affine,
    unit: f64,
    x: f64,
    y: f64,
    width: f64,
    color: vello::peniko::Color,
) {
    scene.fill(
        Fill::NonZero,
        transform,
        color,
        None,
        &RoundedRect::new(x, y, x + width, y + unit * 0.7, unit * 0.35),
    );
}

impl Widget for ThemeTiles {
    fn html(&self, cx: &mut ui::composition::HtmlCx<'_>) {
        cx.picture();
    }

    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.narrowest = f64::from(theme.tokens.metrics.picture_tile_min_width);
        self.preferred = f64::from(theme.tokens.metrics.picture_tile_width);
        self.gap = f64::from(theme.tokens.spacing.rhythm.sibling);
        self.label = f64::from(theme.text(ui::TextRole::Caption).line_height_px())
            + f64::from(theme.tokens.spacing.s2);
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            min_size: Size {
                width: Dimension::length(self.narrowest as f32),
                height: Dimension::auto(),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        _text: &mut ui::text_render::TextRenderer,
    ) -> Size<f32> {
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(width) => width,
            _ => ((self.preferred + self.gap) * self.tiles.len() as f64) as f32,
        });
        let (columns, _, height) =
            tile_geometry(f64::from(width), self.tiles.len(), self.gap, self.narrowest);
        let lines = self.tiles.len().div_ceil(columns);
        Size {
            width,
            height: (lines as f64 * (height + self.label + self.gap) - self.gap).max(0.0) as f32,
        }
    }

    ui::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        // The tiles wear the person's colour where a look's accent is theirs.
        let you = theme.tokens.colors.you;
        if you != self.you {
            self.you = you;
            self.tiles = tiles(self.accent, you);
        }
        let radius = f64::from(theme.tokens.radii.control);
        let caption = theme.text(ui::TextRole::Caption);
        for (index, tile) in self.tiles.iter().enumerate() {
            let area = self.tile_rect(index);
            let [light, dark] = &tile.sides;
            // The light side, then the dark side across a slanted line.
            let slant = area.height() * 0.18;
            let middle = area.center().x;
            // The part of the tile between its edge at `edge` and the slanted line.
            let half = |edge: f64| {
                let mut path = vello::kurbo::BezPath::new();
                path.move_to((edge, area.y0));
                path.line_to((middle + slant, area.y0));
                path.line_to((middle - slant, area.y1));
                path.line_to((edge, area.y1));
                path.close_path();
                path
            };
            let light_half = half(area.x0);
            let dark_half = half(area.x1);
            for (side, half) in [(light, &light_half), (dark, &dark_half)] {
                scene.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    transform,
                    half,
                );
                paint_window(scene, transform, area, side, radius);
                scene.pop_layer();
            }
            let chosen = tile.look.id() == self.chosen;
            let edge = RoundedRect::from_rect(area, radius);
            if chosen {
                ui::surface::selected_edge(scene, transform, area, radius, theme);
            } else {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    theme.tokens.colors.overlay_border_subtle,
                    None,
                    &edge,
                );
            }
            if self.hover == Some(index) && !chosen {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.overlay_subtle,
                    None,
                    &edge,
                );
            }
            if self.focused && self.cursor == index {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.border_focus,
                    None,
                    &edge.rect().inflate(6.0, 6.0).to_rounded_rect(radius + 6.0),
                );
            }
            let name = Self::label_of(tile.look.id());
            let (width, _) = text.measure_style(&name, Some(area.width() as f32), &caption);
            text.draw_style(
                scene,
                &name,
                if chosen {
                    theme.text_primary
                } else {
                    theme.text_dim
                },
                transform
                    * Affine::translate((
                        area.center().x - f64::from(width) / 2.0,
                        area.y1 + f64::from(theme.tokens.spacing.s2),
                    )),
                Some(area.width() as f32),
                &caption,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.dirty = true;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                let hover = self.tile_at(self.last_cursor);
                if hover != self.hover {
                    self.hover = hover;
                    *ctx.handled = true;
                }
            }
            WindowEvent::CursorLeft { .. } if self.hover.is_some() => {
                self.hover = None;
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(index) = self.tile_at(self.last_cursor) {
                    self.choose(index);
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == ElementState::Pressed =>
            {
                use winit::keyboard::{Key, NamedKey};
                let last = self.tiles.len().saturating_sub(1);
                let columns = self.columns();
                match event.logical_key {
                    Key::Named(NamedKey::ArrowLeft) => self.cursor = self.cursor.saturating_sub(1),
                    Key::Named(NamedKey::ArrowRight) => self.cursor = (self.cursor + 1).min(last),
                    Key::Named(NamedKey::ArrowUp) => {
                        self.cursor = self.cursor.saturating_sub(columns);
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.cursor = (self.cursor + columns).min(last);
                    }
                    Key::Named(NamedKey::Enter | NamedKey::Space) => self.choose(self.cursor),
                    _ => return,
                }
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        ui::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "ThemeTiles"
    }

    fn test_id(&self) -> Option<&str> {
        Some("appearance.theme")
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.tile_at(point)
            .map(|_| winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        self.enabled
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Group)
                .with_name(t("settings-appearance-look"))
                .with_value(Self::label_of(&self.chosen))
                .with_state(
                    AccessState::new()
                        .focusable(self.enabled)
                        .focused(self.focused)
                        .disabled(!self.enabled),
                ),
        )
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _key_path: &[u64],
        _state: &mut ui::widget::Edits<'_>,
        _root: &ModelPath,
    ) -> bool {
        match action {
            ui::a11y::Action::SetValue(value) => {
                match self.tiles.iter().position(|tile| {
                    tile.look.id() == value || Self::label_of(tile.look.id()) == *value
                }) {
                    Some(index) => {
                        self.choose(index);
                        true
                    }
                    None => false,
                }
            }
            ui::a11y::Action::Activate => {
                self.choose(self.cursor);
                true
            }
            _ => false,
        }
    }
}

impl ui::widget::controls::ActionControls for ThemeTiles {
    ui::widget::controls::optional_emitter_controls!();
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn the_look_tiles_show_every_look_by_name_on_both_sides;

        phase every_look_has_a_tile_with_a_light_and_a_dark_side;
        {
            let tiles = tiles(None, PresenceHue::Blue);
            assert_eq!(
                tiles.iter().map(|tile| tile.look).collect::<Vec<_>>(),
                Look::ALL
            );
            for tile in &tiles {
                let [light, dark] = &tile.sides;
                assert!(!light.document.is_dark(), "{:?}", tile.look);
                assert!(dark.document.is_dark(), "{:?}", tile.look);
            }
        }

        phase a_chosen_accent_paints_every_tile;
        {
            let own = tiles(None, PresenceHue::Blue);
            let violet = tiles(Some(PresenceHue::Violet), PresenceHue::Blue);
            for (own, violet) in own.iter().zip(&violet) {
                if own.look == Look::Mono {
                    continue;
                }
                assert_ne!(
                    own.sides[1].tokens.colors.intent.accent,
                    violet.sides[1].tokens.colors.intent.accent,
                    "{:?}",
                    own.look
                );
            }
        }

        phase every_tile_has_a_name;
        // Every tile is named in words, never by the message id it looks its
        // name up under.
        {
            for look in Look::ALL {
                let label = ThemeTiles::label_of(look.id());
                assert!(
                    !label.starts_with("settings-"),
                    "the {} tile shows its message id: {label}",
                    look.id()
                );
            }
            assert_eq!(ThemeTiles::label_of("custom:ember"), "ember");
        }

        phase the_tiles_fill_the_line_at_every_width;
        // From a phone to a wide window, every full line of tiles spans the
        // whole width, and a wider pane never shows fewer columns.
        {
            let mut tiles = ThemeTiles::new("tilo", None);
            let mut counts = Vec::new();
            for width in [300.0, 360.0, 420.0, 560.0, 640.0, 720.0, 900.0, 1400.0] {
                tiles.rect = Rect::new(0.0, 0.0, width, 400.0);
                let columns = tiles.columns();
                let line_end = tiles.tile_rect(columns - 1).x1;
                assert!(
                    (line_end - width).abs() < 0.5,
                    "at {width}: the line of {columns} tiles ends at {line_end}"
                );
                counts.push(columns);
            }
            assert!(
                counts.windows(2).all(|pair| pair[1] >= pair[0]),
                "a wider pane never shows fewer columns: {counts:?}"
            );
            assert_eq!(counts.first(), Some(&2), "a phone shows two columns");
            assert_eq!(counts.last(), Some(&5), "a wide pane shows every look in a line");
            tiles.rect = Rect::new(0.0, 0.0, 300.0, 400.0);
            assert!(tiles.tile_rect(2).y0 > tiles.tile_rect(0).y1);
            assert_eq!(tiles.tile_at(tiles.tile_rect(3).center()), Some(3));
        }
    }
}
