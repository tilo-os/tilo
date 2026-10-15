//! A manually painted sidebar body backed by the shared scroll controller.
//! The header/view switcher stay outside its paint and interaction viewport.

use std::collections::HashMap;

use ui::paint::PaintCtx;
use ui::theme::Theme;
use ui::widget::{AppModel, EventCtx, ModelPath, Scrollable, Widget};
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

pub(super) struct SidebarScroll {
    scroll: Scrollable,
    bar_metrics: Option<(u32, u32)>,
    gutter: f64,
}

impl SidebarScroll {
    pub(super) fn new() -> Self {
        Self {
            scroll: Scrollable::new_vertical().always_visible(),
            bar_metrics: None,
            gutter: 0.0,
        }
    }

    /// Configure the shared bar's themed metrics without registering a child
    /// in the app's hand-positioned layout tree. The scratch node is needed
    /// only when its metrics change; content is painted by `sidebar_view`.
    pub(super) fn content_rect(&mut self, viewport: Rect, theme: &Theme) -> Rect {
        let mut scroll_theme = theme.clone();
        // Overlay bars stay quiet at accessibility scales; row targets grow,
        // but a desktop thumb does not need to become a bright 12px rail.
        scroll_theme.tokens.metrics.scroll_thumb = 6.0;
        scroll_theme.tokens.metrics.scrollbar_gutter = 10.0;
        let metrics = &scroll_theme.tokens.metrics;
        let key = (
            metrics.scroll_thumb.to_bits(),
            metrics.scrollbar_gutter.to_bits(),
        );
        if self.bar_metrics != Some(key) {
            let _ = self.scroll.build_layout(
                &mut taffy::TaffyTree::new(),
                &scroll_theme,
                &mut HashMap::new(),
            );
            self.bar_metrics = Some(key);
        }
        self.gutter = f64::from(metrics.scrollbar_gutter);
        self.scroll.set_viewport_rect(viewport);
        // A stable lane keeps the date grid's height independent of overflow.
        Rect::new(
            viewport.x0,
            viewport.y0,
            (viewport.x1 - self.gutter).max(viewport.x0),
            viewport.y1,
        )
    }

    pub(super) fn set_content_height(&mut self, height: f64) {
        let viewport = self.scroll.get_rect();
        self.scroll.set_content_size(
            ui::num::f32_of_f64(viewport.width().max(0.0)),
            ui::num::f32_of_f64(height.max(0.0)),
        );
    }

    /// Bring an offscreen semantic row into the same viewport as pointer
    /// navigation, without exposing its clipped area as a clickable target.
    pub(super) fn reveal(&mut self, rect: Rect) {
        let viewport = self.scroll.get_rect();
        let delta = if rect.y0 < viewport.y0 {
            rect.y0 - viewport.y0
        } else if rect.y1 > viewport.y1 {
            rect.y1 - viewport.y1
        } else {
            0.0
        };
        self.scroll.scroll_by(0.0, ui::num::f32_of_f64(delta));
    }

    pub(super) fn offset_y(&self) -> f64 {
        f64::from(self.scroll.offset_y())
    }

    /// Never return a target for a painted-but-clipped row or the bar lane.
    pub(super) fn clip_hit(&self, rect: Rect) -> Option<Rect> {
        let viewport = self.scroll.get_rect();
        let content = Rect::new(
            viewport.x0,
            viewport.y0,
            (viewport.x1 - self.gutter).max(viewport.x0),
            viewport.y1,
        );
        let hit = rect.intersect(content);
        (hit.width() > 0.0 && hit.height() > 0.0).then_some(hit)
    }

