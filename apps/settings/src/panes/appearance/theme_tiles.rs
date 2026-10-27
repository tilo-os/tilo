//! The theme picker: each theme drawn as a small window in its own colours
//! (the desktop ground, a sidebar with the selected row, a heading and text
//! lines, a primary button in the accent and a switch that is on), named
//! underneath. Automatic is the light theme beside the dark one. Clicking a
//! tile, or Enter and the arrow keys on it, chooses that theme.
//!
//! Each tile is painted with the real theme it names, so what the person
//! picks is what they get: the surfaces' separation, the ink, the accent
//! and the on colour are the tokens every app draws with.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::widget::{ModelPath, TaffyTreeExt, Widget, card_column_width, card_columns};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::i18n::t;
use crate::prefs::AppearancePrefs;

/// One tile: the theme's name as stored and the themes it is painted with
/// (two for Automatic, light then dark).
struct Tile {
    name: &'static str,
    themes: Vec<Theme>,
}

thread_local! {
    /// The themes the tiles are painted with, built once: they are the
    /// shipped presets, which never change while the app runs.
    static TILES: OnceCell<Rc<[Tile]>> = const { OnceCell::new() };
}

fn tiles() -> Rc<[Tile]> {
    TILES.with(|tiles| {
        Rc::clone(tiles.get_or_init(|| {
            AppearancePrefs::THEMES
                .iter()
                .map(|name| Tile {
                    name,
                    themes: if *name == "Automatic" {
                        ["Light", "Dark"]
                            .iter()
                            .filter_map(|name| Theme::named(name))
                            .collect()
                    } else {
                        Theme::named(name).into_iter().collect()
                    },
                })
                .filter(|tile| !tile.themes.is_empty())
                .collect()
        }))
    })
}

/// A tile's height as a share of its width.
const TILE_ASPECT: f64 = 0.66;

/// How the tiles lay out across `width`, by the card grid's reflow: as many
/// on a line as fit at their narrowest, rounded down to a count that fills
/// every line alike, the columns sharing the whole line. `(columns, width,
/// height)`.
fn tile_geometry(width: f64, tiles: usize, gap: f64, narrowest: f64) -> (usize, f64, f64) {
    let tiles = tiles.max(1);
    let fit = card_columns(width, narrowest, gap).min(tiles);
    let columns = (1..=fit)
        .rev()
        .find(|c| tiles.is_multiple_of(*c))
        .unwrap_or(fit);
    let tile = card_column_width(width, columns, gap);
    (columns, tile, (tile * TILE_ASPECT).round())
}

pub struct ThemeTiles {
    tiles: Rc<[Tile]>,
    chosen: String,
    /// The tile under the pointer and the keyboard's tile.
    hover: Option<usize>,
    cursor: usize,
    focused: bool,
    enabled: bool,
    last_cursor: Point,
    action: Option<ui::widget::ActionEmitter<String>>,
    rect: Rect,
    /// The theme's narrowest and asked-for tile widths, gap between tiles
    /// and room for a name.
    narrowest: f64,
    preferred: f64,
    gap: f64,
    label: f64,
}

