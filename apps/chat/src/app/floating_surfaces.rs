//! The floating surface Chat hosts: the call's huddle, out of its window and
//! over the desk, with what the host says back about it (adopted, suspended,
//! closed, reattached).

use automerge::ObjId;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{FloatingSurfaceProvider, Widget};
use ui::winit::event::WindowEvent;
use vello::Scene;
use vello::kurbo::Affine;

use super::ChatApp;

impl FloatingSurfaceProvider for ChatApp {
    fn floating_surface_accessibility(&self, id: &str) -> Option<ui::a11y::AccessNode> {
        (id == crate::live::activity(&self.call.channel))
            .then(|| self.call_card_accessibility())
            .flatten()
    }

    fn perform_floating_surface_accessibility_action(
        &mut self,
        id: &str,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        _path: &ObjId,
    ) -> bool {
        id == crate::live::activity(&self.call.channel)
            && self.activate_call_card(target, action, state)
    }

    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.pending_floating.take()
    }

    fn draw_floating_surfaces(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.call.active
            || self.call.expanded
            // The card's own window draws the call once it is up: this
            // window keeps its copy only for the transfer and the way back.
            || (self.call.placement.is_desktop_adopted()
                && !self.call.placement.is_return_pending())
            || (!self.call.placement.is_return_pending()
                && !matches!(
                    self.call.placement.attachment(),
                    ui::floating_surface::Attachment::Desktop { .. }
                ))
        {
            return;
        }
        let mut hits = Vec::new();
        self.draw_live_huddle(scene, transform, text_renderer, theme, presence, &mut hits);
        self.hits.extend(hits);
    }

    fn draw_floating_surface(
        &mut self,
        id: &str,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> bool {
        if id != crate::live::activity(&self.call.channel) || !self.call.active {
            return false;
        }
        // The card is a window of its own, so its frame keeps its own hits:
        // the window's list describes the window's last frame, not this one.
        let mut hits = Vec::new();
        self.draw_live_huddle(scene, transform, text_renderer, theme, presence, &mut hits);
        self.card_hits = hits;
        true
    }

    fn floating_surface_presentations(
        &self,
    ) -> Vec<ui::floating_surface::FloatingSurfacePresentation> {
        let returning_to_app = self.call.placement.is_return_pending();
        if !self.call.active
            || self.call.expanded
            || (!returning_to_app
                && !matches!(
                    self.call.placement.attachment(),
                    ui::floating_surface::Attachment::Desktop { .. }
                ))
        {
            return Vec::new();
        }
        // Over the desk the card draws the call in the place the layer gives
        // it, inside its own shadow margin; in its window the huddle keeps
        // its title bar, chips and controls around the video.
        let card = !returning_to_app
            && matches!(
                self.call.placement.attachment(),
                ui::floating_surface::Attachment::Desktop { .. }
            );
        let content = self.call_video_rect();
        let window_rect = if card {
            content.inflate(8.0, 8.0)
        } else {
            self.huddle_panel().inflate(8.0, 8.0)
        };
        vec![ui::floating_surface::FloatingSurfacePresentation {
            id: format!("chat-call:{}", self.call.channel),
            content_rect: content,
            window_rect,
            input_rect: Some(content),
            returning_to_app,
        }]
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.call
            .active
            .then(|| format!("chat-call:{}", self.call.channel))
            .into_iter()
            .collect()
    }

    fn set_floating_surface_bounds(&mut self, bounds: vello::kurbo::Rect) {
        self.floating_surface_bounds = bounds;
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
        let id = format!("chat-call:{}", self.call.channel);
        if let ui::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
            id: request_id,
            content_rect,
            transfer_covered,
            coordinate_offset,
            ..
        } = message
            && request_id == &id
            && let Ok(rect) = ui::floating_surface::protocol_rect(*content_rect)
        {
            self.call.placement.prepare_desktop_rect_with_offset(
                rect,
                vello::kurbo::Vec2::new(coordinate_offset[0], coordinate_offset[1]),
            );
            self.call
                .placement
                .set_desktop_transfer_covered(*transfer_covered);
            return;
        }
        if let ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
            id: request_id,
            rect,
        } = message
            && request_id == &id
            && ui::floating_surface::protocol_rect(*rect).is_ok()
        {
            self.call.placement.confirm_desktop_adoption();
            return;
        }
        if matches!(
            message,
            ui::floating_surface::FloatingSurfaceHostMessage::Reattach { id: request_id }
                | ui::floating_surface::FloatingSurfaceHostMessage::Closed { id: request_id }
                if request_id == &id
        ) {
            let app = self.rect;
            let desktop = self.desktop_floating_bounds();
            self.call.placement.reattach_to_app(app, desktop);
            // The card's window went away: the call asks for it again.
            if matches!(
                message,
                ui::floating_surface::FloatingSurfaceHostMessage::Reattach { .. }
            ) {
                self.presented_call = None;
            }
            return;
        }
        if matches!(
            message,
            ui::floating_surface::FloatingSurfaceHostMessage::Suspended { id: request_id }
                if request_id == &id
        ) {
            self.call.placement.confirm_app_adoption(self.rect);
        }
    }

    fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
        self.input_floating_surface = id.map(str::to_owned);
    }

    fn floating_surface_event(
        &mut self,
        id: &str,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        // The pointer stands on the card, in the desk's coordinates: the
        // frame the card's hits were recorded in, and the frame the app's
        // card-owned input reads.
        let previous = self.input_floating_surface.replace(id.to_owned());
        <Self as Widget>::handle_event(self, event, ctx);
        self.input_floating_surface = previous;
        *ctx.handled
    }
}

impl ChatApp {
    /// The card over the desk as assistive technology reads it: who the call
    /// is with and for how long, how it is connected, and the controls the
    /// card draws, where it draws them.
    fn call_card_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        if !self.call.active || self.card_hits.is_empty() {
            return None;
        }
        let mut children = vec![self.call_reading(self.call_video_rect())];
        children.extend(
            Self::deduplicate_semantic_hits(self.semantic_hits_from(&self.card_hits))
                .into_iter()
                .map(|(rect, hit, name, role, selected)| {
                    let mut state = AccessState::new().focusable(true);
                    if matches!(role, Role::ToggleButton) {
                        state = state.checked(selected);
                    }
                    AccessNode::new(role)
                        .with_name(name.text)
                        .with_interface_name(name.interface)
                        .with_state(state)
                        .with_bounds(rect)
                        .with_action_target(hit.semantic_id())
                }),
        );
        Some(
            AccessNode::new(Role::Group)
                .with_name(crate::i18n::t("chat-call-active"))
                .with_bounds(self.call_video_rect())
                .with_hashed_key(("call-card", &self.call.channel))
                .with_children(children),
        )
    }

    /// Press one of the card's controls, named the way the card's own
    /// accessibility names it.
    fn activate_call_card(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let Some((_, hit, ..)) =
            Self::deduplicate_semantic_hits(self.semantic_hits_from(&self.card_hits))
                .into_iter()
                .find(|(_, hit, ..)| hit.semantic_id() == target)
        else {
            return false;
        };
        self.on_hit(hit, state);
        self.request_relayout();
        true
    }
}
