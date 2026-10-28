//! The floating surfaces Chat hosts: an agent's conversation opened out of
//! its thread into a window of its own, with what the host says back about
//! it (adopted, suspended, closed, reattached).

use automerge::ObjId;
use ui::Widget;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::FloatingSurfaceProvider;
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::ChatApp;

impl FloatingSurfaceProvider for ChatApp {
    fn floating_surface_accessibility(&self, id: &str) -> Option<ui::a11y::AccessNode> {
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
            surface.view.room_agents.clone_from(&self.room_agents);
            surface.view.room_names.clone_from(&self.room_names);
            surface.view.room_persons.clone_from(&self.room_persons);
            surface.view.room_labels.clone_from(&self.room_labels);
            surface.view.own_label.clone_from(&self.own_label);
            surface.view.room_colors.clone_from(&self.room_colors);
            surface.view.messages.clone_from(&self.messages);
            surface.view.channels.clone_from(&self.channels);
            surface.view.refresh_person_conversation();
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
            surface.view.room_agents.clone_from(&self.room_agents);
            surface.view.room_names.clone_from(&self.room_names);
            surface.view.room_persons.clone_from(&self.room_persons);
            surface.view.room_labels.clone_from(&self.room_labels);
            surface.view.own_label.clone_from(&self.own_label);
            surface.view.room_colors.clone_from(&self.room_colors);
            surface.view.messages.clone_from(&self.messages);
            surface.view.channels.clone_from(&self.channels);
            surface.view.refresh_person_conversation();
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
        let call_id = format!("chat-call:{}", self.call.channel);
        if id != call_id || !self.call.active {
            return false;
        }
        let mut hits = Vec::new();
        self.draw_live_huddle(scene, transform, text_renderer, theme, presence, &mut hits);
        self.hits.extend(hits);
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
        presentations.push(ui::floating_surface::FloatingSurfacePresentation {
            id: format!("chat-call:{}", self.call.channel),
            content_rect: self.call_video_rect(),
            window_rect: self.huddle_panel().inflate(8.0, 8.0),
            input_rect: Some(self.huddle_panel()),
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
}