impl ThemeTiles {
    pub fn new(chosen: impl Into<String>) -> Self {
        let chosen = chosen.into();
        let tiles = tiles();
        let cursor = tiles
            .iter()
            .position(|tile| tile.name == chosen)
            .unwrap_or_default();
        Self {
            tiles,
            chosen,
            hover: None,
            cursor,
            focused: false,
            enabled: true,
            last_cursor: Point::ZERO,
            action: None,
            rect: Rect::ZERO,
            narrowest: f64::from(ui::tokens::Metrics::BASE.picture_tile_min_width),
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

    /// How many tiles stand on a line at the laid-out width.
    fn columns(&self) -> usize {
        self.geometry().0
    }

    /// Tile `index`'s picture (without its name).
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
        if self.chosen == tile.name || !self.enabled {
            return;
        }
        // The ring stays on the stored theme until the store says otherwise:
        // the pane rebuilds the tiles when the chosen theme changes, so a
        // choice that never lands is never shown as made.
        if let Some(action) = &mut self.action
            && let Err(error) = action.emit(tile.name.to_owned())
        {
            tracing::warn!(%error, "the theme choice was refused");
        }
    }

    pub(super) fn label_of(name: &str) -> String {
        t(&format!(
            "settings-appearance-theme-{}",
            name.to_ascii_lowercase().replace(' ', "-")
        ))
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
    // ratchet-allow(raw_paint): a theme tile is a miniature of that theme, drawn in its own colours
    scene.fill(
        Fill::NonZero,
        transform,
        // ratchet-allow(desktop_ground): a theme tile is a miniature of that theme, drawn in its own colours
        surfaces.base,
        None,
        &RoundedRect::from_rect(area, radius),
    );
    let inset = (area.height() * 0.12).round();
    let window = Rect::new(area.x0 + inset, area.y0 + inset, area.x1, area.y1);
    let corner = (radius * 0.6).max(3.0);
    // ratchet-allow(raw_paint): a theme tile is a miniature of that theme, drawn in its own colours
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

/// A theme tile's sidebar: the recessed region down the window's leading
/// side with three rows, the second selected. Where it ends.
fn paint_sidebar(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    window: Rect,
    (corner, unit): (f64, f64),
) -> Rect {
    let colors = &theme.tokens.colors;
    let surfaces = &colors.surfaces;
    // The sidebar: the recessed region with three rows, the second
    // selected.
    let sidebar = Rect::new(
        window.x0,
        window.y0,
        window.x0 + window.width() * 0.32,
        window.y1,
    );
    // ratchet-allow(raw_paint): a theme tile is a miniature of that theme, drawn in its own colours
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
            // ratchet-allow(raw_paint): a theme tile is a miniature of that theme, drawn in its own colours
            scene.fill(
                Fill::NonZero,
                transform,
                colors.intent.selection_tint,
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
            // ratchet-allow(literal_alpha): a theme tile is a miniature of that theme, drawn in its own colours
            colors.text_dim.multiply_alpha(0.7),
        );
    }
    sidebar
}

/// A theme tile's content beside its sidebar: a heading, two lines of
/// text, a primary button and a switch that is on.
fn paint_tile_content(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    window: Rect,
    sidebar: Rect,
    unit: f64,
) {
    let colors = &theme.tokens.colors;
    // The content: a heading, two lines of text, a primary button and a
    // switch that is on.
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
        // ratchet-allow(literal_alpha): a theme tile is a miniature of that theme, drawn in its own colours
        colors.text_dim.multiply_alpha(0.6),
    );
    tile_bar(
        scene,
        transform,
        unit,
        left,
        window.y0 + unit * 4.0,
        width * 0.7,
        // ratchet-allow(literal_alpha): a theme tile is a miniature of that theme, drawn in its own colours
        colors.text_dim.multiply_alpha(0.6),
    );
    let control = unit * 1.6;
    let bottom = window.y1 - unit * 1.2;
    // ratchet-allow(raw_paint): a theme tile is a miniature of that theme, drawn in its own colours
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
    // ratchet-allow(raw_paint): a theme tile is a miniature of that theme, drawn in its own colours
    scene.fill(
        Fill::NonZero,
        transform,
        colors.intent.on_state,
        None,
        &RoundedRect::from_rect(track, control / 2.0),
    );
    // ratchet-allow(raw_paint): a theme tile is a miniature of that theme, drawn in its own colours
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

/// One bar of a theme tile's miniature text, `unit` tall at most.
fn tile_bar(
    scene: &mut Scene,
    transform: Affine,
    unit: f64,
    x: f64,
    y: f64,
    width: f64,
    color: vello::peniko::Color,
) {
    // ratchet-allow(raw_paint): a theme tile is a miniature of that theme, drawn in its own colours
    scene.fill(
        Fill::NonZero,
        transform,
        color,
        None,
        &RoundedRect::new(x, y, x + width, y + unit * 0.7, unit * 0.35),
    );
}

impl Widget for ThemeTiles {
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
        let width = known.width.unwrap_or_else(|| match available.width {
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
        let radius = f64::from(theme.tokens.radii.control);
        let caption = theme.text(ui::TextRole::Caption);
        for (index, tile) in self.tiles.iter().enumerate() {
            let area = self.tile_rect(index);
            match tile.themes.as_slice() {
                [light, dark] => {
                    // Automatic: the light theme on the left, the dark on
                    // the right, split down the middle.
                    let half = Rect::new(area.x0, area.y0, area.center().x, area.y1);
                    scene.push_layer(
                        Fill::NonZero,
                        vello::peniko::BlendMode::default(),
                        1.0,
                        transform,
                        &half,
                    );
                    paint_window(scene, transform, area, light, radius);
                    scene.pop_layer();
                    let other = Rect::new(area.center().x, area.y0, area.x1, area.y1);
                    scene.push_layer(
                        Fill::NonZero,
                        vello::peniko::BlendMode::default(),
                        1.0,
                        transform,
                        &other,
                    );
                    paint_window(scene, transform, area, dark, radius);
                    scene.pop_layer();
                }
                [only, ..] => paint_window(scene, transform, area, only, radius),
                [] => {}
            }
            let chosen = tile.name == self.chosen;
            let edge = RoundedRect::from_rect(area, radius);
            if chosen {
                // The chosen theme: its own edge, heavier and in the accent,
                // in the same place (the design system's selected edge).
                ui::surface::selected_edge(scene, transform, area, radius, theme);
            } else {
                // ratchet-allow(raw_paint): a theme tile is a miniature of that theme, drawn in its own colours
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    theme.tokens.colors.overlay_border_subtle,
                    None,
                    &edge,
                );
            }
            if self.hover == Some(index) && !chosen {
                // ratchet-allow(raw_paint): a theme tile is a miniature of that theme, drawn in its own colours
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.overlay_subtle,
                    None,
                    &edge,
                );
            }
            if self.focused && self.cursor == index {
                // ratchet-allow(raw_paint): a theme tile is a miniature of that theme, drawn in its own colours
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.border_focus,
                    None,
                    &edge.rect().inflate(6.0, 6.0).to_rounded_rect(radius + 6.0),
                );
            }
            let name = Self::label_of(tile.name);
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
                .with_name(t("settings-appearance-theme"))
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
                match self
                    .tiles
                    .iter()
                    .position(|tile| tile.name == value || Self::label_of(tile.name) == *value)
                {
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
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.action
            .as_mut()
            .and_then(ui::widget::ActionEmitter::take)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every theme Settings offers has a tile painted in that theme, and
    /// Automatic is painted in both.
    #[test]
    fn every_offered_theme_has_a_tile_in_its_own_colours() {
        let tiles = tiles();
        assert_eq!(
            tiles.iter().map(|tile| tile.name).collect::<Vec<_>>(),
            AppearancePrefs::THEMES
        );
        let automatic = tiles.iter().find(|tile| tile.name == "Automatic").unwrap();
        assert_eq!(automatic.themes.len(), 2);
        let light = tiles.iter().find(|tile| tile.name == "Light").unwrap();
        let dark = tiles.iter().find(|tile| tile.name == "Dark").unwrap();
        assert_ne!(
            light.themes[0].tokens.colors.surfaces.content,
            dark.themes[0].tokens.colors.surfaces.content
        );
    }

    /// The tiles fill even lines across the width they get: all six on one
    /// line on a desktop pane, three by two narrower, two by three on a
    /// phone.
    #[test]
    fn the_tiles_fill_even_lines() {
        let mut tiles = ThemeTiles::new("Dark");
        tiles.rect = Rect::new(0.0, 0.0, 720.0, 400.0);
        assert_eq!(tiles.columns(), AppearancePrefs::THEMES.len());
        tiles.rect = Rect::new(0.0, 0.0, 400.0, 400.0);
        assert_eq!(tiles.columns(), 3);
        tiles.rect = Rect::new(0.0, 0.0, 260.0, 400.0);
        assert_eq!(tiles.columns(), 2);
        assert!(tiles.tile_rect(2).y0 > tiles.tile_rect(0).y1);
        assert_eq!(tiles.tile_at(tiles.tile_rect(3).center()), Some(3));
    }

    /// From a phone to a wide window, every line of tiles spans the whole
    /// width: the tiles grow with the window instead of leaving a gap at
    /// the end of the line, and the column count reflows as it widens.
    #[test]
    fn the_tiles_fill_the_line_at_every_width() {
        let mut tiles = ThemeTiles::new("Dark");
        let mut counts = Vec::new();
        let mut first_width = Vec::new();
        for width in [260.0, 360.0, 420.0, 560.0, 640.0, 720.0, 900.0, 1400.0] {
            tiles.rect = Rect::new(0.0, 0.0, width, 400.0);
            let columns = tiles.columns();
            let line_end = tiles.tile_rect(columns - 1).x1;
            assert!(
                (line_end - width).abs() < 0.5,
                "at {width}: the line of {columns} tiles ends at {line_end}"
            );
            assert!(
                (tiles.tile_rect(0).x0).abs() < 0.5,
                "at {width}: the line starts at the edge"
            );
            let widths: Vec<f64> = (0..columns).map(|i| tiles.tile_rect(i).width()).collect();
            assert!(
                widths.iter().all(|w| (w - widths[0]).abs() < 0.01),
                "at {width}: the tiles share the line evenly: {widths:?}"
            );
            counts.push(columns);
            first_width.push(widths[0]);
        }
        assert!(
            counts.windows(2).all(|pair| pair[1] >= pair[0]),
            "a wider pane never shows fewer columns: {counts:?}"
        );
        assert_eq!(counts.first(), Some(&2), "a phone shows two columns");
        assert_eq!(
            counts.last(),
            Some(&AppearancePrefs::THEMES.len()),
            "a wide pane shows every tile on one line"
        );
        assert!(
            first_width[7] > first_width[6],
            "on one line the tiles keep growing with the window: {first_width:?}"
        );
    }
}
