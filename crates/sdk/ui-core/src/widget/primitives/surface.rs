//! `Surface(level)`: one of the design language's five surface levels,
//! painted with its edge and its shadow (`docs/design-language.md`,
//! "Surfaces").
//!
//! | Level | Is | Edge | Shadow | Radius |
//! |---|---|---|---|---|
//! | [`SurfaceLevel::Ground`] | the desktop | — | — | none |
//! | [`SurfaceLevel::Window`] | app content | the window edge | soft | `radii.tile` |
//! | [`SurfaceLevel::Region`] | a sidebar, an inspector | none: the step separates | — | none (flush in its window) |
//! | [`SurfaceLevel::Raised`] | a field well, a segmented track, an object | none | — | `radii.control` |
//! | [`SurfaceLevel::Floating`] | a menu, popover, palette, sheet, tooltip | 1 px inner hairline | two layers | `radii.panel` |
//!
//! A child never paints a surface of its parent's level, and never a raised
//! surface inside a raised one: a [`Surface`] nested in a surface of the
//! same level paints no ground of its own (it stands on its parent's), so
//! the rule holds by construction. Children ask [`SurfaceLevel::inner_radius`]
//! for a radius concentric with the surface they sit in; they never pick
//! one.

use std::cell::RefCell;
use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::Rhythm;
use crate::widget::{TaffyTreeExt, Widget};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SurfaceLevel {
    /// The desktop behind the windows.
    Ground,
    /// A window's content: every app's ground.
    Window,
    /// A recessed region of a window: a sidebar or an inspector.
    Region,
    /// A well inside content: a field, a segmented track, a secondary
    /// button's face, an object.
    Raised,
    /// Chrome that floats: a menu, popover, the palette, a sheet, Quick
    /// Settings, a tooltip.
    Floating,
}

impl SurfaceLevel {
    #[must_use]
    pub fn color(self, theme: &Theme) -> Color {
        let s = &theme.tokens.colors.surfaces;
        match self {
            Self::Ground => s.base,
            Self::Window => s.content,
            Self::Region => s.sidebar,
            Self::Raised => s.raised,
            Self::Floating => crate::chrome::panel_ground(theme),
        }
    }

    #[must_use]
    pub fn radius(self, theme: &Theme) -> f64 {
        let r = &theme.tokens.radii;
        f64::from(match self {
            Self::Ground | Self::Region => 0.0,
            Self::Window => r.tile,
            Self::Raised => r.control,
            Self::Floating => r.panel,
        })
    }

    /// The radius of something inset `inset` inside a surface of this
    /// level: concentric with it, never below `radii.detail`.
    #[must_use]
    pub fn inner_radius(self, theme: &Theme, inset: f64) -> f64 {
        let radii = &theme.tokens.radii;
        f64::from(radii.concentric(
            crate::num::f32_of_f64(self.radius(theme)),
            crate::num::f32_of_f64(inset),
        ))
    }

    /// The padding a [`Surface`] of this level keeps around its content.
    #[must_use]
    pub fn padding(self, theme: &Theme) -> f64 {
        let rhythm = &theme.tokens.spacing.rhythm;
        f64::from(match self {
            Self::Ground | Self::Window => rhythm.get(Rhythm::Group),
            Self::Region | Self::Floating => rhythm.get(Rhythm::Section),
            Self::Raised => rhythm.get(Rhythm::Related),
        })
    }
}

thread_local! {
    /// The levels of the [`Surface`]s being drawn, outermost first.
    static OPEN_LEVELS: RefCell<Vec<SurfaceLevel>> = const { RefCell::new(Vec::new()) };
}

/// Paint `level` over `rect`: its shadow, its fill and its edge. Returns the
/// theme a child standing on it paints with ([`Theme::on_ground`]): ink,
/// lifts and the raised well re-measured against the level's fill.
pub fn paint_surface(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    level: SurfaceLevel,
    theme: &Theme,
) -> Theme {
    let fill = level.color(theme);
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return theme.on_ground(fill);
    }
    let radius = level.radius(theme).min(rect.height() / 2.0);
    match level {
        SurfaceLevel::Ground | SurfaceLevel::Region | SurfaceLevel::Raised => {
            scene.fill(
                Fill::NonZero,
                transform,
                fill,
                None,
                &RoundedRect::from_rect(rect, radius),
            );
        }
        SurfaceLevel::Window => {
            crate::surface::shadow(
                scene,
                transform,
                rect,
                radius,
                &theme.tokens.elevation.window,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                fill,
                None,
                &RoundedRect::from_rect(rect, radius),
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                theme.window_edge(),
                None,
                &RoundedRect::from_rect(rect.inflate(0.5, 0.5), radius + 0.5),
            );
        }
        SurfaceLevel::Floating => {
            crate::surface::floating_shadow(scene, transform, rect, radius, 1.0, theme);
            crate::surface::panel(scene, transform, rect, radius, theme);
        }
    }
    theme.on_ground(fill)
}

