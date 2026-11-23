//! The floating-surface capability of the shell's surface: what the windows'
//! apps present over the shell, forwarded to the rows that hold them.

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

    fn draw_floating_surface(&mut self, id: &str, ctx: &mut tilo_ui::DrawCtx<'_>) -> bool {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.rows.values_mut().any(|row| {
            ui::widget::FloatingSurfaceProvider::draw_floating_surface(
                row,
                id,
                &mut tilo_ui::DrawCtx::new(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    env.clone(),
                ),
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

    fn floating_surface_presentations(
        &self,
    ) -> Vec<ui::floating_surface::FloatingSurfacePresentation> {
        self.rows
            .values()
            .flat_map(ui::widget::FloatingSurfaceProvider::floating_surface_presentations)
            .collect()
    }
}
