//! An agent's conversation floated out of its thread into a window of its own:
//! the route it keeps while dragged, and how it returns to the conversation it
//! came from.

use vello::kurbo::Point;

use crate::model;

use super::{AgentConversationDrag, ChatApp, Focus};

impl ChatApp {
    pub(crate) fn conversation_instance(parent: &Self) -> Self {
        let mut view = Self::bare(parent.content.clone());
        view.channels.clone_from(&parent.channels);
        view.categories.clone_from(&parent.categories);
        view.users.clone_from(&parent.users);
        view.messages.clone_from(&parent.messages);
        view.search_messages.clone_from(&parent.search_messages);
        view.search_users.clone_from(&parent.search_users);
        view.voice_members.clone_from(&parent.voice_members);
        view.me = parent.me.clone();
        view.perms = parent.perms.clone();
        view.room_names.clone_from(&parent.room_names);
        view.room_labels.clone_from(&parent.room_labels);
        view.own_label.clone_from(&parent.own_label);
        view.room_colors.clone_from(&parent.room_colors);
        view.room_agents.clone_from(&parent.room_agents);
        view.capture_agent_presence
            .clone_from(&parent.capture_agent_presence);
        view.agent_authors.clone_from(&parent.agent_authors);
        view.former_ids.clone_from(&parent.former_ids);
        view.person_subscription = parent.person_subscription;
        view.direct_person = parent
            .direct_person
            .as_ref()
            .filter(|peer| {
                parent
                    .person_subscription
                    .is_some_and(|token| ui::person_chat::retain_current(peer, token))
            })
            .cloned();
        view.person_contacts.clone_from(&parent.person_contacts);
        view.person_messages.clone_from(&parent.person_messages);
        view.person_error.clone_from(&parent.person_error);
        view.active_channel.clone_from(&parent.active_channel);
        view.conversation_surface = true;
        view.thread_open = None;
        view.right_flap.snap(false);
        view.external_services = parent.external_services;
        if let Some(dispatcher) = parent.dispatch.clone() {
            view.attach_framework(dispatcher);
        }
        if let Some(chain) = parent.chain.clone() {
            view.attach_chain(chain);
        }
        view
    }

    pub(crate) fn float_agent_conversation(&mut self) -> bool {
        let Some(peer) = self.direct_person.clone() else {
            return false;
        };
        if !self.main_composer_can_edit() || self.rect.area() <= 0.0 {
            return false;
        }
        let channel = self.active_channel.clone();
        if let Some(mut surface) = self.agent_surfaces.remove(&channel) {
            if matches!(
                surface.placement.attachment(),
                ui::floating_surface::Attachment::App { .. }
            ) {
                surface.move_editor_to_view(self);
                surface.placement.detach_to_desktop(surface.source);
                self.agent_surface_requests.push_back(surface.request());
            }
            self.agent_surfaces.insert(channel, surface);
            self.leave_floated_agent_route();
            return true;
        }
        let mut surface =
            crate::agent_conversation_surface::AgentConversationSurface::from_attached(self, &peer);
        surface.placement.detach_to_desktop(surface.source);
        self.agent_surface_requests.push_back(surface.request());
        self.agent_surfaces.insert(channel, surface);
        self.leave_floated_agent_route();
        true
    }

    pub(crate) fn leave_floated_agent_route(&mut self) {
        let fallback = self
            .channels
            .iter()
            .filter(|channel| {
                !channel.archived
                    && channel.kind == model::ChannelKind::Text
                    && channel.conversation_with.is_none()
            })
            .min_by_key(|channel| channel.order)
            .map(|channel| channel.id.clone());
        self.close_person_conversation();
        self.thread_open = None;
        if let Some(channel) = fallback {
            self.active_channel = channel;
            self.composer_main.set_key(&self.active_channel);
        } else {
            self.active_channel.clear();
        }
        self.focus = Focus::Main;
        self.scroll = 0.0;
        self.request_relayout();
    }

    pub(crate) fn begin_agent_conversation_drag(&mut self, origin: Point) {
        let Some(peer) = self.direct_person.clone() else {
            return;
        };
        self.agent_conversation_drag = Some(AgentConversationDrag {
            peer,
            channel: self.active_channel.clone(),
            origin,
        });
    }