/// A document set on a reader: a page that brings its own colours (an HTML
/// mail on its light sheet, a web page), drawn by `content` and clipped to
/// the raised level's radius, with the hairline derived from the window
/// content it stands on just inside its edge, so a sheet as light as that
/// content still has one.
pub fn paint_document_sheet(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    theme: &Theme,
    content: impl FnOnce(&mut Scene),
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let sheet = RoundedRect::from_rect(
        rect,
        SurfaceLevel::Raised.radius(theme).min(rect.height() / 2.0),
    );
    scene.push_layer(
        Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        transform,
        &sheet,
    );
    content(scene);
    scene.pop_layer();
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        sheet_edge(theme),
        None,
        &crate::surface::edge_outline(rect, sheet.radii().top_left),
    );
}

/// The hairline at a document sheet's edge: measured against the window
/// content the sheet stands on.
fn sheet_edge(theme: &Theme) -> Color {
    crate::surface::edge_on(SurfaceLevel::Window.color(theme))
}

/// Paint the see-through material over `rect` at `radius`: half the panel
/// ground over the blurred backdrop, its floating shadow outside it only (a
/// shadow under the body would darken what shows through) and the floating
/// hairline. Only for chrome over pixels the person is choosing
/// (`docs/design-language.md`, "Material and transparency"). Returns the
/// theme a child standing on it paints with.
pub fn paint_see_through(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    theme: &Theme,
) -> Theme {
    let ground = crate::chrome::panel_ground(theme);
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return theme.on_ground(ground);
    }
    let body = RoundedRect::from_rect(rect, radius);
    let mut outside = vello::kurbo::BezPath::from_vec(
        vello::kurbo::Shape::path_elements(&rect.inflate(64.0, 64.0), 0.1).collect(),
    );
    outside.extend(vello::kurbo::Shape::path_elements(&body, 0.1));
    scene.push_clip_layer(Fill::EvenOdd, transform, &outside);
    crate::surface::floating_shadow(scene, transform, rect, radius, 1.0, theme);
    scene.pop_layer();
    crate::floating_surface::paint_material(
        scene,
        transform,
        &body,
        crate::floating_surface::Material::SeeThrough,
        theme,
    );
    crate::surface::floating_edge(scene, transform, rect, radius, theme);
    theme.on_ground(ground)
}

/// A retained surface of one level around one child, padded by the
/// level's rhythm. A `Surface` inside a surface of the same level paints
/// no ground of its own.
pub struct Surface {
    level: SurfaceLevel,
    child: Box<dyn Widget>,
    padding: Option<Rhythm>,
    rect: Rect,
}

impl Surface {
    #[must_use]
    pub fn new(level: SurfaceLevel, child: Box<dyn Widget>) -> Self {
        Self {
            level,
            child,
            padding: None,
            rect: Rect::ZERO,
        }
    }

    /// Pad the content by `rhythm` instead of the level's own padding.
    #[must_use]
    pub fn padded(mut self, rhythm: Rhythm) -> Self {
        self.padding = Some(rhythm);
        self
    }

    /// The level this surface paints.
    #[must_use]
    pub fn level(&self) -> SurfaceLevel {
        self.level
    }

    /// Whether this surface paints its own ground, given the levels open
    /// around it: not when its parent surface is the same level.
    fn paints_ground(&self, open: &[SurfaceLevel]) -> bool {
        open.last() != Some(&self.level)
    }
}

impl Widget for Surface {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.child.build_layout(taffy, theme, node_map);
        let pad = self.padding.map_or_else(
            || self.level.padding(theme),
            |rhythm| f64::from(theme.tokens.spacing.rhythm.get(rhythm)),
        );
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            flex_shrink: 0.0,
            padding: crate::widget::layout::insets_all(crate::num::f32_of_f64(pad)),
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if let Some(&child) = taffy.children(node).unwrap_or_default().first() {
            self.child
                .apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let paints = OPEN_LEVELS.with(|open| self.paints_ground(&open.borrow()));
        let child_theme = if paints {
            paint_surface(scene, transform, self.rect, self.level, theme)
        } else {
            theme.clone()
        };
        OPEN_LEVELS.with(|open| open.borrow_mut().push(self.level));
        crate::widget::draw_child(
            self.child.as_mut(),
            scene,
            transform,
            text_renderer,
            &child_theme,
            presence,
        );
        OPEN_LEVELS.with(|open| {
            open.borrow_mut().pop();
        });
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let Some(&child) = ctx.taffy.children(ctx.node_id).unwrap_or_default().first() {
            self.child
                .handle_event(event, &mut ctx.reborrow(ctx.path, child));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.child.as_ref());
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.child.collect_damage(regions);
    }

