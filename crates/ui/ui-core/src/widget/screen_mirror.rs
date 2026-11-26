//! `ScreenMirror`: someone's screen as a small picture. A window card in the screen's own shape
//! holds their windows where they stand, each its app's mark with what it is called under it where
//! there is room, and the one they work in ringed in their colour.

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use tilo_ui_base::component::{Component, Described};
use tilo_ui_base::icons::Icon;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::{TaffyTreeExt, Widget};

/// Someone's screen as a picture of where they are.
#[derive(Component, Default)]
#[component(name = "ScreenMirror", role = "Image", label = "label")]
pub struct ScreenMirrorProps {
    /// What the picture says to a person who cannot see it: whose screen it is.
    #[component(required, positional)]
    pub label: String,
    /// Their screen's width, logical pixels: with `screen_height`, the shape the picture keeps.
    pub screen_width: f64,
    /// Their screen's height, logical pixels.
    pub screen_height: f64,
    /// Their colour, `rrggbb`: the ring of the window they work in.
    #[component(data)]
    pub colour: String,
    /// How tall the picture stands, logical pixels; the screen keeps its shape inside it.
    pub height: f64,
    /// The person looking follows them: their colour runs the picture's whole height, saying
    /// whose view this is.
    pub followed: bool,
    /// Their windows, in the order they paint.
    #[component(part(default))]
    pub windows: Vec<ScreenMirrorWindowProps>,
}

/// One window on someone's screen.
#[derive(Component, Default)]
#[component(name = "ScreenMirrorWindow", role = "Window", label = "name")]
pub struct ScreenMirrorWindowProps {
    /// What it is called here.
    #[component(required, positional)]
    pub name: String,
    /// The app it shows, by its id: its mark.
    #[component(data)]
    pub app: String,
    /// Its left edge, as a share of the screen's width.
    pub left: f64,
    /// Its top edge, as a share of the screen's height.
    pub top: f64,
    /// Its right edge, as a share of the screen's width.
    pub right: f64,
    /// Its bottom edge, as a share of the screen's height.
    pub bottom: f64,
    /// They work in it.
    pub focused: bool,
}

tilo_ui_base::part_type!(ScreenMirrorWindowProps);

/// See the module docs.
pub struct ScreenMirror {
    props: ScreenMirrorProps,
    colour: Color,
    rect: Rect,
    layout_dirty: bool,
    dirty: bool,
}

impl ScreenMirror {
    /// The screen's card inside the widget's rect, in the screen's own shape.
    fn screen_rect(&self) -> Rect {
        let rect = self.rect;
        let aspect = self.props.screen_width / self.props.screen_height.max(1.0);
        let (width, height) = if rect.width() / aspect.max(0.01) <= rect.height() {
            (rect.width(), rect.width() / aspect.max(0.01))
        } else {
            (rect.height() * aspect, rect.height())
        };
        Rect::from_center_size(rect.center(), (width, height))
    }

    /// Where `window` stands inside the screen's card `screen`.
    fn tile(screen: Rect, window: &ScreenMirrorWindowProps) -> Rect {
        Rect::new(
            screen.x0 + window.left * screen.width(),
            screen.y0 + window.top * screen.height(),
            screen.x0 + window.right * screen.width(),
            screen.y0 + window.bottom * screen.height(),
        )
        .inset(-1.0)
    }
}

