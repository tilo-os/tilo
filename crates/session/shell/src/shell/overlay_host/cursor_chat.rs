//! Cursor chat: press `/` with the pointer over a shared window and say
//! something in a bubble beside the pointer, live to everyone in that
//! window while it is typed, kept for a moment after Enter, gone after.
//! Talking, not chat: nothing is stored. A message that is one emoji is a
//! reaction, floating up from the pointer instead.
//!
//! The composer is the shell's own field: keys are edited by the chrome's
//! shared editing core ([`tilo_desktop::text_entry`]) and shown in the
//! toolkit's text field inside the bubble. What it says leaves through
//! [`ShellOverlayHost::take_cursor_chat_outbox`], which the frame hands to
//! the live presence plane for the window under the pointer.

use std::time::Instant;

use tilo_desktop::text_entry::{self, Edit, Mods};
use ui::text_selection::Selection;
use ui::widget::{TextInput, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, ModifiersState};

/// What the composer asks the live plane to say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChatOut {
    /// Say this beside the pointer (`true` once sent), or nothing.
    Say(Option<(String, bool)>),
    /// React with this emoji.
    React(String),
}

/// This person's cursor chat.
#[derive(Default)]
pub(crate) struct CursorChatComposer {
    /// A shared window is under the pointer, so `/` may open the composer.
    available: bool,
    /// What is said would reach a shared window: one is under the pointer,
    /// or one was said to before in this workspace.
    reachable: bool,
    /// The keyboard route claimed the `/` press that opens the composer.
    armed: bool,
    open: bool,
    text: String,
    selection: Selection,
    field: Option<TextInput>,
    /// What was sent and when, shown beside the pointer for its linger.
    sent: Option<(String, Instant)>,
    /// A reaction this person sent, floating up from their pointer.
    reaction: Option<(String, Instant)>,
    outbox: Vec<ChatOut>,
}

impl std::fmt::Debug for CursorChatComposer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CursorChatComposer")
            .field("available", &self.available)
            .field("open", &self.open)
            .field("text", &self.text)
            .finish_non_exhaustive()
    }
}

/// The key that opens cursor chat: `/` with no command modifier (Shift is
/// how some layouts type it).
fn opens_chat(key: &Key, modifiers: ModifiersState) -> bool {
    matches!(key, Key::Character(text) if text.as_str() == "/")
        && !modifiers.control_key()
        && !modifiers.alt_key()
        && !modifiers.super_key()
}

impl CursorChatComposer {
    fn say_typed(&mut self) {
        let text = self.text.trim();
        self.outbox.push(ChatOut::Say(
            (!text.is_empty()).then(|| (self.text.clone(), false)),
        ));
    }

    fn close(&mut self) {
        self.open = false;
        self.armed = false;
        self.text.clear();
        self.selection = Selection::default();
        self.field = None;
    }

    /// Enter: a lone emoji is a reaction, anything else is said and kept
    /// for its linger; nothing typed just closes.
    fn send(&mut self, now: Instant) {
        let text = self.text.trim().to_owned();
        if let Some(emoji) = ui::presence::single_emoji(&text) {
            self.outbox.push(ChatOut::React(emoji.to_owned()));
            self.reaction = Some((emoji.to_owned(), now));
            self.sent = None;
        } else if text.is_empty() {
            self.outbox.push(ChatOut::Say(None));
        } else {
            self.outbox.push(ChatOut::Say(Some((text.clone(), true))));
            self.sent = Some((text, now));
        }
        self.close();
    }

    /// Esc: take back whatever was showing.
    fn cancel(&mut self) {
        self.outbox.push(ChatOut::Say(None));
        self.close();
    }

    /// Keep the message within the live plane's bounds while it is typed.
    fn clamp(&mut self) {
        let max_chars = ui::presence::CURSOR_CHAT_MAX_CHARS;
        let max_bytes = tilo_relay_protocol::live::MAX_CURSOR_CHAT_BYTES;
        if self.text.chars().count() <= max_chars && self.text.len() <= max_bytes {
            return;
        }
        let mut kept = String::new();
        for (count, character) in self.text.chars().enumerate() {
            if count >= max_chars || kept.len() + character.len_utf8() > max_bytes {
                break;
            }
            kept.push(character);
        }
        self.text = kept;
        let end = self.text.chars().count();
        self.selection = Selection {
            anchor: self.selection.anchor.min(end),
            cursor: self.selection.cursor.min(end),
        };
    }

    /// The linger of what was sent is over: stop saying it.
    fn expire(&mut self, now: Instant) {
        if let Some((_, at)) = &self.sent
            && now.saturating_duration_since(*at) >= ui::presence::CURSOR_CHAT_LINGER
        {
            self.sent = None;
            if !self.open {
                self.outbox.push(ChatOut::Say(None));
            }
        }
        if let Some((_, at)) = &self.reaction
            && now.saturating_duration_since(*at) >= ui::motion::Float::LIFE
        {
            self.reaction = None;
        }
    }
}