    fn mark_drawn(&mut self) {
        self.child.mark_drawn();
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.child.take_structure_dirty()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.child.wants_pointer_capture()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "Surface"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_have_their_colours_radii_and_concentric_children() {
        for theme in [Theme::dark(), Theme::light()] {
            let s = &theme.tokens.colors.surfaces;
            assert_eq!(SurfaceLevel::Ground.color(&theme), s.base);
            assert_eq!(SurfaceLevel::Window.color(&theme), s.content);
            assert_eq!(SurfaceLevel::Region.color(&theme), s.sidebar);
            assert_eq!(SurfaceLevel::Raised.color(&theme), s.raised);
            assert_eq!(SurfaceLevel::Floating.color(&theme), s.overlay);
            assert!((SurfaceLevel::Floating.radius(&theme) - 18.0).abs() < 1e-6);
            assert!((SurfaceLevel::Window.radius(&theme) - 14.0).abs() < 1e-6);
            assert!((SurfaceLevel::Raised.radius(&theme) - 8.0).abs() < 1e-6);
            // A row inset 8 inside a floating panel: 18 - 8 = 10.
            assert!((SurfaceLevel::Floating.inner_radius(&theme, 8.0) - 10.0).abs() < 1e-6);
            // Never below the detail radius.
            assert!((SurfaceLevel::Raised.inner_radius(&theme, 12.0) - 4.0).abs() < 1e-6);
        }
    }

    #[test]
    fn a_raised_surface_inside_a_raised_surface_paints_no_ground() {
        let inner = Surface::new(
            SurfaceLevel::Raised,
            Box::new(crate::widget::Spacer::fixed(10.0)),
        );
        assert!(!inner.paints_ground(&[SurfaceLevel::Window, SurfaceLevel::Raised]));
        assert!(inner.paints_ground(&[SurfaceLevel::Window]));
        assert!(inner.paints_ground(&[SurfaceLevel::Raised, SurfaceLevel::Floating]));
    }

    #[test]
    fn a_document_sheet_draws_its_content_inside_its_edge() {
        for theme in [Theme::dark(), Theme::light()] {
            let page = Color::from_rgba8(250, 250, 250, 255);
            let rect = Rect::new(10.0, 10.0, 310.0, 410.0);
            let mut scene = Scene::new();
            let mut drawn = false;
            paint_document_sheet(&mut scene, Affine::IDENTITY, rect, &theme, |scene| {
                drawn = true;
                scene.fill(Fill::NonZero, Affine::IDENTITY, page, None, &rect);
            });
            assert!(drawn, "the page is drawn on its sheet");
            let colours = &scene.encoding().draw_data;
            let packed = |c: Color| c.premultiply().to_rgba8().to_u32();
            assert!(colours.contains(&packed(page)));
            assert!(colours.contains(&packed(sheet_edge(&theme))), "its edge");
        }
        let mut scene = Scene::new();
        paint_document_sheet(
            &mut scene,
            Affine::IDENTITY,
            Rect::ZERO,
            &Theme::dark(),
            |_| panic!("an empty sheet draws nothing"),
        );
    }

    #[test]
    fn a_floating_surface_paints_its_shadow_fill_and_hairline() {
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let child = paint_surface(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 200.0, 120.0),
            SurfaceLevel::Floating,
            &theme,
        );
        let colours = &scene.encoding().draw_data;
        let packed = |c: Color| c.premultiply().to_rgba8().to_u32();
        assert!(colours.contains(&packed(theme.tokens.colors.surfaces.overlay)));
        assert!(colours.contains(&packed(theme.tokens.elevation.floating_edge)));
        assert_eq!(
            child.tokens.colors.surfaces.content, theme.tokens.colors.surfaces.overlay,
            "children stand on the floating level"
        );
    }
}

/// A region of a window laid out and drawn at another density
/// ([`crate::tokens::Density`]): a data-heavy sidebar or list at compact
/// while the rest of the window stays comfortable. Rows, rhythm and tables
/// inside it take the region's density; controls and text keep their size.
pub struct DensityRegion {
    density: crate::tokens::Density,
    child: Box<dyn Widget>,
}

impl DensityRegion {
    #[must_use]
    pub fn new(density: crate::tokens::Density, child: Box<dyn Widget>) -> Self {
        Self { density, child }
    }

    #[must_use]
    pub fn compact(child: Box<dyn Widget>) -> Self {
        Self::new(crate::tokens::Density::Compact, child)
    }
}

impl Widget for DensityRegion {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let theme = theme.clone().with_density(self.density);
        self.child.build_layout(taffy, &theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.child.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let theme = theme.clone().with_density(self.density);
        crate::widget::draw_child(
            self.child.as_mut(),
            scene,
            transform,
            text_renderer,
            &theme,
            presence,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.child.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.child.as_ref());
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.child.collect_damage(regions);
    }

    fn mark_drawn(&mut self) {
        self.child.mark_drawn();
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.child.take_structure_dirty()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.child.wants_pointer_capture()
    }

    fn get_rect(&self) -> Rect {
        self.child.get_rect()
    }

    fn name(&self) -> &'static str {
        "DensityRegion"
    }
}
