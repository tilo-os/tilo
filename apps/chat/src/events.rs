//! Input routing for [`ChatApp`]. Mouse presses hit-test the regions
//! recorded during the previous `draw`; keyboard input drives the focused
//! composer, splicing directly into the per-channel draft `Text` object in
//! the CRDT (so a draft is collaborative state, and undo/redo come for
//! free from the toolkit's snapshotting).

use automerge::transaction::Transactable;
use automerge::AutoCommit;
use vello::kurbo::Point;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::app::{ChatApp, Focus, Hit};
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
                if let Some(hit) = self.hit_at(self.last_mouse) {
                    self.on_hit(hit, doc);
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
                    self.call_active = true;
                    self.call_channel = id;
                    self.call_expanded = false;
                    self.request_relayout();
                } else {
                    self.active_channel = id;
                    self.thread_open = None;
                    self.scroll = 0.0;
                    self.focus = Focus::Main;
                    self.caret = 0;
                }
            }
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
                if self.call_active {
                    self.call_expanded = !self.call_expanded;
                    self.request_relayout();
                } else {
                    self.call_active = true;
                    if let Some(v) = self
                        .ws_channels()
                        .into_iter()
                        .find(|c| c.kind == ChannelKind::Voice)
                        .map(|c| c.id.clone())
                    {
                        self.call_channel = v;
                    }
                }
            }
            Hit::CloseThread => {
                self.thread_open = None;
                self.focus = Focus::Main;
            }
            Hit::CallMute => self.muted = !self.muted,
            Hit::CallAudioOnly => self.audio_only = !self.audio_only,
            Hit::CallFollow => self.follow_host = !self.follow_host,
            Hit::CallExpand => {
                self.call_expanded = !self.call_expanded;
                self.request_relayout();
            }
            Hit::CallLeave => {
                self.call_active = false;
                self.call_expanded = false;
                self.request_relayout();
            }
            Hit::Dismiss => {
                if self.emoji_open {
                    self.emoji_open = false;
                    self.emoji_target = None;
                } else if self.history_open.is_some() {
                    self.history_open = None;
                }
            }
        }
        self.reload(doc);
    }

    fn on_key(&mut self, key: &Key, modifiers: &Modifiers, doc: &mut AutoCommit) -> bool {
        // Escape unwinds the top-most layer.
        if let Key::Named(NamedKey::Escape) = key {
            if self.emoji_open {
                self.emoji_open = false;
                self.emoji_target = None;
            } else if self.history_open.is_some() {
                self.history_open = None;
            } else if self.call_expanded {
                self.call_expanded = false;
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
        if self.emoji_open || self.history_open.is_some() {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::ChatDoc;

    fn app_with_doc() -> (ChatApp, AutoCommit) {
        let mut doc = AutoCommit::new();
        ChatDoc::seed_if_empty(&mut doc);
        let mut app = ChatApp::new();
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
        app.call_active = false;
        app.on_hit(Hit::Channel("c_standup".into()), &mut doc);
        assert!(app.call_active);
        assert_eq!(app.call_channel, "c_standup");
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
