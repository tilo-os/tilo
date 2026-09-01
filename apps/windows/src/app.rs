//! Immediate-mode `QuickUI` surface for compatibility routing.
use crate::actions;
use crate::model::{Catalogue, Route};
use crate::provider::{CompatibilityProvider, EmptyProvider, SampleProvider};
use crate::strings;
use automerge::{AutoCommit, ObjId};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonStyle, Widget};
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
    route_buttons: Vec<Button>,
    reset_button: Button,
    route_signal: Rc<Cell<Option<Route>>>,
    reset_signal: Rc<Cell<bool>>,
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
        let route_signal = Rc::new(Cell::new(None));
        let reset_signal = Rc::new(Cell::new(false));
        let route_buttons = Route::ALL
            .iter()
            .map(|route| {
                let signal = Rc::clone(&route_signal);
                let route = *route;
                Button::new(
                    &format!("windows-route-{}", route.short().to_ascii_lowercase()),
                    route.button_key(),
                    None,
                )
                .with_size(108.0, 32.0)
                .on_click(move || signal.set(Some(route)))
            })
            .collect();
        let reset = Rc::clone(&reset_signal);
        Self {
            provider,
            catalogue,
            selected: 0,
            area: Rect::ZERO,
            mouse: Point::ZERO,
            hits: Vec::new(),
            node: None,
            notice: None,
            route_buttons,
            reset_button: Button::new(
                "windows-use-recommendation",
                "windows-use-recommendation",
                None,
            )
            .with_size(195.0, 32.0)
            .on_click(move || reset.set(true)),
            route_signal,
            reset_signal,
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
        }
    }
    fn apply_control_signals(&mut self) {
        if let Some(route) = self.route_signal.take()
            && let Some(id) = self.selected_app().map(|app| app.id.clone())
        {
            self.notice = self.provider.set_route_override(&id, route).err();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-route-set"));
            }
            self.refresh();
        }
        if self.reset_signal.replace(false)
            && let Some(id) = self.selected_app().map(|app| app.id.clone())
        {
            self.notice = self.provider.reset_route_override(&id).err();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-route-reset"));
            }
            self.refresh();
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
        presence: &ui::presence::PresenceCtx,
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
            &strings::t("windows-app-title"),
            21.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        Self::text(
            tr,
            scene,
            t,
            Point::new(sidebar.x0 + PAD, sidebar.y0 + 53.0),
            &strings::t("windows-catalogue"),
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
                &strings::t("windows-empty-title"),
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
                &strings::t("windows-empty-body"),
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
            &strings::t("windows-why-route"),
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
            &strings::t("windows-open-with"),
            14.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        for (index, route) in Route::ALL.iter().enumerate() {
            let x = detail.x0 + 94.0 + index as f64 * 118.0;
            let button = Rect::new(x, routes_y - 8.0, x + 108.0, routes_y + 24.0);
            if let Some(shared_button) = self.route_buttons.get_mut(index) {
                shared_button.apply_layout_external(button);
                shared_button.set_style(if *route == app.route {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Neutral
                });
                shared_button.draw(scene, t, tr, theme, presence);
            }
        }
        if app.uses_override() {
            let reset = Rect::new(
                detail.x0 + 460.0,
                routes_y - 8.0,
                detail.x0 + 655.0,
                routes_y + 24.0,
            );
            self.reset_button.apply_layout_external(reset);
            self.reset_button.set_style(ButtonStyle::Neutral);
            self.reset_button.draw(scene, t, tr, theme, presence);
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
            &strings::t("windows-capsule-access"),
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
            &strings::t("windows-runner"),
            &app.runner,
            theme,
        );
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 71.0,
            &strings::t("windows-capsule"),
            &app.capsule,
            theme,
        );
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 96.0,
            &strings::t("windows-private-data"),
            &app.prefix,
            theme,
        );
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 121.0,
            &strings::t("windows-host-access"),
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
        scale: f64,
        mods: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus: &mut Option<NodeId>,
        node: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        for button in &mut self.route_buttons {
            button.handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        }
        self.reset_button
            .handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        self.apply_control_signals();
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
    fn test_rects(&self) -> Vec<(String, Rect)> {
        let mut rects = self
            .route_buttons
            .iter()
            .map(|button| {
                (
                    button.test_id().unwrap_or_default().to_string(),
                    button.get_rect(),
                )
            })
            .collect::<Vec<_>>();
        rects.push((
            "windows-use-recommendation".into(),
            self.reset_button.get_rect(),
        ));
        rects
    }
    fn children(&self) -> Vec<&dyn Widget> {
        let mut children = self
            .route_buttons
            .iter()
            .map(|button| button as &dyn Widget)
            .collect::<Vec<_>>();
        children.push(&self.reset_button);
        children
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children = self
            .route_buttons
            .iter_mut()
            .map(|button| button as &mut dyn Widget)
            .collect::<Vec<_>>();
        children.push(&mut self.reset_button);
        children
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn override_updates_runner_without_changing_capsule_identity() {
        let mut app = WindowsApp::new_offline();
        let capsule = app.catalogue.apps[0].capsule.clone();
        app.route_signal.set(Some(Route::Vm));
        app.apply_control_signals();
        assert_eq!(app.catalogue.apps[0].route, Route::Vm);
        assert_eq!(app.catalogue.apps[0].capsule, capsule);
    }

    #[test]
    fn live_manager_does_not_claim_fixture_apps_are_installed() {
        assert!(WindowsApp::new_live().catalogue.apps.is_empty());
    }

    #[test]
    fn shared_route_controls_have_stable_automation_ids() {
        let app = WindowsApp::new_offline();
        assert_eq!(Widget::test_id(&app), Some("windows-app"));
        let ids = app
            .children()
            .iter()
            .filter_map(|child| child.test_id())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![
                "windows-route-wine",
                "windows-route-proton",
                "windows-route-vm",
                "windows-use-recommendation",
            ]
        );
    }
}