impl super::ShellOverlayHost {
    /// Whether cursor chat can start: the frame says a shared window is
    /// under the pointer, and nothing else of the shell's has the keyboard.
    pub(crate) fn set_cursor_chat_available(&mut self, available: bool) {
        self.cursor_chat.available = available;
    }

    /// Whether the keyboard route should keep this key from the focused
    /// window because it opens cursor chat. The caller has already checked
    /// that the focused window takes no text.
    pub(crate) fn cursor_chat_claims(&self, key: &Key, modifiers: ModifiersState) -> bool {
        self.cursor_chat.available
            && !self.cursor_chat.open
            && !self.person_cursor_controls_suppressed()
            && opens_chat(key, modifiers)
    }

    /// Open the composer on purpose (Super+/, the avatar menu's "Say
    /// something here"), whatever has the keyboard: beside the pointer over
    /// the shared window under it, or the one it last went to. Says so when
    /// there is no shared window to say it in.
    pub(crate) fn open_cursor_chat_on_request(&mut self) {
        if self.cursor_chat.open || self.person_cursor_controls_suppressed() {
            return;
        }
        if !self.cursor_chat.reachable {
            if let Some(chrome) = self.chrome.as_ref() {
                chrome.borrow_mut().push_warning_notification(
                    crate::shell::i18n::t("cursor-chat-no-window"),
                    crate::shell::i18n::t("cursor-chat-no-window-body"),
                );
            }
            return;
        }
        let chat = &mut self.cursor_chat;
        chat.armed = false;
        chat.open = true;
        chat.sent = None;
    }

    /// Whether what is said would reach a shared window: one is under the
    /// pointer, or one was said to before in this workspace.
    pub(crate) fn set_cursor_chat_reachable(&mut self, reachable: bool) {
        self.cursor_chat.reachable = reachable;
    }

    /// The keyboard route kept this `/` from the focused window: the
    /// chrome's dispatch of the same press opens the composer.
    pub(crate) fn arm_cursor_chat(&mut self) {
        self.cursor_chat.armed = true;
    }

    /// The composer is open and takes every key.
    pub(crate) fn cursor_chat_open(&self) -> bool {
        self.cursor_chat.open
    }

    /// What to say on the live plane since the last call, oldest first.
    pub(crate) fn take_cursor_chat_outbox(&mut self, now: Instant) -> Vec<ChatOut> {
        self.cursor_chat.expire(now);
        std::mem::take(&mut self.cursor_chat.outbox)
    }

    /// Whether this person's own bubble or reaction is moving at `now`: a
    /// reaction floating, a sent message arriving or fading.
    pub(crate) fn cursor_chat_animating(&self, now: Instant) -> bool {
        let chat = &self.cursor_chat;
        let motion = self.motion.get();
        chat.reaction.is_some()
            || chat.sent.as_ref().is_some_and(|(_, sent)| {
                ui::motion::Transient::is_moving(
                    ui::motion::Transient::READOUT,
                    (*sent, *sent + ui::presence::CURSOR_CHAT_LINGER),
                    now,
                    motion,
                )
            })
    }

    /// When this person's sent message starts to fade, or goes.
    pub(super) fn cursor_chat_deadline(&self) -> Option<Instant> {
        let (_, sent) = self.cursor_chat.sent.as_ref()?;
        let gone = *sent + ui::presence::CURSOR_CHAT_LINGER;
        let now = Instant::now();
        [
            ui::motion::Transient::exit_starts(
                ui::motion::Transient::READOUT,
                gone,
                self.motion.get(),
            ),
            gone,
        ]
        .into_iter()
        .find(|at| *at > now)
    }

