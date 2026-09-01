//! Immediate-mode `QuickUI` surface for compatibility routing.
use crate::actions;
use crate::model::{Catalogue, Route};
use crate::provider::{CompatibilityProvider, EmptyProvider, SampleProvider};
use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

const PAD: f64 = 20.0;
const SIDEBAR_W: f64 = 296.0;
const ROW_H: f64 = 72.0;

#[derive(Clone, Copy)]
enum Hit {
    App(usize),
    Route(Route),
    Reset,
}

pub struct WindowsApp {
    provider: Box<dyn CompatibilityProvider>,
    catalogue: Catalogue,
    selected: usize,
    area: Rect,
    mouse: Point,
    hits: Vec<(Rect, Hit)>,
    node: Option<NodeId>,
    notice: Option<String>,
}

impl WindowsApp {
    pub fn new_live() -> Self {
        Self::with_provider(Box::new(EmptyProvider))
    }
    pub fn new_offline() -> Self {
        Self::with_provider(Box::new(SampleProvider::new()))
    }
    pub fn with_provider(provider: Box<dyn CompatibilityProvider>) -> Self {
        let catalogue = provider.catalogue();
        Self {
            provider,
            catalogue,
            selected: 0,
            area: Rect::ZERO,
            mouse: Point::ZERO,
            hits: Vec::new(),
            node: None,
            notice: None,
        }
    }
    fn selected_app(&self) -> Option<&crate::model::WindowsApp> {
        self.catalogue.apps.get(self.selected)
    }
    fn refresh(&mut self) {
        self.catalogue = self.provider.catalogue();
        if self.selected >= self.catalogue.apps.len() {
            self.selected = 0;
        }
    }
    fn hit(&mut self, hit: Hit) {
        match hit {
            Hit::App(index) => {
                self.selected = index;
                self.notice = None;
            }
            Hit::Route(route) => {
                if let Some(id) = self.selected_app().map(|app| app.id.clone()) {
                    self.notice = self.provider.set_route_override(&id, route).err();
                    if self.notice.is_none() {
                        self.notice = Some(format!(
                            "Route set to {}. Launches still require local confirmation.",
                            route.label()
                        ));
                    }
                    self.refresh();
                }
            }
            Hit::Reset => {
                if let Some(id) = self.selected_app().map(|app| app.id.clone()) {
                    self.notice = self.provider.reset_route_override(&id).err();
                    if self.notice.is_none() {
                        self.notice = Some("Using the catalogue recommendation again.".into());
                    }
                    self.refresh();
                }
            }
        }
    }
    fn fill(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
        scene.fill(Fill::NonZero, t, color, None, &rect);
    }
    fn pill(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
        scene.fill(
            Fill::NonZero,
            t,
            color,
            None,
            &RoundedRect::from_rect(rect, 8.0),
        );
    }
    fn text(
        tr: &mut TextRenderer,
        scene: &mut Scene,
        t: Affine,
        at: Point,
        text: &str,
        size: f32,
        color: Color,
        weight: TextWeight,
    ) {
        tr.draw_text_weighted(
            scene,
            text,
            size,
            color,
            t * Affine::translate((at.x, at.y)),
            None,
            weight,
        );
    }
    fn line(
        tr: &mut TextRenderer,
        scene: &mut Scene,
        t: Affine,
        x: f64,
        y: f64,
        label: &str,
        value: &str,
        theme: &Theme,
    ) {
        Self::text(
            tr,
            scene,
            t,
            Point::new(x, y),
            label,
            12.0,
            theme.text_dim,
            TextWeight::Regular,
        );
        Self::text(
            tr,
            scene,
            t,
            Point::new(x + 148.0, y),
            value,
            13.0,
            theme.text_primary,
            TextWeight::Regular,
        );
    }
}

