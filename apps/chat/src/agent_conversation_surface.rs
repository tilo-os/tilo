//! A direct Agent conversation whose retained Chat presentation can move to
//! a shell-owned secondary surface without duplicating its editor state.

use ui::Widget;
use ui::floating_surface::{
    Attachment, FloatingPlacement, FloatingRole, FloatingSource, FloatingSurfaceHostMessage,
    FloatingSurfacePresentation, FloatingSurfaceRequest,
};
use vello::kurbo::{Point, Rect, Size, Vec2};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::app::{ChatApp, Hit};

pub(crate) struct AgentConversationSurface {
    pub(crate) id: String,
    pub(crate) view: Box<ChatApp>,
    pub(crate) placement: FloatingPlacement,
    pub(crate) source: Rect,
    compact_size: Size,
    view_owns_editor: bool,
    closed: bool,
}

impl AgentConversationSurface {
    pub(crate) fn from_attached(parent: &mut ChatApp, agent: &str) -> Self {
        parent.painted_queue_offer = None;
        let mut view = ChatApp::conversation_instance(parent, agent);
        std::mem::swap(&mut view.composer_main, &mut parent.composer_main);
        parent
            .composer_main
            .share_private_store_from(&view.composer_main);
        view.focus = parent.focus;
        view.draft_main.clone_from(&parent.draft_main);
        view.scroll = parent.scroll;
        let source = Rect::new(
            parent.center_left(),
            parent.rect.y0,
            parent.center_right(),
            parent.rect.y1,
        );
        view.rect = source;
        let mut placement = FloatingPlacement::attached(source.size());
        placement.set_attached_region(Some(source));
        let compact_size = Size::new(
            parent
                .chrome
                .detached_conversation_width
                .min(source.width()),
            parent
                .chrome
                .detached_conversation_height
                .min(source.height()),
        );
        placement.settle_detached_to_preferred_size(compact_size);
        Self {
            id: format!("chat-conversation:{}", uuid::Uuid::new_v4()),
            view: Box::new(view),
            placement,
            source,
            compact_size,
            view_owns_editor: true,
            closed: false,
        }
    }

    pub(crate) fn move_editor_to_parent(&mut self, parent: &mut ChatApp) {
        if !self.view_owns_editor {
            return;
        }
        self.view.painted_queue_offer = None;
        parent.painted_queue_offer = None;
        // Returning this exact editor retires the parent's old Float-control
        // ordinal. Do this at the ownership transfer, not on every later
        // surface reconciliation: a repeated sync may legitimately find a
        // freshly focused Send or intent control in the attached view.
        parent.clear_keyboard_target();
        std::mem::swap(&mut parent.composer_main, &mut self.view.composer_main);
        parent.focus = self.view.focus;
        parent.draft_main.clone_from(&self.view.draft_main);
        parent.scroll = self.view.scroll;
        self.view_owns_editor = false;
        parent.set_composer_focus();
    }

    pub(crate) fn move_editor_to_view(&mut self, parent: &mut ChatApp) {
        if self.view_owns_editor {
            return;
        }
        self.view.painted_queue_offer = None;
        parent.painted_queue_offer = None;
        std::mem::swap(&mut self.view.composer_main, &mut parent.composer_main);
        parent
            .composer_main
            .share_private_store_from(&self.view.composer_main);
        self.view.focus = parent.focus;
        // A retained child can still point its immediate-mode keyboard ring
        // at the Return control that attached this exact editor. Re-floating
        // retires that control state; the moved editor becomes the child
        // surface's truthful keyboard and accessibility focus owner again.
        self.view.clear_keyboard_target();
        self.view.draft_main.clone_from(&parent.draft_main);
        self.view.scroll = parent.scroll;
        self.view_owns_editor = true;
    }

    pub(crate) fn close(&mut self) {
        self.closed = true;
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed
    }

