//! The floating surfaces Chat hosts: an agent's conversation opened out of
//! its thread into a window of its own, with what the host says back about
//! it (adopted, suspended, closed, reattached).

use automerge::ObjId;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{FloatingSurfaceProvider, Widget};
use ui::winit::event::WindowEvent;
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::ChatApp;

impl FloatingSurfaceProvider for ChatApp {
    fn floating_surface_accessibility(&self, id: &str) -> Option<ui::a11y::AccessNode> {
        if id == crate::live::activity(&self.call.channel) {
            return self.call_card_accessibility();
        }
        self.agent_surfaces
            .values()
            .find(|surface| surface.id == id && !surface.is_closed())?
            .accessibility()
    }

    fn perform_floating_surface_accessibility_action(
        &mut self,
        id: &str,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        if id == crate::live::activity(&self.call.channel) {
            return self.activate_call_card(target, action, state);
        }
        let Some((source, child_target)) = target
            .strip_prefix("agent-surface:")
            .and_then(|target| target.split_once('|'))
        else {
            return false;
        };
        if source != id {
            return false;
        }
        let Some(surface) = self
            .agent_surfaces
            .values_mut()
            .find(|surface| surface.id == id && !surface.is_closed())
        else {
            return false;
        };
        let handled = surface
            .view
            .perform_semantic_action(child_target, action, state, path);
        self.settle_agent_surface_commands();
        handled
    }

    fn floating_surface_ime(&self, id: &str) -> Option<(ui::ime::InputContent, Rect)> {
        let surface = self
            .agent_surfaces
            .values()
            .find(|surface| surface.id == id && !surface.is_closed())?;
        Some((surface.view.ime_content()?, surface.view.ime_cursor_rect()?))
    }

    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.agent_surface_requests
            .pop_front()
            .or_else(|| self.pending_floating.take())
    }

    fn draw_floating_surfaces(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let desktop = self.desktop_floating_bounds();
        for surface in self.agent_surfaces.values_mut() {
            if surface.is_closed() {
                continue;
            }
            surface.observe_theme(theme);
            surface.view.perms = self.perms.clone();
            surface
                .view
                .workspace_agents
                .clone_from(&self.workspace_agents);
            surface
                .view
                .workspace_names
                .clone_from(&self.workspace_names);
            surface
                .view
                .workspace_persons
                .clone_from(&self.workspace_persons);
            surface
                .view
                .workspace_labels
                .clone_from(&self.workspace_labels);
            surface.view.own_label.clone_from(&self.own_label);
            surface
                .view
                .workspace_colors
                .clone_from(&self.workspace_colors);
            surface.view.messages.clone_from(&self.messages);
            surface.view.channels.clone_from(&self.channels);
            if matches!(
                surface.placement.attachment(),
                ui::floating_surface::Attachment::Desktop { .. }
            ) || surface.placement.is_return_pending()
            {
                let content = surface.placement.rect(surface.source, desktop);
                surface.view.rect = content;
                let view = &mut surface.view;
                ui::floating_surface::paint_app_owned_content(
                    scene,
                    transform,
                    content,
                    theme,
                    |scene| {
                        view.draw(scene, transform, text_renderer, theme, presence);
                        true
                    },
                );
            }
        }
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
        let desktop = self.desktop_floating_bounds();
        if let Some(surface) = self
            .agent_surfaces
            .values_mut()
            .find(|surface| surface.id == id)
        {
            if surface.is_closed() {
                return false;
            }
            surface.observe_theme(theme);
            surface.view.perms = self.perms.clone();
            surface
                .view
                .workspace_agents
                .clone_from(&self.workspace_agents);
            surface
                .view
                .workspace_names
                .clone_from(&self.workspace_names);
            surface
                .view
                .workspace_persons
                .clone_from(&self.workspace_persons);
            surface
                .view
                .workspace_labels
                .clone_from(&self.workspace_labels);
            surface.view.own_label.clone_from(&self.own_label);
            surface
                .view
                .workspace_colors
                .clone_from(&self.workspace_colors);
            surface.view.messages.clone_from(&self.messages);
            surface.view.channels.clone_from(&self.channels);
            let content = surface.placement.rect(surface.source, desktop);
            surface.view.rect = content;
            let view = &mut surface.view;
            return ui::floating_surface::paint_app_owned_content(
                scene,
                transform,
                content,
                theme,
                |scene| {
                    view.draw(scene, transform, text_renderer, theme, presence);
                    true
                },
            );
        }
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
        let mut presentations: Vec<_> = self
            .agent_surfaces
            .values()
            .filter_map(|surface| surface.presentation(self.desktop_floating_bounds()))
            .collect();
        let returning_to_app = self.call.placement.is_return_pending();
        if !self.call.active
            || self.call.expanded
            || (!returning_to_app
                && !matches!(
                    self.call.placement.attachment(),
                    ui::floating_surface::Attachment::Desktop { .. }
                ))
        {
            return presentations;
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
        presentations.push(ui::floating_surface::FloatingSurfacePresentation {
            id: format!("chat-call:{}", self.call.channel),
            content_rect: content,
            window_rect,
            input_rect: Some(content),
            returning_to_app,
        });
        presentations
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        let mut ids = self
            .agent_surfaces
            .values()
            .filter(|surface| !surface.is_closed())
            .map(|surface| surface.id.clone())
            .collect::<Vec<_>>();
        ids.extend(
            self.call
                .active
                .then(|| format!("chat-call:{}", self.call.channel)),
        );
        ids
    }

    fn set_floating_surface_bounds(&mut self, bounds: Rect) {
        self.floating_surface_bounds = bounds;
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
        let desktop = self.desktop_floating_bounds();
        let returned = match message {
            ui::floating_surface::FloatingSurfaceHostMessage::Suspended { id }
            | ui::floating_surface::FloatingSurfaceHostMessage::Reattach { id } => {
                Some((id.clone(), false))
            }
            ui::floating_surface::FloatingSurfaceHostMessage::Closed { id } => {
                Some((id.clone(), true))
            }
            _ => None,
        };
        if self
            .agent_surfaces
            .values_mut()
            .any(|surface| surface.host_message(message, desktop))
        {
            if let Some((id, closed)) = returned {
                self.settle_returned_agent_surface(&id, closed);
            }
            self.canonical_dirty = true;
            return;
        }
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

    fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool) {
        if focused {
            self.focused_agent_surface = id.map(str::to_owned);
        } else if self.focused_agent_surface.as_deref() == id {
            self.focused_agent_surface = None;
        }
        for surface in self.agent_surfaces.values_mut() {
            surface
                .view
                .set_focus(focused && id == Some(surface.id.as_str()));
        }
    }

    fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>) {
        self.focused_agent_surface = id.map(str::to_owned);
        for surface in self.agent_surfaces.values_mut() {
            surface.view.set_focus(id == Some(surface.id.as_str()));
        }
    }

    fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
        self.input_agent_surface = id.map(str::to_owned);
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
        let previous = self.input_agent_surface.replace(id.to_owned());
        <Self as Widget>::handle_event(self, event, ctx);
        self.input_agent_surface = previous;
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