/// One of their windows inside their screen `screen_rect`: its ground, its app's mark, what it is
/// called under it where there is room, and the ring of the one they work in.
fn paint_window(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    screen_rect: Rect,
    window: &ScreenMirrorWindowProps,
    colour: Color,
) {
    let tile = ScreenMirror::tile(screen_rect, window);
    if tile.width() < 4.0 || tile.height() < 4.0 {
        return;
    }
    // The tile is a well in the screen's card, and its rest edge is the well's own.
    let on = crate::widget::paint_surface(
        scene,
        transform,
        tile,
        crate::widget::SurfaceLevel::Raised,
        theme,
    );
    let ink = on.tokens.colors.text_dim;
    let style = on
        .text(TextRole::Caption)
        .with_weight(tilo_ui_base::text_render::TextWeight::Medium);
    let line_h = f64::from(theme.text(TextRole::Caption).line_height_px());
    let icon = (tile.height() * 0.3).clamp(10.0, 20.0);
    let named = (!window.name.trim().is_empty()
        && tile.height() >= icon + line_h + 10.0
        && tile.width() >= 44.0)
        .then_some(window.name.as_str());
    let block = if named.is_some() {
        icon + 4.0 + line_h
    } else {
        icon
    };
    let top = tile.center().y - block / 2.0;
    let mark = Rect::new(
        tile.center().x - icon / 2.0,
        top,
        tile.center().x + icon / 2.0,
        top + icon,
    );
    let glyph = match tilo_ui_base::icons::app_glyph(&window.app, None) {
        Icon::AppWindow => Icon::Square,
        glyph => glyph,
    };
    glyph.draw_with_stroke(scene, transform, mark, ink, 1.5);
    if let Some(name) = named {
        let lines = Rect::new(
            tile.x0 + 4.0,
            mark.y1 + 4.0,
            tile.x1 - 4.0,
            mark.y1 + 4.0 + line_h,
        );
        crate::widget::TextLineFace::styled(name, &style, TextRole::Caption).paint(
            &mut tilo_ui_base::paint::PaintCtx::new(scene, transform, text, &on),
            lines,
            ink,
        );
    }
    // The one they work in wears their colour: the surface's own edge stands everywhere else. It
    // is the person's own colour, which the theme cannot name.
    if window.focused {
        scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            transform,
            colour,
            None,
            &tilo_ui_base::surface::edge_outline(
                tile,
                crate::widget::SurfaceLevel::Raised.radius(theme),
            ),
        );
    }
}

impl Described for ScreenMirror {
    type Props = ScreenMirrorProps;

    fn from_props(_key: &str, props: ScreenMirrorProps) -> Self {
        Self {
            colour: crate::widget::avatar::colour_of(&props.colour).unwrap_or(Color::TRANSPARENT),
            props,
            rect: Rect::ZERO,
            layout_dirty: true,
            dirty: true,
        }
    }

    fn set_props(&mut self, props: ScreenMirrorProps) -> bool {
        self.layout_dirty |= (props.height - self.props.height).abs() > f64::EPSILON;
        self.colour = crate::widget::avatar::colour_of(&props.colour).unwrap_or(Color::TRANSPARENT);
        self.props = props;
        self.dirty = true;
        true
    }
}

impl Widget for ScreenMirror {
    fn name(&self) -> &'static str {
        "ScreenMirror"
    }

    /// Whose screen it is, and each window on it by what it is called, the one they work in
    /// selected.
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        let screen = self.screen_rect();
        Some(
            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Image)
                .with_name(self.props.label.clone())
                .with_children(
                    self.props
                        .windows
                        .iter()
                        .map(|window| {
                            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Window)
                                .with_name(window.name.clone())
                                .with_bounds(Self::tile(screen, window))
                                .with_state(tilo_ui_base::a11y::AccessState {
                                    selected: window.focused,
                                    ..tilo_ui_base::a11y::AccessState::default()
                                })
                        })
                        .collect(),
                ),
        )
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a height in logical pixels"
        )]
        let height = self.props.height.max(0.0) as f32;
        taffy.widget_leaf(
            Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::length(height),
                },
                ..Style::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        self.layout_dirty = false;
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.dirty = false;
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let screen_rect = self.screen_rect();
        let colour = self.colour;
        let windows = &self.props.windows;
        crate::widget::paint_window_card(scene, transform, screen_rect, theme, |scene, on| {
            for window in windows {
                paint_window(scene, transform, text, on, screen_rect, window, colour);
            }
        });
        if self.props.followed {
            // Their colour runs the picture's whole height: whose view this is. It is the
            // person's own colour, which the theme cannot name.
            let bar = Rect::new(
                self.rect.x0 + 1.0,
                self.rect.y0 + 6.0,
                self.rect.x0 + 3.0,
                self.rect.y1 - 6.0,
            );
            scene.fill(vello::peniko::Fill::NonZero, transform, colour, None, &bar);
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    tilo_ui_base::described!();
}