    pub(crate) fn request(&self, desktop: Rect) -> FloatingSurfaceRequest {
        FloatingSurfaceRequest {
            id: self.id.clone(),
            title: self
                .view
                .direct_agent
                .as_deref()
                .map_or_else(String::new, |agent| self.view.agent_display_name(agent)),
            role: FloatingRole::Conversation,
            source: FloatingSource::AppElement {
                app_id: "so.tilo.chat".into(),
                element_id: self.id.clone(),
                region: self.source,
            },
            preferred_size: self.compact_size,
            aspect_ratio: None,
            initial_rect: Some(self.placement.rect(self.source, desktop)),
            presentation_rect: None,
            tracking_source: None,
        }
    }

    pub(crate) fn presentation(&self, desktop: Rect) -> Option<FloatingSurfacePresentation> {
        if !matches!(self.placement.attachment(), Attachment::Desktop { .. })
            && !self.placement.is_return_pending()
        {
            return None;
        }
        let rect = self.placement.rect(self.source, desktop);
        Some(FloatingSurfacePresentation {
            id: self.id.clone(),
            content_rect: rect,
            window_rect: rect,
            returning_to_app: self.placement.is_return_pending(),
        })
    }

    /// Route the detached conversation's own grip to its retained outer
    /// placement. Forwarding this press into `view` would begin a second
    /// app-to-desktop handoff and manufacture a nested conversation surface.
    pub(crate) fn route_drag_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        app: Rect,
        desktop: Rect,
    ) -> bool {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = Point::new(position.x / scale, position.y / scale);
                self.view.last_mouse = point;
                if !self.placement.is_dragging() {
                    return false;
                }
                let _ = self.placement.update_drag(point, app, desktop);
                self.view.rect = self.placement.rect(self.source, desktop);
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.view.hits.iter().rev().any(|(rect, hit)| {
                rect.contains(self.view.last_mouse) && matches!(hit, Hit::AgentDragHandle)
            }) =>
            {
                self.placement
                    .begin_drag(self.view.last_mouse, app, desktop);
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.placement.is_dragging() => {
                self.placement.end_drag(desktop);
                self.view.rect = self.placement.rect(self.source, desktop);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn host_message(
        &mut self,
        message: &FloatingSurfaceHostMessage,
        desktop: Rect,
    ) -> bool {
        match message {
            FloatingSurfaceHostMessage::PrepareSurface {
                id,
                content_rect,
                coordinate_offset,
                transfer_covered,
                ..
            } if id == &self.id => {
                let Ok(rect) = ui::floating_surface::protocol_rect(*content_rect) else {
                    return false;
                };
                self.placement.prepare_desktop_rect_with_offset(
                    rect,
                    Vec2::new(coordinate_offset[0], coordinate_offset[1]),
                );
                self.placement
                    .set_desktop_transfer_covered(*transfer_covered);
            }
            FloatingSurfaceHostMessage::Adopted { id, rect } if id == &self.id => {
                if ui::floating_surface::protocol_rect(*rect).is_err() {
                    return false;
                }
                self.placement.confirm_desktop_adoption_within(desktop);
            }
            FloatingSurfaceHostMessage::Reattach { id }
            | FloatingSurfaceHostMessage::Closed { id }
                if id == &self.id =>
            {
                self.placement.reattach_to_app(self.source, desktop);
            }
            FloatingSurfaceHostMessage::Suspended { id } if id == &self.id => {
                self.placement.confirm_app_adoption(self.source);
                self.placement.reset_attached();
            }
            _ => return false,
        }
        self.view.rect = self.placement.rect(self.source, desktop);
        true
    }

    pub(crate) fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        fn qualify(node: &mut ui::a11y::AccessNode, id: &str) {
            if let Some(target) = node.action_target.as_mut() {
                *target = format!("agent-surface:{id}|{target}");
            }
            for child in &mut node.children {
                qualify(child, id);
            }
        }
        let mut node = self.view.accessibility()?;
        node = node.with_hashed_key(&self.id);
        qualify(&mut node, &self.id);
        Some(node)
    }
}
