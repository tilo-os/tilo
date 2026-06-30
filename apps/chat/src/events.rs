//! Input routing for [`ChatApp`]. Mouse presses hit-test the regions
//! recorded during the previous `draw`; keyboard input drives the focused
//! composer, splicing directly into the per-channel draft `Text` object in
//! the CRDT (so a draft is collaborative state, and undo/redo come for
//! free from the toolkit's snapshotting).

use automerge::transaction::Transactable;
use automerge::AutoCommit;
use vello::kurbo::{Point, Vec2};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::app::{
    ChatApp, ContextMenu, Focus, Hit, MenuAction, MenuTarget, RowSwipe, SWIPE_COMMIT_DX,
};
use crate::call::{CallAction, OverlayDrag};
use crate::doc::{ChatDoc, NewMessage, NOW};
use crate::model::{self, ChannelKind};

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(NOW)
}

impl ChatApp {
    pub(crate) fn dispatch_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        handled: &mut bool,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / scale, position.y / scale);
                // Live-drag the floating call overlay.
                if let Some(drag) = self.overlay_drag {
                    let delta = self.last_mouse - drag.origin;
                    self.call.overlay_offset = drag.start_offset + Vec2::new(delta.x, delta.y);
                    self.request_relayout();
                    *handled = true;
                }
                // Track an in-progress channel-row swipe.
                if let Some(sw) = &mut self.swipe {
                    sw.current = self.last_mouse;
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64 * 44.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale,
                };
                self.scroll += dy;
                *handled = true;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                match self.hit_at(self.last_mouse) {
                    Some(Hit::CallDragHandle) => {
                        // Begin dragging the overlay (only meaningful docked).
                        self.overlay_drag = Some(OverlayDrag {
                            origin: self.last_mouse,
                            start_offset: self.call.overlay_offset,
                        });
                        *handled = true;
                    }
                    Some(Hit::Channel(id)) => {
                        // Arm a potential swipe on the row; a plain click still
                        // resolves on release if it didn't travel.
                        if let Some(idx) = self.ws_channels().iter().position(|c| c.id == id) {
                            self.swipe = Some(RowSwipe {
                                channel_index: idx,
                                origin: self.last_mouse,
                                current: self.last_mouse,
                            });
                        }
                        // Defer the open to release so a swipe isn't a click.
                        *handled = true;
                    }
                    Some(hit) => {
                        self.on_hit(hit, doc);
                        *handled = true;
                    }
                    None => {}
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.overlay_drag = None;
                if let Some(sw) = self.swipe.take() {
                    let channel = self
                        .ws_channels()
                        .get(sw.channel_index)
                        .map(|c| c.id.clone());
                    if let Some(id) = channel {
                        if sw.dx() >= SWIPE_COMMIT_DX {
                            // Swipe-left commits the channel's primary action,
                            // which is exactly the context menu's first item.
                            self.apply_channel_action(&id, MenuAction::channel_swipe_left(), doc);
                        } else if sw.dx() < 6.0 {
                            // Didn't travel — treat as a click on the channel.
                            self.on_hit(Hit::Channel(id), doc);
                        }
                    }
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                if let Some(hit) = self.hit_at(self.last_mouse) {
                    self.on_right_click(hit);
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
                ..
            } => {
                if self.on_key(logical_key, modifiers, doc) {
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    /// Right-click opens a context menu for the channel/category/workspace
    /// under the cursor.
    fn on_right_click(&mut self, hit: Hit) {
        let menu = match hit {
            Hit::Channel(id) => Some(ContextMenu {
                target: MenuTarget::Channel(id),
                anchor: self.last_mouse,
                items: MenuAction::for_channel().to_vec(),
            }),
            Hit::VoiceChannel(id) => Some(ContextMenu {
                target: MenuTarget::Channel(id),
                anchor: self.last_mouse,
                items: MenuAction::for_channel().to_vec(),
            }),
            Hit::CategoryToggle(id) | Hit::AddChannel { category: id, .. } => Some(ContextMenu {
                target: MenuTarget::Category(id),
                anchor: self.last_mouse,
                items: MenuAction::for_category().to_vec(),
            }),
            _ => Some(ContextMenu {
                target: MenuTarget::Workspace(self.active_ws.clone()),
                anchor: self.last_mouse,
                items: vec![MenuAction::NewCategory],
            }),
        };
        self.menu = menu;
    }

    /// Top-most matching region (regions are recorded back-to-front, so a
    /// reverse scan resolves the visually-topmost first).
    fn hit_at(&self, p: Point) -> Option<Hit> {
        self.hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(p))
            .map(|(_, h)| h.clone())
    }

    fn on_hit(&mut self, hit: Hit, doc: &mut AutoCommit) {
        // Any click dismisses an open context menu unless it's a menu action.
        if !matches!(hit, Hit::MenuAction { .. }) {
            self.menu = None;
        }
        match hit {
            Hit::Workspace(id) => {
                if self.active_ws != id {
                    self.active_ws = id;
                    self.thread_open = None;
                    self.scroll = 0.0;
                    self.focus = Focus::Main;
                    self.caret = 0;
                    self.active_channel.clear(); // force reload to pick first text channel
                }
            }
            Hit::Channel(id) => {
                let kind = self.channel(&id).map(|c| c.kind);
                if kind == Some(ChannelKind::Voice) {
                    self.join_voice(&id, doc);
                } else {
                    self.active_channel = id;
                    self.thread_open = None;
                    self.scroll = 0.0;
                    self.focus = Focus::Main;
                    self.caret = 0;
                    // Picking a channel in overlay mode closes the left flap.
                    if self.left_mode == crate::responsive::FlapMode::Overlay {
                        self.left_flap.close();
                    }
                }
            }
            Hit::VoiceChannel(id) => self.join_voice(&id, doc),
            Hit::CategoryToggle(id) => {
                ChatDoc::toggle_category_collapsed(doc, &id);
            }
            Hit::ContextMenu { target, anchor } => {
                let items = match &target {
                    MenuTarget::Channel(_) => MenuAction::for_channel().to_vec(),
                    MenuTarget::Category(_) => MenuAction::for_category().to_vec(),
                    MenuTarget::Workspace(_) => vec![MenuAction::NewCategory],
                };
                self.menu = Some(ContextMenu { target, anchor, items });
            }
            Hit::MenuAction { target, action } => {
                self.menu = None;
                self.apply_menu_action(target, action, doc);
            }
            Hit::AddChannel { workspace, category } => {
                let id = ChatDoc::create_channel(doc, &workspace, &category, "new-channel", ChannelKind::Text);
                self.begin_rename(MenuTarget::Channel(id));
            }
            Hit::AddCategory(ws) => {
                let id = ChatDoc::create_category(doc, &ws, "New Category");
                self.begin_rename(MenuTarget::Category(id));
            }
            Hit::RenameConfirm => self.commit_rename(doc),
            Hit::Reaction { msg, emoji } => {
                ChatDoc::toggle_reaction(doc, &self.active_channel, &msg, &emoji, model::ME);
            }
            Hit::AddReaction { msg, anchor } => {
                self.emoji_open = true;
                self.emoji_target = Some(msg);
                self.emoji_anchor = anchor;
            }
            Hit::OpenThread(id) => {
                if self.thread_open.as_deref() == Some(id.as_str()) {
                    self.thread_open = None;
                    self.focus = Focus::Main;
                } else {
                    self.thread_open = Some(id);
                    self.focus = Focus::Thread;
                    self.right_flap.open();
                }
                self.caret = 0;
            }
            Hit::EditHistory { msg, anchor } => {
                self.history_open = Some(msg);
                self.history_anchor = anchor;
            }
            Hit::EmojiChoice(e) => {
                if let Some(target) = self.emoji_target.take() {
                    ChatDoc::toggle_reaction(doc, &self.active_channel, &target, &e, model::ME);
                } else {
                    self.insert_text(doc, &e);
                }
                self.emoji_open = false;
            }
            Hit::FocusMain => {
                self.focus = Focus::Main;
                self.caret = self.draft_main.chars().count();
            }
            Hit::FocusThread => {
                self.focus = Focus::Thread;
                self.caret = self.draft_thread.chars().count();
            }
            Hit::Send => self.send_focused(doc),
            Hit::ComposerEmoji(anchor) => {
                self.emoji_open = true;
                self.emoji_target = None;
                self.emoji_anchor = anchor;
            }
            Hit::Attach => self.attach_demo(doc),
            Hit::StartCall => {
                if self.call.active {
                    self.call.expanded = !self.call.expanded;
                    self.request_relayout();
                } else if let Some(v) = self
                    .ws_channels()
                    .into_iter()
                    .find(|c| c.kind == ChannelKind::Voice)
                    .map(|c| c.id.clone())
                {
                    self.join_voice(&v, doc);
                }
            }
            Hit::CloseThread => {
                self.thread_open = None;
                self.focus = Focus::Main;
            }
            Hit::CallDragHandle => {} // handled at press time
            Hit::CallParticipantsToggle => {
                self.call.participants_expanded = !self.call.participants_expanded;
            }
            Hit::CallMute => {
                if self.call_allows(CallAction::Speak) {
                    self.call.toggle_mute();
                    self.sync_local_voice(doc);
                }
            }
            Hit::CallDeafen => {
                self.call.toggle_deafen();
                self.sync_local_voice(doc);
            }
            Hit::CallAudioOnly => {
                if self.call_allows(CallAction::Camera) {
                    self.call.audio_only = !self.call.audio_only;
                    self.sync_local_voice(doc);
                }
            }
            Hit::CallFollow => self.call.follow_host = !self.call.follow_host,
            Hit::CallExpand => {
                self.call.expanded = !self.call.expanded;
                self.request_relayout();
            }
            Hit::CallLeave => self.leave_voice(doc),
            Hit::ToggleLeftFlap => self.left_flap.toggle(self.left_mode),
            Hit::ToggleRightFlap => self.right_flap.toggle(self.right_mode),
            Hit::Dismiss => {
                if self.menu.is_some() {
                    self.menu = None;
                } else if self.emoji_open {
                    self.emoji_open = false;
                    self.emoji_target = None;
                } else if self.history_open.is_some() {
                    self.history_open = None;
                } else if self.left_mode == crate::responsive::FlapMode::Overlay && self.left_flap_open() {
                    self.left_flap.close();
                } else if self.right_mode == crate::responsive::FlapMode::Overlay && self.right_flap_open() {
                    self.right_flap.close();
                }
            }
        }
        self.reload(doc);
        self.persist(doc);
    }

    // --- voice / call wiring ---------------------------------------------

    /// Join a voice channel: connect the local member, set call state, play
    /// the join cue. Gated by the call permission seam.
    fn join_voice(&mut self, channel: &str, doc: &mut AutoCommit) {
        if !self.call_allows(CallAction::Join) {
            return;
        }
        // Leaving a prior call first emits a leave cue for that channel.
        if self.call.active && self.call.channel != channel {
            ChatDoc::voice_leave(doc, &self.call.channel.clone(), model::ME);
        }
        self.call.join(channel.to_string());
        ChatDoc::voice_join(doc, channel, model::ME);
        self.sync_local_voice(doc);
        // Media: publish the local audio track over the (simulated or live)
        // transport so the pipeline is exercised end to end.
        self.media.transport_mut().publish(crate::media::TrackKind::Audio);
        self.sound.join();
        self.request_relayout();
    }

    /// Leave the current call: drop the local member, play the leave cue.
    fn leave_voice(&mut self, doc: &mut AutoCommit) {
        if !self.call.active {
            return;
        }
        let ch = self.call.channel.clone();
        ChatDoc::voice_leave(doc, &ch, model::ME);
        self.media.transport_mut().unpublish(crate::media::TrackKind::Audio);
        self.call.leave();
        self.sound.leave();
        self.request_relayout();
    }

    /// Mirror this peer's local mic/deafen/audio-only state into the shared
    /// voice membership so others see it. Talking is simulated from the
    /// not-muted state for the local member.
    fn sync_local_voice(&mut self, doc: &mut AutoCommit) {
        if !self.call.active {
            return;
        }
        let ch = self.call.channel.clone();
        ChatDoc::voice_set_state(
            doc,
            &ch,
            model::ME,
            !self.call.muted,
            self.call.muted,
            self.call.deafened,
            self.call.audio_only,
        );
    }

    // --- structure CRUD ---------------------------------------------------

    fn apply_menu_action(&mut self, target: MenuTarget, action: MenuAction, doc: &mut AutoCommit) {
        match target {
            MenuTarget::Channel(id) => self.apply_channel_action(&id, action, doc),
            MenuTarget::Category(id) => self.apply_category_action(&id, action, doc),
            MenuTarget::Workspace(ws) => {
                if action == MenuAction::NewCategory {
                    let id = ChatDoc::create_category(doc, &ws, "New Category");
                    self.begin_rename(MenuTarget::Category(id));
                }
            }
        }
        self.reload(doc);
        self.persist(doc);
    }

    fn apply_channel_action(&mut self, id: &str, action: MenuAction, doc: &mut AutoCommit) {
        match action {
            MenuAction::Rename => self.begin_rename(MenuTarget::Channel(id.to_string())),
            MenuAction::Delete => {
                ChatDoc::delete_channel(doc, id);
                if self.active_channel == id {
                    self.active_channel.clear(); // reload picks a new one
                }
            }
            MenuAction::MoveUp => ChatDoc::reorder_channel(doc, id, true),
            MenuAction::MoveDown => ChatDoc::reorder_channel(doc, id, false),
            _ => {}
        }
        self.reload(doc);
        self.persist(doc);
    }

    fn apply_category_action(&mut self, id: &str, action: MenuAction, doc: &mut AutoCommit) {
        let ws = self.active_ws.clone();
        match action {
            MenuAction::Rename => self.begin_rename(MenuTarget::Category(id.to_string())),
            MenuAction::Delete => ChatDoc::delete_category(doc, id),
            MenuAction::ToggleCollapse => ChatDoc::toggle_category_collapsed(doc, id),
            MenuAction::NewChannelHere => {
                let new_id = ChatDoc::create_channel(doc, &ws, id, "new-channel", ChannelKind::Text);
                self.begin_rename(MenuTarget::Channel(new_id));
            }
            MenuAction::NewVoiceChannelHere => {
                let new_id = ChatDoc::create_channel(doc, &ws, id, "New Voice", ChannelKind::Voice);
                self.begin_rename(MenuTarget::Channel(new_id));
            }
            _ => {}
        }
        self.reload(doc);
        self.persist(doc);
    }

    /// Open the inline rename editor for a target, seeded with its current
    /// name.
    fn begin_rename(&mut self, target: MenuTarget) {
        let current = match &target {
            MenuTarget::Channel(id) => self.channel(id).map(|c| c.name.clone()),
            MenuTarget::Category(id) => self.category(id).map(|c| c.name.clone()),
            MenuTarget::Workspace(_) => None,
        }
        .unwrap_or_default();
        self.caret = current.chars().count();
        self.rename = Some((target, current));
    }

    /// Commit the inline rename to the CRDT and close the editor.
    fn commit_rename(&mut self, doc: &mut AutoCommit) {
        if let Some((target, text)) = self.rename.take() {
            let text = text.trim();
            if !text.is_empty() {
                match target {
                    MenuTarget::Channel(id) => ChatDoc::rename_channel(doc, &id, text),
                    MenuTarget::Category(id) => ChatDoc::rename_category(doc, &id, text),
                    MenuTarget::Workspace(_) => {}
                }
            }
            self.caret = 0;
            self.reload(doc);
            self.persist(doc);
        }
    }

    fn on_key(&mut self, key: &Key, modifiers: &Modifiers, doc: &mut AutoCommit) -> bool {
        // The inline rename editor captures all keys while open.
        if self.rename.is_some() {
            return self.rename_key(key, doc);
        }

        // Escape unwinds the top-most layer.
        if let Key::Named(NamedKey::Escape) = key {
            if self.menu.is_some() {
                self.menu = None;
            } else if self.emoji_open {
                self.emoji_open = false;
                self.emoji_target = None;
            } else if self.history_open.is_some() {
                self.history_open = None;
            } else if self.call.expanded {
                self.call.expanded = false;
                self.request_relayout();
            } else if self.thread_open.is_some() {
                self.thread_open = None;
                self.focus = Focus::Main;
            } else {
                return false;
            }
            return true;
        }
        // While a popover is up, swallow other keys.
        if self.emoji_open || self.history_open.is_some() || self.menu.is_some() {
            return true;
        }

        let shift = modifiers.state().shift_key();
        match key {
            Key::Named(NamedKey::Enter) => {
                if shift {
                    self.insert_text(doc, "\n");
                } else {
                    self.send_focused(doc);
                }
                true
            }
            Key::Named(NamedKey::Backspace) => {
                self.backspace(doc);
                true
            }
            Key::Named(NamedKey::Space) => {
                self.insert_text(doc, " ");
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.caret = self.caret.saturating_sub(1);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.caret = (self.caret + 1).min(self.focused_draft().chars().count());
                true
            }
            Key::Named(NamedKey::Home) => {
                self.caret = 0;
                true
            }
            Key::Named(NamedKey::End) => {
                self.caret = self.focused_draft().chars().count();
                true
            }
            Key::Character(s) => {
                if s.chars().all(|c| !c.is_control()) {
                    self.insert_text(doc, s);
                }
                true
            }
            _ => false,
        }
    }

    /// Keystroke handling while the inline rename editor is open. Enter
    /// commits, Escape cancels, the rest edits the working name (appended at
    /// the end — a single-field name editor doesn't need a caret model).
    fn rename_key(&mut self, key: &Key, doc: &mut AutoCommit) -> bool {
        match key {
            Key::Named(NamedKey::Enter) => {
                self.commit_rename(doc);
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.rename = None;
                self.caret = 0;
                true
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some((_, text)) = &mut self.rename {
                    text.pop();
                    self.caret = text.chars().count();
                }
                true
            }
            Key::Named(NamedKey::Space) => {
                if let Some((_, text)) = &mut self.rename {
                    text.push(' ');
                    self.caret = text.chars().count();
                }
                true
            }
            Key::Character(s) => {
                if let Some((_, text)) = &mut self.rename {
                    if s.chars().all(|c| !c.is_control()) {
                        text.push_str(s);
                        self.caret = text.chars().count();
                    }
                }
                true
            }
            _ => true, // swallow everything else while renaming
        }
    }

    fn draft_key(&self) -> String {
        match self.focus {
            Focus::Thread if self.thread_open.is_some() => self.thread_draft_key().unwrap(),
            _ => self.active_channel.clone(),
        }
    }

    fn insert_text(&mut self, doc: &mut AutoCommit, s: &str) {
        let key = self.draft_key();
        let obj = ChatDoc::draft_obj(doc, &key);
        let _ = doc.splice_text(&obj, self.caret, 0, s);
        self.caret += s.chars().count();
        self.reload(doc);
    }

    fn backspace(&mut self, doc: &mut AutoCommit) {
        if self.caret == 0 {
            return;
        }
        let key = self.draft_key();
        let obj = ChatDoc::draft_obj(doc, &key);
        let _ = doc.splice_text(&obj, self.caret - 1, 1, "");
        self.caret -= 1;
        self.reload(doc);
    }

    fn send_focused(&mut self, doc: &mut AutoCommit) {
        let (key, parent) = match self.focus {
            Focus::Thread if self.thread_open.is_some() => {
                (self.thread_draft_key().unwrap(), self.thread_open.clone())
            }
            _ => (self.active_channel.clone(), None),
        };
        let text = ChatDoc::draft_text(doc, &key);
        if text.trim().is_empty() {
            return;
        }
        ChatDoc::push_message(
            doc,
            &self.active_channel,
            NewMessage { author: model::ME.into(), ts: now_secs(), text, parent, ..Default::default() },
        );
        ChatDoc::clear_draft(doc, &key);
        self.caret = 0;
        if matches!(self.focus, Focus::Main) {
            self.scroll = 0.0;
        }
        self.reload(doc);
        self.persist(doc);
    }

    /// Simulate sharing a file from the FUSE workspace as an attachment.
    fn attach_demo(&mut self, doc: &mut AutoCommit) {
        use crate::model::Attachment;
        ChatDoc::push_message(
            doc,
            &self.active_channel,
            NewMessage {
                author: model::ME.into(),
                ts: now_secs(),
                text: "Sharing this from the workspace 👇".into(),
                attachment: Some(Attachment::Image {
                    name: "shared-from-fuse.png".into(),
                    path: "/var/workspace/screenshots/shared-from-fuse.png".into(),
                    w: 1200,
                    h: 760,
                    tint: 4,
                }),
                ..Default::default()
            },
        );
        self.scroll = 0.0;
        self.reload(doc);
        self.persist(doc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::ChatDoc;

    fn app_with_doc() -> (ChatApp, AutoCommit) {
        let mut doc = AutoCommit::new();
        ChatDoc::seed_if_empty(&mut doc);
        let mut app = ChatApp::new_ephemeral();
        app.reload(&doc);
        (app, doc)
    }

    #[test]
    fn typing_then_send_appends_a_message() {
        let (mut app, mut doc) = app_with_doc();
        let before = ChatDoc::list_messages(&doc, "c_crit").len();
        app.focus = Focus::Main;
        app.insert_text(&mut doc, "hello team");
        assert_eq!(app.draft_main, "hello team");
        app.send_focused(&mut doc);
        let after = ChatDoc::list_messages(&doc, "c_crit");
        assert_eq!(after.len(), before + 1);
        assert_eq!(after.last().unwrap().text, "hello team");
        assert_eq!(app.draft_main, "", "draft cleared after send");
    }

    #[test]
    fn reply_in_thread_sets_parent() {
        let (mut app, mut doc) = app_with_doc();
        app.thread_open = Some("m_concept".into());
        app.focus = Focus::Thread;
        app.insert_text(&mut doc, "nice");
        app.send_focused(&mut doc);
        let replies = model::reply_count(&ChatDoc::list_messages(&doc, "c_crit"), "m_concept");
        assert_eq!(replies, 4, "thread gained one reply on top of the seeded three");
    }

    #[test]
    fn switching_to_voice_channel_starts_a_call() {
        let (mut app, mut doc) = app_with_doc();
        app.call.active = false;
        app.on_hit(Hit::Channel("c_standup".into()), &mut doc);
        assert!(app.call.active);
        assert_eq!(app.call.channel, "c_standup");
        // Joining records the local member in the shared voice membership.
        assert!(ChatDoc::list_voice_members(&doc, "c_standup").iter().any(|m| m.user == model::ME));
    }

    #[test]
    fn leaving_a_call_removes_local_member() {
        let (mut app, mut doc) = app_with_doc();
        app.on_hit(Hit::Channel("c_standup".into()), &mut doc);
        assert!(ChatDoc::list_voice_members(&doc, "c_standup").iter().any(|m| m.user == model::ME));
        app.on_hit(Hit::CallLeave, &mut doc);
        assert!(!app.call.active);
        assert!(!ChatDoc::list_voice_members(&doc, "c_standup").iter().any(|m| m.user == model::ME));
    }

    #[test]
    fn context_menu_delete_removes_a_channel() {
        let (mut app, mut doc) = app_with_doc();
        assert!(app.channels.iter().any(|c| c.id == "c_random"));
        app.on_hit(
            Hit::MenuAction { target: MenuTarget::Channel("c_random".into()), action: MenuAction::Delete },
            &mut doc,
        );
        assert!(!app.channels.iter().any(|c| c.id == "c_random"), "channel deleted via menu");
    }

    #[test]
    fn swipe_left_action_matches_context_menu_primary() {
        // The parity invariant: a left-swipe resolves to the same action as
        // the channel context menu's first item.
        assert_eq!(MenuAction::channel_swipe_left(), MenuAction::for_channel()[0]);
        assert_eq!(MenuAction::channel_swipe_left(), MenuAction::Delete);
    }

    #[test]
    fn add_channel_creates_and_opens_rename() {
        let (mut app, mut doc) = app_with_doc();
        let before = app.channels.len();
        app.on_hit(Hit::AddChannel { workspace: "ws_acme".into(), category: "cat_acme_main".into() }, &mut doc);
        assert_eq!(app.channels.len(), before + 1);
        assert!(app.rename.is_some(), "new channel opens the inline rename editor");
    }

    #[test]
    fn inline_rename_writes_through() {
        let (mut app, mut doc) = app_with_doc();
        app.begin_rename(MenuTarget::Channel("c_random".into()));
        // Replace the name by clearing + typing.
        if let Some((_, text)) = &mut app.rename {
            text.clear();
        }
        for ch in "watercooler".chars() {
            app.rename_key(&Key::Character(ch.to_string().into()), &mut doc);
        }
        app.commit_rename(&mut doc);
        let c = app.channels.iter().find(|c| c.id == "c_random").unwrap();
        assert_eq!(c.name, "watercooler");
    }

    #[test]
    fn toggle_left_flap_flips_state() {
        let (mut app, mut doc) = app_with_doc();
        let open = app.left_flap_open();
        app.on_hit(Hit::ToggleLeftFlap, &mut doc);
        assert_ne!(app.left_flap_open(), open, "toggling the left flap flips it");
    }

    #[test]
    fn backspace_edits_the_draft() {
        let (mut app, mut doc) = app_with_doc();
        app.focus = Focus::Main;
        app.insert_text(&mut doc, "abc");
        app.backspace(&mut doc);
        assert_eq!(app.draft_main, "ab");
        assert_eq!(app.caret, 2);
    }
}