    /// Run before the generic hit map. The shared bar owns a track/thumb
    /// press and its eventual release, including drags outside the sidebar.
    pub(super) fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        self.scroll.handle_event(event, ctx);
        *ctx.handled
    }

    pub(super) fn contains(&self, point: Point) -> bool {
        self.scroll.get_rect().contains(point)
    }

    pub(super) fn draw_bar(&self, ctx: &mut PaintCtx<'_>) {
        self.scroll
            .draw_bar_overlay(ctx.scene, ctx.transform, ctx.theme);
    }

    /// The manually hosted widget still needs its ordinary frame lifecycle:
    /// sync advances a held track; update settles wheel momentum.
    pub(super) fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.scroll.sync(state, path);
        self.scroll.update(state, path);
    }

    pub(super) fn wants_pointer_capture(&self) -> bool {
        self.scroll.wants_pointer_capture()
    }

    pub(super) fn needs_redraw(&self) -> bool {
        self.scroll.needs_redraw()
    }

    pub(super) fn needs_animation_frame(&self) -> bool {
        self.scroll.needs_animation_frame()
    }

    pub(super) fn mark_drawn(&mut self) {
        self.scroll.mark_drawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sidebar() -> SidebarScroll {
        let mut sidebar = SidebarScroll::new();
        sidebar.content_rect(Rect::new(0.0, 180.0, 280.0, 810.0), &Theme::dark());
        sidebar
    }

    #[test]
    fn final_calendars_remain_reachable_below_task_navigation() {
        use super::super::{Action, CalendarApp};
        use crate::model::ListSel;
        use ui::synthetic_input::{cursor_moved, mouse_wheel};
        use ui::testing::dispatch;
        use vello::Scene;
        use vello::kurbo::Affine;

        let theme = Theme::dark().with_ui_scale(1.5);
        let mut app = CalendarApp::new_demo();
        app.detail_open = false;
        app.view = crate::model::View::Tasks;
        app.list_sel = Some(ListSel::Today);
        app.rect = Rect::new(0.0, 0.0, 1440.0, 500.0);
        app.layout_theme = theme.clone();
        app.relayout();
        let mut text = ui::text_render::TextRenderer::new();
        let paint = |app: &mut CalendarApp, text: &mut ui::text_render::TextRenderer| {
            let mut scene = Scene::new();
            app.draw_sidebar(&mut PaintCtx::new(
                &mut scene,
                Affine::IDENTITY,
                text,
                &theme,
            ))
        };
        let before = paint(&mut app, &mut text);
        let final_calendar = app
            .accounts
            .accounts
            .last()
            .unwrap()
            .calendars
            .last()
            .unwrap()
            .id
            .clone();
        assert!(!before.iter().any(|(_, action)| {
            matches!(action, Action::SetCalendarVisible { id, .. } if *id == final_calendar)
        }));
        assert!(!app.accounts.accounts.is_empty());
        assert!(!app.model.tasks_for(&ListSel::Today, app.today).is_empty());
        let body = app.sidebar_scroll.scroll.get_rect();
        dispatch(&mut app, &cursor_moved(body.center().x, body.center().y));
        assert!(dispatch(&mut app, &mouse_wheel(0.0, -1000.0)));
        let after = paint(&mut app, &mut text);
        assert!(app.sidebar_scroll.offset_y() > 0.0);
        assert!(after.iter().any(|(_, action)| {
            matches!(action, Action::SetCalendarVisible { id, .. } if *id == final_calendar)
        }));
    }

    #[test]
    fn final_content_is_reachable_below_fixed_chrome() {
        let mut sidebar = sidebar();
        sidebar.set_content_height(1600.0);
        sidebar.scroll.scroll_by(0.0, f32::MAX);
        let bottom = 180.0 + 1600.0 - sidebar.offset_y();
        assert!((bottom - 810.0).abs() < 0.01);
        assert!(
            sidebar
                .clip_hit(Rect::new(0.0, bottom - 40.0, 280.0, bottom))
                .is_some()
        );
        assert!(
            sidebar
                .clip_hit(Rect::new(0.0, 120.0, 280.0, 180.0))
                .is_none()
        );
    }

    #[test]
    fn clipped_targets_cannot_overlap_chrome_or_scrollbar() {
        let mut sidebar = sidebar();
        sidebar.set_content_height(1600.0);
        let hit = sidebar
            .clip_hit(Rect::new(0.0, 160.0, 280.0, 210.0))
            .unwrap();
        assert_eq!(hit.y0, 180.0);
        assert_eq!(hit.y1, 210.0);
        assert!(hit.x1 < 280.0);
        assert!(
            sidebar
                .clip_hit(Rect::new(hit.x1, 180.0, 280.0, 210.0))
                .is_none()
        );
        assert!(
            sidebar
                .clip_hit(Rect::new(0.0, 810.0, 280.0, 850.0))
                .is_none()
        );
    }

    #[test]
    fn scrollbar_drag_owns_press_motion_and_release_outside_the_body() {
        use ui::synthetic_input::{cursor_moved, mouse_input};
        use ui::testing::dispatch;
        use winit::event::{ElementState, MouseButton};

        let mut sidebar = sidebar();
        sidebar.set_content_height(1600.0);
        let x = 280.0 - sidebar.gutter / 2.0;
        dispatch(&mut sidebar.scroll, &cursor_moved(x, 200.0));
        assert!(dispatch(
            &mut sidebar.scroll,
            &mouse_input(ElementState::Pressed, MouseButton::Left),
        ));
        assert!(sidebar.wants_pointer_capture());
        assert!(dispatch(&mut sidebar.scroll, &cursor_moved(400.0, 500.0)));
        assert!(sidebar.offset_y() > 0.0);
        assert!(dispatch(
            &mut sidebar.scroll,
            &mouse_input(ElementState::Released, MouseButton::Left),
        ));
        assert!(!sidebar.wants_pointer_capture());
        // The entire bar lane is outside all row targets, not just its thumb.
        assert!(
            !sidebar
                .clip_hit(Rect::new(0.0, 180.0, 280.0, 810.0))
                .unwrap()
                .contains(Point::new(x, 200.0))
        );
    }

    #[test]
    fn collapsing_content_clamps_a_previous_scroll() {
        let mut sidebar = sidebar();
        sidebar.set_content_height(1600.0);
        sidebar.scroll.scroll_by(0.0, f32::MAX);
        assert!(sidebar.offset_y() > 0.0);
        sidebar.set_content_height(300.0);
        assert_eq!(sidebar.offset_y(), 0.0);
    }
}
