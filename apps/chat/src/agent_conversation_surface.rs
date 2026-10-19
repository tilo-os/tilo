//! An exact task/run conversation whose retained Chat presentation can move to
//! the shared floating-surface host without duplicating its private editor.

use ui::Widget;
use ui::floating_surface::{
    Attachment, FloatingPlacement, FloatingRole, FloatingSource, FloatingSurfaceHostMessage,
    FloatingSurfacePresentation, FloatingSurfaceRequest,
};
use vello::kurbo::{Rect, Size, Vec2};

use crate::app::ChatApp;

pub(crate) struct AgentConversationSurface {
    pub(crate) id: String,
    pub(crate) conversation: crate::composer::ConversationKey,
    pub(crate) view: Box<ChatApp>,
    pub(crate) placement: FloatingPlacement,
    pub(crate) source: Rect,
    compact_size: Size,
    frame_shadow: ui::tokens::Shadow,
    frame_rim: f64,
    view_owns_editor: bool,
    closed: bool,
}

impl AgentConversationSurface {
    pub(crate) fn from_attached(parent: &mut ChatApp, agent: &str) -> Self {
        let mut view = ChatApp::conversation_instance(parent, agent);
        std::mem::swap(&mut view.agent_tasks, &mut parent.agent_tasks);
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
        placement.set_reduce_motion(parent.motion_reduced);
        let compact = f64::from(ui::tokens::Metrics::BASE.panel_width_sheet);
        Self {
            id: format!("chat-conversation:{}", uuid::Uuid::new_v4()),
            conversation: parent.direct_person.as_ref().map_or_else(
                || crate::composer::ConversationKey::Agent {
                    workspace: crate::app::ROOM_SCOPE.to_owned(),
                    route: parent
                        .agent_draft_route()
                        .unwrap_or_else(|| agent.to_owned()),
                },
                |public_key| crate::app::ChatApp::person_conversation_key(public_key),
            ),
            view: Box::new(view),
            placement,
            source,
            compact_size: Size::new(compact.min(source.width()), compact.min(source.height())),
            frame_shadow: ui::tokens::Elevation::default().floating,
            frame_rim: f64::from(ui::tokens::Metrics::default().chrome_rule_px),
            view_owns_editor: true,
            closed: false,
        }
    }

    pub(crate) fn move_editor_to_parent(&mut self, parent: &mut ChatApp) {
        if !self.view_owns_editor {
            return;
        }
        std::mem::swap(&mut parent.agent_tasks, &mut self.view.agent_tasks);
        std::mem::swap(&mut parent.composer_main, &mut self.view.composer_main);
        parent.focus = self.view.focus;
        parent.draft_main.clone_from(&self.view.draft_main);
        parent.scroll = self.view.scroll;
        self.view_owns_editor = false;
    }

    pub(crate) fn move_editor_to_view(&mut self, parent: &mut ChatApp) {
        if self.view_owns_editor {
            return;
        }
        std::mem::swap(&mut self.view.agent_tasks, &mut parent.agent_tasks);
        std::mem::swap(&mut self.view.composer_main, &mut parent.composer_main);
        parent
            .composer_main
            .share_private_store_from(&self.view.composer_main);
        self.view.focus = parent.focus;
        self.view.draft_main.clone_from(&parent.draft_main);
        self.view.scroll = parent.scroll;
        self.view_owns_editor = true;
    }

    pub(crate) fn close(&mut self) {
        self.view.close_person_conversation();
        self.closed = true;
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed
    }

    pub(crate) fn request(&self) -> FloatingSurfaceRequest {
        FloatingSurfaceRequest {
            id: self.id.clone(),
            title: if let crate::composer::ConversationKey::Person { public_key, .. } =
                &self.conversation
            {
                self.view.person_display_name(public_key)
            } else {
                self.view
                    .direct_agent
                    .as_deref()
                    .map_or_else(String::new, |agent| {
                        self.view
                            .direct_agent_tasks()
                            .and_then(crate::agent_tasks::AgentTaskView::current)
                            .map_or_else(
                                || self.view.agent_display_name(agent),
                                |task| {
                                    crate::i18n::t_args(
                                        "chat-agent-task-window",
                                        &[
                                            ("title", task.title.clone().into()),
                                            (
                                                "run",
                                                task.run_epoch
                                                    .unwrap_or_default()
                                                    .to_string()
                                                    .into(),
                                            ),
                                            ("name", self.view.agent_display_name(agent).into()),
                                        ],
                                    )
                                },
                            )
                    })
            },
            role: FloatingRole::Conversation,
            source: FloatingSource::AppElement {
                app_id: "so.tilo.chat".into(),
                element_id: self.id.clone(),
                region: self.source,
            },
            preferred_size: self.compact_size,
            aspect_ratio: None,
            initial_rect: Some(self.source),
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
        let content_rect = self.placement.rect(self.source, desktop);
        Some(FloatingSurfacePresentation {
            id: self.id.clone(),
            content_rect,
            window_rect: ui::floating_surface::app_owned_content_window_rect(
                content_rect,
                &self.frame_shadow,
                self.frame_rim,
            ),
            input_rect: Some(content_rect),
            returning_to_app: self.placement.is_return_pending(),
        })
    }

    pub(crate) fn observe_theme(&mut self, theme: &ui::theme::Theme) {
        self.frame_shadow = theme.tokens.elevation.floating;
        self.frame_rim = f64::from(theme.tokens.metrics.chrome_rule_px);
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
                self.placement.confirm_desktop_adoption();
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
