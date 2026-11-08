//! The floating-surface capability of the desk's surface: what the windows'
//! apps present over the desk, forwarded to the rows that hold them.

use ui::automerge::ObjId;

use super::DesktopSurface;

impl ui::widget::FloatingSurfaceProvider for DesktopSurface {
    fn floating_surface_event(
        &mut self,
        id: &str,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        self.rows.values_mut().any(|row| {
            ui::widget::FloatingSurfaceProvider::floating_surface_event(row, id, event, ctx)
        })
    }

    fn floating_surface_accessibility(&self, id: &str) -> Option<ui::a11y::AccessNode> {
        self.rows.values().find_map(|row| {
            ui::widget::FloatingSurfaceProvider::floating_surface_accessibility(row, id)
        })
    }

    fn perform_floating_surface_accessibility_action(
        &mut self,
        id: &str,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        self.rows.values_mut().any(|row| {
            ui::widget::FloatingSurfaceProvider::perform_floating_surface_accessibility_action(
                row, id, target, action, state, path,
            )
        })
    }

    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.rows
            .values_mut()
            .find_map(ui::widget::FloatingSurfaceProvider::take_floating_surface_request)
    }

    fn draw_floating_surface(
        &mut self,
        id: &str,
        scene: &mut ui::vello::Scene,
        transform: ui::kurbo::Affine,
        text_renderer: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) -> bool {
        self.rows.values_mut().any(|row| {
            ui::widget::FloatingSurfaceProvider::draw_floating_surface(
                row,
                id,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            )
        })
    }

    fn set_floating_surface_bounds(&mut self, bounds: ui::kurbo::Rect) {
        for row in self.rows.values_mut() {
            ui::widget::FloatingSurfaceProvider::set_floating_surface_bounds(row, bounds);
        }
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.rows
            .values()
            .flat_map(ui::widget::FloatingSurfaceProvider::active_floating_surface_ids)
            .collect()
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
        for row in self.rows.values_mut() {
            ui::widget::FloatingSurfaceProvider::on_floating_surface_host_message(row, message);
        }
    }
}