    pub(crate) fn update_agent_conversation_drag(&mut self, pointer: Point) {
        let Some(drag) = self.agent_conversation_drag.clone() else {
            return;
        };
        let desktop = self.desktop_floating_bounds();
        if let Some(surface) = self.agent_surfaces.get_mut(&drag.channel) {
            let handoff = surface.placement.update_drag(pointer, self.rect, desktop);
            if handoff == ui::floating_surface::Handoff::Detached {
                self.agent_surface_requests.push_back(surface.request());
            }
            self.request_relayout();
            return;
        }
        if !ui::floating_surface::FloatingPlacement::is_beyond_detach_boundary(pointer, self.rect) {
            return;
        }
        let mut surface =
            crate::agent_conversation_surface::AgentConversationSurface::from_attached(
                self, &drag.peer,
            );
        surface
            .placement
            .begin_drag(drag.origin, self.rect, desktop);
        if surface.placement.update_drag(pointer, self.rect, desktop)
            != ui::floating_surface::Handoff::Detached
        {
            surface.move_editor_to_parent(self);
            return;
        }
        self.agent_surface_requests.push_back(surface.request());
        self.agent_surfaces.insert(drag.channel, surface);
        self.leave_floated_agent_route();
        self.request_relayout();
    }

    pub(crate) fn end_agent_conversation_drag(&mut self) {
        let Some(drag) = self.agent_conversation_drag.take() else {
            return;
        };
        let desktop = self.desktop_floating_bounds();
        if let Some(surface) = self.agent_surfaces.get_mut(&drag.channel) {
            surface.placement.end_drag(desktop);
        }
    }

    pub(crate) fn restore_attached_agent_surface(&mut self, channel: &str) {
        let Some(mut surface) = self.agent_surfaces.remove(channel) else {
            return;
        };
        let attached = matches!(
            surface.placement.attachment(),
            ui::floating_surface::Attachment::App { .. }
        );
        let route_is_active = self.active_channel == channel;
        if attached && route_is_active {
            surface.move_editor_to_parent(self);
            self.set_composer_focus();
        }
        // A returned person view is now owned by the parent. Do not retain an
        // invisible attached copy (and its subscription) after navigating away.
        let person_returned = attached && route_is_active;
        if (!surface.is_closed() || !route_is_active) && !person_returned {
            self.agent_surfaces.insert(channel.to_owned(), surface);
        }
    }

    fn restore_retained_agent_route(&mut self, channel: &str) {
        if !self.agent_surfaces.contains_key(channel) {
            return;
        }
        self.close_person_conversation();
        let Some(surface) = self.agent_surfaces.get(channel) else {
            return;
        };
        channel.clone_into(&mut self.active_channel);
        self.person_subscription = surface.view.person_subscription;
        self.direct_person = surface
            .view
            .direct_person
            .as_ref()
            .filter(|peer| {
                surface
                    .view
                    .person_subscription
                    .is_some_and(|token| ui::person_chat::retain_current(peer, token))
            })
            .cloned();
        self.person_contacts
            .clone_from(&surface.view.person_contacts);
        self.person_messages
            .clone_from(&surface.view.person_messages);
        self.thread_open = None;
        self.focus = Focus::Main;
        self.scroll = 0.0;
    }

    pub(super) fn settle_returned_agent_surface(&mut self, id: &str, closed: bool) {
        let Some(channel) = self
            .agent_surfaces
            .iter()
            .find_map(|(channel, surface)| (surface.id == id).then(|| channel.clone()))
        else {
            return;
        };
        if !closed {
            self.restore_retained_agent_route(&channel);
        }
        if closed && let Some(surface) = self.agent_surfaces.get_mut(&channel) {
            surface.close();
        }
        if self.focused_agent_surface.as_deref() == Some(id) {
            self.focused_agent_surface = None;
        }
        if self.input_agent_surface.as_deref() == Some(id) {
            self.input_agent_surface = None;
        }
        self.restore_attached_agent_surface(&channel);
    }

    pub(crate) fn settle_agent_surface_commands(&mut self) {
        let requested = self
            .agent_surfaces
            .iter_mut()
            .find_map(|(channel, surface)| {
                std::mem::take(&mut surface.view.conversation_return_requested)
                    .then(|| (channel.clone(), surface.id.clone()))
            });
        let Some((channel, id)) = requested else {
            return;
        };
        if self
            .agent_surfaces
            .get(&channel)
            .is_some_and(|surface| surface.id == id)
            && let Some(surface) = self.agent_surfaces.get_mut(&channel)
        {
            surface.placement.request_app_return();
        }
    }
}