    /// Keys and typed text for the composer, and the `/` that opens it.
    /// Returns whether the event was the composer's.
    pub(super) fn route_cursor_chat_event(
        &mut self,
        event: &WindowEvent,
        modifiers: ModifiersState,
    ) -> bool {
        let chat = &mut self.cursor_chat;
        match event {
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key, state, ..
                },
                ..
            } => {
                if !chat.open {
                    if *state == ElementState::Pressed
                        && chat.armed
                        && opens_chat(logical_key, modifiers)
                    {
                        chat.armed = false;
                        chat.open = true;
                        chat.sent = None;
                        return true;
                    }
                    return false;
                }
                if *state != ElementState::Pressed {
                    return true;
                }
                match text_entry::apply(
                    logical_key,
                    Mods::from_state(modifiers),
                    &mut chat.text,
                    &mut chat.selection,
                ) {
                    Edit::Submit => chat.send(Instant::now()),
                    Edit::Cancel => chat.cancel(),
                    Edit::Changed => {
                        chat.clamp();
                        chat.say_typed();
                    }
                    Edit::ConsumedQuiet | Edit::Ignored => {}
                }
                true
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) if chat.open => {
                if text_entry::commit(text, &mut chat.text, &mut chat.selection) == Edit::Changed {
                    chat.clamp();
                    chat.say_typed();
                }
                true
            }
            _ => false,
        }
    }

    /// This person's own bubble beside their pointer: the composer while it
    /// is open, what they sent while it lingers, and their reaction.
    /// Returns the bounds it painted.
    pub(super) fn draw_cursor_chat(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
    ) -> Vec<Rect> {
        let mut painted = Vec::new();
        let chat = &self.cursor_chat;
        if !chat.open && chat.sent.is_none() && chat.reaction.is_none() {
            return painted;
        }
        let Some(at) = self.last_cursor else {
            return painted;
        };
        let now = Instant::now();
        let motion = theme.motion();
        let (r, g, b) = ui::presence::identity_rgb(crate::shell::state::local_presence_hue());
        let colour = theme.tokens.colors.presence.of_rgb((r, g, b));
        let name = ui::local_identity::load()
            .map(|identity| identity.display_name)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| crate::shell::i18n::t("cursor-chat-you"));
        let chat = &mut self.cursor_chat;
        if chat.open {
            let placeholder = crate::shell::i18n::t("cursor-chat-placeholder");
            let shown = if chat.text.is_empty() {
                placeholder.as_str()
            } else {
                chat.text.as_str()
            };
            let (bubble, line) = ui::presence::paint_own_chat_bubble(
                scene,
                transform,
                text_renderer,
                theme,
                (at, &name, colour),
                (shown, 120.0, self.rect),
            );
            painted.push(transform.transform_rect_bbox(bubble.inflate(8.0, 8.0)));
            let field = chat.field.get_or_insert_with(|| {
                TextInput::new(Vec::new())
                    .with_plain_chrome()
                    .with_spellcheck(false)
                    .with_autocorrect(false)
                    .with_text_role(ui::TextRole::Body)
            });
            field.set_text(&chat.text);
            field.set_selection(chat.selection.anchor, chat.selection.cursor);
            field.set_focus(true);
            field.apply_layout_external(line);
            // The field paints in the bubble's ink, on the talker's colour.
            let ink = ui::presence::contrast_ink(colour);
            let mut on_colour = theme.clone();
            on_colour.tokens.colors.text_primary = ink;
            on_colour.tokens.colors.text_dim = ui::presence::contrast_ink_dim(colour);
            if chat.text.is_empty() {
                text_renderer.draw_style(
                    scene,
                    &placeholder,
                    ui::presence::contrast_ink_dim(colour),
                    transform * Affine::translate((line.x0, line.y0)),
                    None,
                    &theme.text(ui::TextRole::Body),
                );
            }
            field.draw(
                scene,
                transform,
                text_renderer,
                &on_colour,
                &ui::presence::PresenceCtx::default(),
            );
        } else if let Some((text, sent)) = &chat.sent {
            let shown = ui::motion::Transient::opacity(
                ui::motion::Transient::READOUT,
                (*sent, *sent + ui::presence::CURSOR_CHAT_LINGER),
                now,
                motion,
            );
            if shown > 0.0 {
                let mut layer = Scene::new();
                let (bubble, line) = ui::presence::paint_own_chat_bubble(
                    &mut layer,
                    Affine::IDENTITY,
                    text_renderer,
                    theme,
                    (at, &name, colour),
                    (text, 0.0, self.rect),
                );
                text_renderer.draw_style(
                    &mut layer,
                    text,
                    ui::presence::contrast_ink(colour),
                    Affine::translate((line.x0, line.y0)),
                    None,
                    &theme.text(ui::TextRole::Body),
                );
                let bounds = bubble.inflate(8.0, 8.0);
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    shown,
                    transform,
                    &bounds,
                );
                scene.append(&layer, Some(transform));
                scene.pop_layer();
                painted.push(transform.transform_rect_bbox(bounds));
            }
        }
        if let Some((emoji, started)) = &chat.reaction
            && let Some((rise, opacity)) =
                ui::motion::Float::at(now.saturating_duration_since(*started), motion)
        {
            painted.push(ui::presence::paint_reaction(
                scene,
                transform,
                text_renderer,
                theme,
                at,
                &ui::presence::ReactionFloat {
                    emoji: emoji.clone(),
                    rise,
                    opacity,
                },
            ));
        }
        painted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::NamedKey;

    fn typed(chat: &mut CursorChatComposer, text: &str) {
        for character in text.chars() {
            let key = Key::Character(character.to_string().into());
            assert_eq!(
                text_entry::apply(&key, Mods::default(), &mut chat.text, &mut chat.selection),
                Edit::Changed
            );
            chat.clamp();
            chat.say_typed();
        }
    }

    /// Typing says each change live; Enter sends and keeps it for its
    /// linger, after which it is taken back; Esc takes it back at once.
    #[test]
    fn typing_is_said_live_enter_sends_and_escape_takes_it_back() {
        let mut chat = CursorChatComposer {
            open: true,
            ..CursorChatComposer::default()
        };
        typed(&mut chat, "hi");
        assert_eq!(
            chat.outbox,
            vec![
                ChatOut::Say(Some(("h".into(), false))),
                ChatOut::Say(Some(("hi".into(), false))),
            ]
        );
        chat.outbox.clear();
        let now = Instant::now();
        chat.send(now);
        assert!(!chat.open);
        assert_eq!(chat.outbox, vec![ChatOut::Say(Some(("hi".into(), true)))]);
        chat.outbox.clear();
        chat.expire(now + std::time::Duration::from_secs(1));
        assert!(chat.outbox.is_empty(), "still lingering");
        chat.expire(now + ui::presence::CURSOR_CHAT_LINGER);
        assert_eq!(chat.outbox, vec![ChatOut::Say(None)]);

        let mut chat = CursorChatComposer {
            open: true,
            ..CursorChatComposer::default()
        };
        typed(&mut chat, "oops");
        chat.outbox.clear();
        chat.cancel();
        assert_eq!(chat.outbox, vec![ChatOut::Say(None)]);
        assert!(chat.text.is_empty() && !chat.open);
    }

    /// One emoji alone is a reaction, not a message.
    #[test]
    fn one_emoji_is_sent_as_a_reaction() {
        let mut chat = CursorChatComposer {
            open: true,
            text: "\u{1f389}".into(),
            ..CursorChatComposer::default()
        };
        chat.send(Instant::now());
        assert_eq!(chat.outbox, vec![ChatOut::React("\u{1f389}".into())]);
        assert!(chat.reaction.is_some() && chat.sent.is_none());
    }

    /// A message never grows past what the live plane carries.
    #[test]
    fn a_message_stops_at_the_live_planes_bound() {
        let mut chat = CursorChatComposer {
            open: true,
            ..CursorChatComposer::default()
        };
        typed(
            &mut chat,
            &"a".repeat(ui::presence::CURSOR_CHAT_MAX_CHARS + 5),
        );
        assert_eq!(
            chat.text.chars().count(),
            ui::presence::CURSOR_CHAT_MAX_CHARS
        );
        let Some(ChatOut::Say(Some((last, false)))) = chat.outbox.last() else {
            panic!("typed text is said");
        };
        assert_eq!(last.chars().count(), ui::presence::CURSOR_CHAT_MAX_CHARS);
    }

    /// Super+/ and the avatar menu open it whatever has the keyboard, as
    /// long as what is said reaches a shared window; with none, the person
    /// is told where to point instead.
    #[test]
    fn asking_for_it_opens_it_wherever_the_keyboard_is() {
        let tree = crate::shell::state::WorkspaceTree::bootstrap_demo();
        let (mut host, chrome) = crate::shell::build_tilo_overlay_host_with_chrome(&tree);
        let before = chrome.borrow().notifications().len();
        host.open_cursor_chat_on_request();
        assert!(!host.cursor_chat_open(), "no shared window: nothing opens");
        assert!(
            chrome.borrow().notifications().len() > before,
            "and the person is told where to point"
        );
        host.set_cursor_chat_reachable(true);
        host.open_cursor_chat_on_request();
        assert!(host.cursor_chat_open(), "a shared window: it opens");
    }

    /// `/` opens it, also where Shift types it; a command chord with `/`
    /// is not chat.
    #[test]
    fn slash_opens_it_and_chords_do_not() {
        let slash = Key::Character("/".into());
        assert!(opens_chat(&slash, ModifiersState::empty()));
        assert!(opens_chat(&slash, ModifiersState::SHIFT));
        assert!(!opens_chat(&slash, ModifiersState::CONTROL));
        assert!(!opens_chat(&slash, ModifiersState::SUPER));
        assert!(!opens_chat(
            &Key::Named(NamedKey::Enter),
            ModifiersState::empty()
        ));
    }
}

#[cfg(feature = "hosted-test-control")]
impl super::ShellOverlayHost {
    pub(super) fn cursor_chat_open_for_shots(&mut self, text: &str) {
        let chat = &mut self.cursor_chat;
        chat.open = true;
        chat.text = text.to_owned();
        let end = chat.text.chars().count();
        chat.selection = Selection {
            anchor: end,
            cursor: end,
        };
    }
}
