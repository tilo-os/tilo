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
use crate::model::{self, ChannelKind, Id};

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(NOW)
}

/// Byte offset of character index `ci` in `s` (clamped to the end).
fn char_byte(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
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
            // One of your own messages — offer the message actions (Edit).
            Hit::MessageMenu { msg } => Some(ContextMenu {
                target: MenuTarget::Message(msg),
                anchor: self.last_mouse,
                items: MenuAction::for_message().to_vec(),
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
            // Left-click on your own message body is a no-op — the edit menu
            // is a right-click affordance (see `on_right_click`).
            Hit::MessageMenu { .. } => {}
            Hit::EditSave => self.commit_edit(doc),
            Hit::EditCancel => self.cancel_edit(),
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
            Hit::Attach => self.open_attach_picker(),
            Hit::AttachPick(path) => {
                self.attach_open = false;
                self.attach_file(doc, std::path::Path::new(&path));
            }
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
                if self.attach_open {
                    self.attach_open = false;
                } else if self.menu.is_some() {
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
            MenuTarget::Message(id) => {
                if action == MenuAction::EditMessage {
                    self.begin_edit(id);
                }
            }
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
            MenuTarget::Message(_) | MenuTarget::Workspace(_) => None,
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
                    MenuTarget::Message(_) | MenuTarget::Workspace(_) => {}
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

        // The inline message editor captures keys while open: Enter saves,
        // Escape cancels; every other key edits its working text through the
        // shared handlers below (which target `self.editing` when it's set).
        if self.editing.is_some() {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.cancel_edit();
                    return true;
                }
                Key::Named(NamedKey::Enter) if !modifiers.state().shift_key() => {
                    self.commit_edit(doc);
                    return true;
                }
                _ => {}
            }
        }

        // Escape unwinds the top-most layer.
        if let Key::Named(NamedKey::Escape) = key {
            if self.attach_open {
                self.attach_open = false;
            } else if self.menu.is_some() {
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
        if self.emoji_open || self.history_open.is_some() || self.menu.is_some() || self.attach_open {
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
                self.caret = (self.caret + 1).min(self.editable_char_len());
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                // From an empty composer, ArrowUp loads your last message into
                // the inline editor (Slack/Discord convention). Ignored once an
                // edit is already in progress or a draft is being composed.
                if self.editing.is_none() && self.focused_draft().is_empty() {
                    if let Some(id) = self.last_own_message_id() {
                        self.begin_edit(id);
                    }
                }
                true
            }
            Key::Named(NamedKey::Home) => {
                self.caret = 0;
                true
            }
            Key::Named(NamedKey::End) => {
                self.caret = self.editable_char_len();
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
        // The inline message editor keeps its working text in memory (like the
        // rename editor) so an in-progress edit isn't published until saved.
        if let Some((_, text)) = &mut self.editing {
            let at = char_byte(text, self.caret);
            text.insert_str(at, s);
            self.caret += s.chars().count();
            return;
        }
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
        if let Some((_, text)) = &mut self.editing {
            let end = char_byte(text, self.caret);
            let start = char_byte(text, self.caret - 1);
            text.replace_range(start..end, "");
            self.caret -= 1;
            return;
        }
        let key = self.draft_key();
        let obj = ChatDoc::draft_obj(doc, &key);
        let _ = doc.splice_text(&obj, self.caret - 1, 1, "");
        self.caret -= 1;
        self.reload(doc);
    }

    /// Character count of whatever text the caret currently indexes: the
    /// inline message editor when one is open, otherwise the focused draft.
    fn editable_char_len(&self) -> usize {
        match &self.editing {
            Some((_, text)) => text.chars().count(),
            None => self.focused_draft().chars().count(),
        }
    }

    /// Enter edit mode on one of your own messages: load its current body into
    /// the inline editor with the caret at the end. Only your own messages are
    /// editable; a foreign id is ignored. Closes any open popover.
    pub(crate) fn begin_edit(&mut self, id: Id) {
        let Some(m) = self.message(&id) else { return };
        if m.author != model::ME {
            return;
        }
        let text = m.text.clone();
        self.caret = text.chars().count();
        self.editing = Some((id, text));
        self.focus = Focus::Main;
        self.menu = None;
        self.emoji_open = false;
        self.emoji_target = None;
        self.history_open = None;
    }

    /// The id of your most recent message in the surface the composer is
    /// focused on (the main timeline, or the open thread) — the target of
    /// ArrowUp-to-edit from an empty composer.
    fn last_own_message_id(&self) -> Option<Id> {
        let in_thread = matches!(self.focus, Focus::Thread) && self.thread_open.is_some();
        let parent = self.thread_open.clone();
        self.messages
            .iter()
            .rev()
            .find(|m| {
                m.author == model::ME
                    && if in_thread { m.parent == parent } else { m.parent.is_none() }
            })
            .map(|m| m.id.clone())
    }

    /// Commit the inline message editor: write the new body through
    /// [`ChatDoc::edit_message`] (which archives the prior text into the
    /// message's history and flags it edited), then close the editor. An
    /// edit that empties the message is discarded rather than blanking it.
    fn commit_edit(&mut self, doc: &mut AutoCommit) {
        let Some((id, text)) = self.editing.take() else { return };
        self.caret = 0;
        if text.trim().is_empty() {
            return;
        }
        ChatDoc::edit_message(doc, &self.active_channel, &id, &text, now_secs());
        self.reload(doc);
        self.persist(doc);
    }

    /// Discard the inline message editor without writing.
    fn cancel_edit(&mut self) {
        self.editing = None;
        self.caret = 0;
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

    /// Open the workspace file picker (the composer "+"). Lists the real
    /// image files in the shared workspace so the user shares an actual file —
    /// nothing is fabricated. In a headless environment with no workspace the
    /// list is empty and the picker says so.
    fn open_attach_picker(&mut self) {
        self.menu = None;
        self.emoji_open = false;
        self.emoji_target = None;
        self.history_open = None;
        self.attach_entries = crate::picker::list_images(&crate::picker::attach_dir());
        self.attach_open = true;
    }

    /// Share the real file at `path` into the active channel: read its true
    /// pixel dimensions from the header and post it with its actual filesystem
    /// path, so the shared [`ui::ImageStore`] decodes the real bytes (projected
    /// from the FUSE workspace). No caption is invented — the image stands on
    /// its own. Returns whether a message was posted (a non-image / missing
    /// file posts nothing).
    pub(crate) fn attach_file(&mut self, doc: &mut AutoCommit, path: &std::path::Path) -> bool {
        use crate::model::Attachment;
        let Some((w, h)) = crate::picker::image_dimensions(path) else {
            return false;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".into());
        ChatDoc::push_message(
            doc,
            &self.active_channel,
            NewMessage {
                author: model::ME.into(),
                ts: now_secs(),
                text: String::new(),
                attachment: Some(Attachment::Image {
                    name,
                    path: path.to_string_lossy().into_owned(),
                    w,
                    h,
                }),
                ..Default::default()
            },
        );
        self.scroll = 0.0;
        self.reload(doc);
        self.persist(doc);
        true
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
    fn live_starter_selects_first_real_workspace_and_channel() {
        // Against a real (minimal) starter document whose ids differ from the
        // demo defaults the app boots with, `ensure_selection` must re-home
        // the active workspace/channel and drop the stale demo thread.
        let mut doc = AutoCommit::new();
        ChatDoc::seed_starter(&mut doc, model::ME, "You", "Y");
        let mut app = ChatApp::new_ephemeral(); // bare view state → demo ids
        app.reload(&doc);
        if app.ensure_selection() {
            app.reload(&doc);
        }
        assert_eq!(app.active_ws, "ws_home", "re-homed to the real workspace");
        assert_eq!(app.active_channel, "c_general", "picked the first text channel");
        assert!(app.thread_open.is_none(), "stale demo thread dropped");
        // And nothing fake leaked in.
        assert!(app.users.iter().all(|u| u.id == model::ME));
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
    fn attachment_image_decodes_through_the_image_store() {
        // The concept message in #design-crit carries an image attachment;
        // prewarming must drive it through the real fetch → decode → cache
        // path so a frame paints a decoded thumbnail (not a placeholder).
        let (app, _doc) = app_with_doc();
        assert!(
            app.messages.iter().any(|m| matches!(
                &m.attachment,
                Some(crate::model::Attachment::Image { path, .. }) if path == crate::images::DEMO_CALL_GRID
            )),
            "seeded conversation has an image attachment"
        );
        app.prewarm_images();
        assert!(
            app.images.get(crate::images::DEMO_CALL_GRID).is_some(),
            "the shared ImageStore decoded the attachment thumbnail"
        );
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
    fn expanding_a_call_keeps_the_full_remote_roster() {
        // Reaching the expanded overlay through the real hit path: joining the
        // demo huddle and hitting Expand must leave every remote in the call,
        // so the overlay lays out host + three remote tiles (which the grid
        // packer then keeps inside the window — see render_call's grid test).
        let (mut app, mut doc) = app_with_doc();
        app.on_hit(Hit::Channel("c_huddle".into()), &mut doc); // join the huddle
        assert!(app.call.active && app.call.channel == "c_huddle");
        app.on_hit(Hit::CallExpand, &mut doc);
        assert!(app.call.expanded, "the Expand control opens the full-window overlay");
        let remotes = ChatDoc::list_voice_members(&doc, "c_huddle")
            .into_iter()
            .filter(|m| m.user != model::ME)
            .count();
        assert_eq!(remotes, 3, "all three remote participants remain in the call");
    }

    #[test]
    fn expanded_overlay_fills_the_surface_at_every_width() {
        // The narrow/portrait void regression, exercised through the real app:
        // join the demo huddle, expand, and at each target width the host +
        // remote tiles must cover the whole content band — no large empty
        // region beneath or beside the host.
        use vello::kurbo::Rect;
        let (mut app, mut doc) = app_with_doc();
        app.on_hit(Hit::Channel("c_huddle".into()), &mut doc);
        app.on_hit(Hit::CallExpand, &mut doc);
        assert!(app.call.active && app.call.expanded);

        for &(w, h) in &[(1280.0_f64, 720.0_f64), (800.0, 600.0), (560.0, 720.0)] {
            app.rect = Rect::new(0.0, 0.0, w, h);
            let region = app.overlay_content_region();
            let tiles = app.overlay_tiles();
            assert_eq!(tiles.len(), 4, "w={w}: host + three remotes → four tiles");
            // Every tile inside the region.
            for t in &tiles {
                assert!(
                    t.x0 >= region.x0 - 0.5 && t.x1 <= region.x1 + 0.5
                        && t.y0 >= region.y0 - 0.5 && t.y1 <= region.y1 + 0.5,
                    "w={w}: a tile escaped the content region"
                );
            }
            // Bounding box covers the region (within one gap) in both axes.
            let bbox = tiles.iter().fold(
                Rect::new(f64::MAX, f64::MAX, f64::MIN, f64::MIN),
                |b, r| Rect::new(b.x0.min(r.x0), b.y0.min(r.y0), b.x1.max(r.x1), b.y1.max(r.y1)),
            );
            let slack = region.width().min(region.height()) * 0.05 + 17.0;
            assert!(
                region.x1 - bbox.x1 <= slack && bbox.x0 - region.x0 <= slack,
                "w={w}: horizontal void — bbox x {:?} vs region x [{:.0},{:.0}]",
                (bbox.x0, bbox.x1), region.x0, region.x1
            );
            assert!(
                region.y1 - bbox.y1 <= slack && bbox.y0 - region.y0 <= slack,
                "w={w}: vertical void below the tiles — bbox y1 {:.0} vs region y1 {:.0}",
                bbox.y1, region.y1
            );
            // The host is a genuine cell, not a pinned sliver.
            let host = app.overlay_host_rect();
            assert_eq!(host, tiles[0], "host is the first grid cell");
            assert!(
                host.width() > region.width() * 0.25 && host.height() > region.height() * 0.25,
                "w={w}: host tile is a real share of the surface, not a corner sliver"
            );
        }
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

    fn write_real_png(path: &std::path::Path, w: u32, h: u32) {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([80, 160, 220, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(path, bytes.into_inner()).unwrap();
    }

    #[test]
    fn attach_shares_a_real_file_with_true_dimensions_and_no_caption() {
        // The real attach path: picking a file posts *that* file (its real
        // path + true header dimensions) with no invented caption — the exact
        // opposite of fabricating content into the conversation.
        let (mut app, mut doc) = app_with_doc();
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("diagram.png");
        write_real_png(&png, 240, 160);

        let before = ChatDoc::list_messages(&doc, "c_crit").len();
        app.on_hit(Hit::AttachPick(png.to_string_lossy().into_owned()), &mut doc);
        let msgs = ChatDoc::list_messages(&doc, "c_crit");
        assert_eq!(msgs.len(), before + 1, "one real attachment posted");
        let last = msgs.last().unwrap();
        assert_eq!(last.text, "", "no invented caption");
        match &last.attachment {
            Some(model::Attachment::Image { path, w, h, .. }) => {
                assert_eq!(path, &png.to_string_lossy(), "posts the real filesystem path");
                assert_eq!((*w, *h), (240, 160), "reads the file's true header dimensions");
            }
            other => panic!("expected a real image attachment, got {other:?}"),
        }
    }

    #[test]
    fn attach_button_opens_picker_listing_real_workspace_files() {
        let (mut app, mut doc) = app_with_doc();
        let dir = tempfile::tempdir().unwrap();
        write_real_png(&dir.path().join("shot.png"), 64, 64);
        // Point the workspace picker at the temp dir and open it via the real
        // event path (the composer "+").
        unsafe { std::env::set_var("QOS_CHAT_ATTACH_DIR", dir.path()) };
        app.on_hit(Hit::Attach, &mut doc);
        unsafe { std::env::remove_var("QOS_CHAT_ATTACH_DIR") };
        assert!(app.attach_open, "the picker opens");
        assert!(
            app.attach_entries.iter().any(|e| e.name == "shot.png"),
            "a real workspace image is listed, not a synthesized stand-in"
        );
    }

    #[test]
    fn flaps_reflow_when_width_changes_on_the_same_instance() {
        // Guard against "responsive on first launch, frozen after resize": the
        // flap mode is re-derived from the *current* width every layout pass,
        // never cached from the first frame.
        use crate::responsive::FlapMode;
        let (mut app, _doc) = app_with_doc();
        app.rect = vello::kurbo::Rect::new(0.0, 0.0, 1280.0, 720.0);
        app.reconcile_flaps();
        assert_eq!(app.left_mode, FlapMode::Docked);
        // Resize narrow on the SAME instance.
        app.rect = vello::kurbo::Rect::new(0.0, 0.0, 560.0, 720.0);
        app.reconcile_flaps();
        assert_eq!(app.left_mode, FlapMode::Overlay, "narrowing reflows to overlay");
        // And widen again.
        app.rect = vello::kurbo::Rect::new(0.0, 0.0, 1280.0, 720.0);
        app.reconcile_flaps();
        assert_eq!(app.left_mode, FlapMode::Docked, "widening reflows back to docked");
    }

    /// Drive an edit end to end through the real handlers: the message action
    /// menu's Edit loads the body into the inline editor, keystrokes revise it,
    /// and Enter writes it through `ChatDoc::edit_message` — flagging the
    /// message edited and archiving the prior body into its history.
    #[test]
    fn edit_via_menu_writes_through_edit_message() {
        let (mut app, mut doc) = app_with_doc();
        let original = app.message("m_me1").unwrap().text.clone();
        let history_before = app.message("m_me1").unwrap().history.len();
        assert!(!app.message("m_me1").unwrap().edited);

        // The right-click menu resolves to this action; invoke it directly.
        app.on_hit(
            Hit::MenuAction {
                target: MenuTarget::Message("m_me1".into()),
                action: MenuAction::EditMessage,
            },
            &mut doc,
        );
        assert!(app.editing.is_some(), "Edit loads the message into the inline editor");
        assert_eq!(app.editing.as_ref().unwrap().1, original, "editor seeded with the current body");

        let mods = Modifiers::default();
        // Clear the working text, then type a new body — all via the key path.
        while app.editing.as_ref().is_some_and(|(_, t)| !t.is_empty()) {
            app.on_key(&Key::Named(NamedKey::Backspace), &mods, &mut doc);
        }
        for ch in "Revised: host tile carries the follow control.".chars() {
            let k = if ch == ' ' {
                Key::Named(NamedKey::Space)
            } else {
                Key::Character(ch.to_string().into())
            };
            app.on_key(&k, &mods, &mut doc);
        }
        // Enter commits.
        app.on_key(&Key::Named(NamedKey::Enter), &mods, &mut doc);

        assert!(app.editing.is_none(), "saving closes the editor");
        let m = app.message("m_me1").unwrap();
        assert_eq!(m.text, "Revised: host tile carries the follow control.");
        assert!(m.edited, "the message is flagged edited");
        assert_eq!(m.history.len(), history_before + 1, "history grew by one revision");
        assert_eq!(m.history.last().unwrap().text, original, "history holds the original body");
    }

    /// ArrowUp in an empty composer loads your most recent message into the
    /// inline editor (Slack/Discord convention).
    #[test]
    fn arrow_up_in_empty_composer_edits_last_own_message() {
        let (mut app, mut doc) = app_with_doc();
        app.focus = Focus::Main;
        assert!(app.draft_main.is_empty(), "composer starts empty");
        app.on_key(&Key::Named(NamedKey::ArrowUp), &Modifiers::default(), &mut doc);
        let (id, text) = app.editing.clone().expect("ArrowUp opened the editor");
        assert_eq!(id, "m_me2", "loaded the last own top-level message");
        assert_eq!(text, app.message("m_me2").unwrap().text, "seeded with its current body");
    }

    /// ArrowUp does nothing once a draft is being composed — it must not
    /// clobber in-progress text with an edit.
    #[test]
    fn arrow_up_with_a_draft_does_not_start_editing() {
        let (mut app, mut doc) = app_with_doc();
        app.focus = Focus::Main;
        app.insert_text(&mut doc, "half a thought");
        app.on_key(&Key::Named(NamedKey::ArrowUp), &Modifiers::default(), &mut doc);
        assert!(app.editing.is_none(), "a non-empty composer suppresses ArrowUp-to-edit");
    }

    /// Escape cancels an in-progress edit and writes nothing.
    #[test]
    fn edit_escape_discards_without_writing() {
        let (mut app, mut doc) = app_with_doc();
        let original = app.message("m_me1").unwrap().text.clone();
        app.begin_edit("m_me1".into());
        app.on_key(&Key::Character("Z".into()), &Modifiers::default(), &mut doc);
        app.on_key(&Key::Named(NamedKey::Escape), &Modifiers::default(), &mut doc);
        assert!(app.editing.is_none(), "Escape closes the editor");
        let m = app.message("m_me1").unwrap();
        assert_eq!(m.text, original, "cancel leaves the message untouched");
        assert!(!m.edited, "cancel does not flag the message edited");
    }

    /// Only your own messages are editable — loading a foreign message is a
    /// no-op, so there is no way to rewrite someone else's words.
    #[test]
    fn cannot_edit_another_authors_message() {
        let (mut app, _doc) = app_with_doc();
        assert_eq!(app.message("m_open").unwrap().author, "u_aria");
        app.begin_edit("m_open".into());
        assert!(app.editing.is_none(), "a foreign message never enters the editor");
    }

    #[test]
    fn host_call_tile_has_no_fabricated_video_frame() {
        // The host video surface must be empty until a real capture feeds it —
        // never a synthesized gradient "face".
        let app = ChatApp::new_ephemeral();
        assert!(
            app.video.latest_frame().is_none(),
            "no frame is fabricated at construction"
        );
    }
}
