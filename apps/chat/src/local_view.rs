//! Peer-local Chat presentation. This payload never contains a document snapshot.
use serde::{Deserialize, Serialize};
use ui::{
    application_view::{ApplicationFocusRestore, ApplicationLocalView},
    view::ThawError,
};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "persisted peer-local view schema; each flag is an independent panel toggle"
)]
struct LocalView {
    workspace: String,
    channel: String,
    thread: Option<String>,
    context_workspace: String,
    scroll: f64,
    focus_thread: bool,
    inbox: bool,
    left: bool,
    right: bool,
    emoji: bool,
    history: Option<String>,
    attachments: bool,
    call_expanded: bool,
    call_offset: [f64; 2],
    participants: bool,
}
impl crate::ChatApp {
    pub(crate) fn capture_local(&self) -> Result<Option<ApplicationLocalView>, ThawError> {
        let call_offset = match self.call.placement.attachment() {
            ui::floating_surface::Attachment::App { offset } => [offset.x, offset.y],
            ui::floating_surface::Attachment::Desktop { .. } => [0., 0.],
        };
        let view = LocalView {
            workspace: self.active_ws.clone(),
            channel: self.active_channel.clone(),
            thread: self.thread_open.clone(),
            context_workspace: self.context_ws.clone(),
            scroll: self.scroll,
            focus_thread: self.focus == crate::app::Focus::Thread,
            inbox: self.inbox_open,
            left: self.left_flap_open(),
            right: self.right_flap_open(),
            emoji: self.emoji_open,
            history: self.history_open.clone(),
            attachments: self.attach_open,
            call_expanded: self.call.expanded,
            call_offset,
            participants: self.call.participants_expanded,
        };
        Ok(Some(ApplicationLocalView {
            app_id: crate::definition().app_id.into(),
            version: 1,
            payload: serde_json::to_vec(&view).map_err(|e| ThawError::State(e.to_string()))?,
        }))
    }
    pub(crate) fn restore_local(
        &mut self,
        local: &ApplicationLocalView,
        doc: &automerge::AutoCommit,
    ) -> Result<ApplicationFocusRestore, ThawError> {
        if local.app_id != crate::definition().app_id {
            return Err(ThawError::BindingMismatch);
        }
        if local.version != 1 {
            return Err(ThawError::UnsupportedVersion {
                found: local.version,
                supported: 1,
            });
        }
        let view: LocalView =
            serde_json::from_slice(&local.payload).map_err(|e| ThawError::State(e.to_string()))?;
        if !view.scroll.is_finite()
            || view.scroll < 0.
            || view.call_offset.iter().any(|n| !n.is_finite())
        {
            return Err(ThawError::State("invalid Chat local geometry".into()));
        }
        self.active_ws = view.workspace;
        self.active_channel = view.channel;
        self.context_ws = view.context_workspace;
        self.thread_open = view.thread;
        self.reload(doc);
        if self.ensure_selection() {
            self.reload(doc);
        }
        if self
            .thread_open
            .as_ref()
            .is_some_and(|id| self.message(id).is_none())
        {
            self.thread_open = None;
        }
        self.scroll = view.scroll;
        self.focus = if view.focus_thread && self.thread_open.is_some() {
            crate::app::Focus::Thread
        } else {
            crate::app::Focus::Main
        };
        self.inbox_open = view.inbox;
        self.left_flap.snap(view.left);
        self.right_flap.snap(view.right);
        self.emoji_open = view.emoji;
        self.history_open = view.history.filter(|id| self.message(id).is_some());
        self.attach_open = view.attachments;
        self.call.expanded = view.call_expanded;
        self.call.participants_expanded = view.participants;
        self.call
            .placement
            .restore_local_offset(vello::kurbo::Vec2::new(
                view.call_offset[0],
                view.call_offset[1],
            ));
        self.set_composer_focus();
        Ok(ApplicationFocusRestore::Application)
    }
}