impl Widget for WindowsApp {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        self.refresh();
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap_or_else(|_| NodeId::from(0_u64));
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.area = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
    }
    #[allow(
        clippy::too_many_lines,
        reason = "the single canvas widget keeps paint order and hit geometry together"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        Self::fill(scene, t, self.area, theme.bg_app);
        self.hits.clear();
        let sidebar = Rect::new(
            self.area.x0,
            self.area.y0,
            (self.area.x0 + SIDEBAR_W).min(self.area.x1),
            self.area.y1,
        );
        Self::fill(scene, t, sidebar, theme.bg_primary);
        Self::text(
            tr,
            scene,
            t,
            Point::new(sidebar.x0 + PAD, sidebar.y0 + 26.0),
            "Windows Apps",
            21.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        Self::text(
            tr,
            scene,
            t,
            Point::new(sidebar.x0 + PAD, sidebar.y0 + 53.0),
            "Compatibility catalogue",
            12.0,
            theme.text_dim,
            TextWeight::Regular,
        );
        let mut y = sidebar.y0 + 82.0;
        for (index, app) in self.catalogue.apps.iter().enumerate() {
            let row = Rect::new(sidebar.x0 + 10.0, y, sidebar.x1 - 10.0, y + ROW_H - 6.0);
            if index == self.selected {
                Self::pill(scene, t, row, theme.bg_active);
            }
            let badge = Rect::new(row.x0 + 10.0, row.y0 + 14.0, row.x0 + 68.0, row.y0 + 35.0);
            Self::pill(
                scene,
                t,
                badge,
                if app.route == Route::Vm {
                    theme.border_standard
                } else {
                    theme.accent_primary
                },
            );
            Self::text(
                tr,
                scene,
                t,
                Point::new(badge.x0 + 7.0, badge.y0 + 4.0),
                app.route.short(),
                10.0,
                theme.text_primary,
                TextWeight::Bold,
            );
            Self::text(
                tr,
                scene,
                t,
                Point::new(row.x0 + 78.0, row.y0 + 10.0),
                &app.name,
                13.0,
                theme.text_primary,
                TextWeight::Bold,
            );
            Self::text(
                tr,
                scene,
                t,
                Point::new(row.x0 + 78.0, row.y0 + 31.0),
                app.status.label(),
                11.0,
                theme.text_dim,
                TextWeight::Regular,
            );
            self.hits.push((row, Hit::App(index)));
            y += ROW_H;
        }
        let detail = Rect::new(
            sidebar.x1 + PAD,
            self.area.y0 + PAD,
            self.area.x1 - PAD,
            self.area.y1 - PAD,
        );
        let Some(app) = self.selected_app().cloned() else {
            Self::text(
                tr,
                scene,
                t,
                Point::new(detail.x0, detail.y0 + 8.0),
                "No Windows apps yet",
                28.0,
                theme.text_primary,
                TextWeight::Bold,
            );
            Self::text(
                tr,
                scene,
                t,
                Point::new(detail.x0, detail.y0 + 44.0),
                &self.catalogue.vm.detail,
                13.0,
                theme.text_dim,
                TextWeight::Regular,
            );
            Self::text(
                tr,
                scene,
                t,
                Point::new(detail.x0, detail.y0 + 92.0),
                "Installed Windows software and catalogue recommendations will appear here.",
                13.0,
                theme.text_dim,
                TextWeight::Regular,
            );
            return;
        };
        Self::text(
            tr,
            scene,
            t,
            Point::new(detail.x0, detail.y0 + 8.0),
            &app.name,
            28.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        Self::text(
            tr,
            scene,
            t,
            Point::new(detail.x0, detail.y0 + 44.0),
            &format!(
                "{} · {} · {}",
                app.publisher,
                app.catalogue_source,
                app.confidence.label()
            ),
            13.0,
            theme.text_dim,
            TextWeight::Regular,
        );
        let status = Rect::new(
            detail.x0,
            detail.y0 + 70.0,
            detail.x0 + 128.0,
            detail.y0 + 98.0,
        );
        Self::pill(scene, t, status, theme.accent_primary);
        Self::text(
            tr,
            scene,
            t,
            Point::new(status.x0 + 10.0, status.y0 + 6.0),
            &format!("{} · {}", app.route.short(), app.status.label()),
            11.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        let card = Rect::new(detail.x0, detail.y0 + 118.0, detail.x1, detail.y0 + 238.0);
        Self::pill(scene, t, card, theme.bg_primary);
        Self::text(
            tr,
            scene,
            t,
            Point::new(card.x0 + PAD, card.y0 + 16.0),
            "Why this opens here",
            14.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        Self::text(
            tr,
            scene,
            t,
            Point::new(card.x0 + PAD, card.y0 + 44.0),
            &app.reason,
            13.0,
            theme.text_dim,
            TextWeight::Regular,
        );
        Self::text(
            tr,
            scene,
            t,
            Point::new(card.x0 + PAD, card.y0 + 74.0),
            app.route.security_summary(),
            12.0,
            theme.text_dim,
            TextWeight::Regular,
        );
        let routes_y = card.y1 + 22.0;
        Self::text(
            tr,
            scene,
            t,
            Point::new(detail.x0, routes_y),
            "Open with",
            14.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        for (index, route) in Route::ALL.iter().enumerate() {
            let x = detail.x0 + 94.0 + index as f64 * 118.0;
            let button = Rect::new(x, routes_y - 8.0, x + 108.0, routes_y + 24.0);
            Self::pill(
                scene,
                t,
                button,
                if *route == app.route {
                    theme.accent_primary
                } else {
                    theme.bg_primary
                },
            );
            Self::text(
                tr,
                scene,
                t,
                Point::new(button.x0 + 11.0, button.y0 + 8.0),
                route.label(),
                12.0,
                theme.text_primary,
                TextWeight::Bold,
            );
            self.hits.push((button, Hit::Route(*route)));
        }
        if app.uses_override() {
            let reset = Rect::new(
                detail.x0 + 460.0,
                routes_y - 8.0,
                detail.x0 + 655.0,
                routes_y + 24.0,
            );
            Self::pill(scene, t, reset, theme.bg_primary);
            Self::text(
                tr,
                scene,
                t,
                Point::new(reset.x0 + 10.0, reset.y0 + 8.0),
                "Use recommendation",
                12.0,
                theme.text_primary,
                TextWeight::Regular,
            );
            self.hits.push((reset, Hit::Reset));
        }
        let info = Rect::new(
            detail.x0,
            routes_y + 48.0,
            detail.x1,
            (routes_y + 226.0).min(detail.y1),
        );
        Self::pill(scene, t, info, theme.bg_primary);
        Self::text(
            tr,
            scene,
            t,
            Point::new(info.x0 + PAD, info.y0 + 16.0),
            "Capsule and access",
            14.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 46.0,
            "Runner",
            &app.runner,
            theme,
        );
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 71.0,
            "Capsule",
            &app.capsule,
            theme,
        );
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 96.0,
            "Private data",
            &app.prefix,
            theme,
        );
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 121.0,
            "Host access",
            &app.access_summary(),
            theme,
        );
        Self::text(
            tr,
            scene,
            t,
            Point::new(info.x0 + PAD, info.y0 + 151.0),
            &app.fallback_detail,
            12.0,
            theme.text_dim,
            TextWeight::Regular,
        );
        if let Some(notice) = &self.notice {
            Self::text(
                tr,
                scene,
                t,
                Point::new(detail.x0, detail.y1 - 20.0),
                notice,
                12.0,
                theme.status_success,
                TextWeight::Regular,
            );
        }
    }
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        _scale: f64,
        _mods: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus: &mut Option<NodeId>,
        _node: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = Point::new(position.x, position.y);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(hit) = self
                    .hits
                    .iter()
                    .find(|(rect, _)| rect.contains(self.mouse))
                    .map(|(_, hit)| *hit)
                {
                    self.hit(hit);
                    *handled = true;
                }
            }
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "WindowsApp"
    }
    fn get_rect(&self) -> Rect {
        self.area
    }
    fn resources(&self, _doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        actions::provide_resources(&self.catalogue)
    }
    fn test_id(&self) -> Option<&str> {
        Some("windows-app")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn override_updates_runner_without_changing_capsule_identity() {
        let mut app = WindowsApp::new_offline();
        let capsule = app.catalogue.apps[0].capsule.clone();
        app.hit(Hit::Route(Route::Vm));
        assert_eq!(app.catalogue.apps[0].route, Route::Vm);
        assert_eq!(app.catalogue.apps[0].capsule, capsule);
    }

    #[test]
    fn live_manager_does_not_claim_fixture_apps_are_installed() {
        assert!(WindowsApp::new_live().catalogue.apps.is_empty());
    }
}
