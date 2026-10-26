//! Input routing for [`ChatApp`]. Mouse presses hit-test the regions
//! recorded during the previous `draw`; keyboard input drives the focused
//! composer, splicing directly into the per-channel draft `Text` object in
//! the CRDT (so a draft is collaborative state, and undo/redo come for
//! free from the toolkit's snapshotting).

#[cfg(test)]
use automerge::AutoCommit;
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_rich_text::FieldOutcome;

use crate::app::{
    AttachmentImportPending, AttachmentImportResult, AttachmentPickerResult, ChatApp, Focus, Hit,
    MenuAction, MenuTarget,
};
use crate::messages::{NewMessage, chat_part};
use crate::model::{ChannelKind, Id};
use crate::parts::ChatPart;
use crate::record::chat;
use crate::resource::Chat;

mod attachment_import;
mod embedded_input;
use attachment_import::{
    TOO_BIG_TO_SHARE, prepare_attachment_import, run_worker, store_attachment,
};
use embedded_input::drive_retained_control;
pub(crate) use embedded_input::drive_text_input;

fn item(id: &str) -> ui::tilo_resource::Id {
    ui::tilo_resource::Id::from_raw(id)
}

struct EventDispatch<'a, 'doc> {
    event: &'a WindowEvent,
    scale: f64,
    modifiers: Modifiers,
    doc: &'a mut ui::widget::Edits<'doc>,
    handled: &'a mut bool,
}

impl<'a, 'doc> EventDispatch<'a, 'doc> {
    fn new(
        event: &'a WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &'a mut ui::widget::Edits<'doc>,
        handled: &'a mut bool,
    ) -> Self {
        Self {
            event,
            scale,
            modifiers,
            doc,
            handled,
        }
    }
}

impl ChatApp {
    fn route_thread_scroll_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let handled = self.thread_open.is_some()
            && !self.emoji_open
            && matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
            )
            && drive_retained_control(&mut self.thread_scroll, event, scale, modifiers, doc);
        if handled && !matches!(event, WindowEvent::CursorMoved { .. }) {
            self.thread_follow =
                (self.thread_scroll.offset_y() - self.thread_scroll.max_offset_y()).abs() < 1.0;
        }
        handled
    }

    /// Scroll what is under the pointer: an open thread
    /// (which keeps the wheel from the timeline behind it), or the timeline.
    fn handle_wheel(&mut self, delta: &MouseScrollDelta, scale: f64) {
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 44.0,
            MouseScrollDelta::PixelDelta(p) => p.y / scale,
        };
        if self.thread_open.is_some() && self.thread_body.contains(self.last_mouse) {
            return;
        }
        self.scroll += dy;
        // A positive offset moves away from the live edge. Do this at event
        // time rather than waiting for paint: `sync` may run between the
        // input event and the next frame, and must not acknowledge messages
        // the reader has just scrolled away from.
        if dy > 0.0 {
            self.timeline_at_end = false;
        }
    }

    pub(crate) fn dispatch_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        if self.route_modal_event(event, scale, modifiers, doc) {
            *handled = true;
            return false;
        }
        if self.route_emoji_picker_event(event, scale, modifiers, doc) {
            *handled = true;
            return false;
        }
        if self.route_thread_scroll_event(event, scale, modifiers, doc) {
            *handled = true;
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let last_mouse = Point::new(position.x / scale, position.y / scale);
                self.handle_cursor_moved(
                    last_mouse,
                    EventDispatch::new(event, scale, modifiers, doc, handled),
                );
            }
            // A file dropped on the conversation is shared there: from its
            // bytes where it has no path to read (a browser page).
            WindowEvent::DroppedFile(path) => {
                if self.can_participate() {
                    match ui::portal::dropped() {
                        Some(ui::portal::Dropped::Contents(file)) => {
                            self.device_files.push_back((file.name, file.bytes));
                        }
                        Some(ui::portal::Dropped::NotRead { name, size, why }) => {
                            self.refuse_unread_drop(&name, size, why);
                        }
                        None => {
                            self.begin_attachment_import(path);
                        }
                    }
                }
                *handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_wheel(delta, scale);
                // The wheel moved a view (the timeline, the emoji grid) and
                // nothing the app publishes: the timeline's
                // read receipt follows when it arrives at its newest edge.
                ui::widget::note_view_only_pan();
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                return self.handle_left_press(event, scale, modifiers, doc, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_release(event, scale, modifiers, doc);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                if let Some(hit) = self.hit_at(self.last_mouse) {
                    self.on_right_click(hit);
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.handle_keyboard_input(
                    logical_key,
                    EventDispatch::new(event, scale, modifiers, doc, handled),
                );
            }
            WindowEvent::Ime(_) => {
                *handled |= self.handle_text_input_event(event, scale, modifiers, doc);
            }
            _ => {}
        }
        false
    }

    /// While the emoji picker is up it takes the keyboard (typing searches,
    /// arrows move, Enter picks, Escape closes) and the pointer over it; a
    /// press anywhere else closes it. A pick goes the way a picked
    /// [`Hit::EmojiChoice`] does. Pointer moves still reach the rest of
    /// Chat, so hover stays current.
    fn route_emoji_picker_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if !self.emoji_open {
            return false;
        }
        if !self.emoji_picker.is_open() {
            self.emoji_picker.reopen();
        }
        let over = self.emoji_picker.rect().contains(self.last_mouse);
        drive_retained_control(&mut self.emoji_picker, event, scale, modifiers, doc);
        for picked in self.emoji_picker.take_events() {
            match picked {
                ui::widget::PickerEvent::Picked(emoji) => self.on_hit_emoji_choice(&emoji, doc),
                ui::widget::PickerEvent::Dismissed => self.close_emoji_picker(),
            }
        }
        if !self.emoji_picker.is_open() {
            self.close_emoji_picker();
        }
        match event {
            WindowEvent::KeyboardInput { .. }
            | WindowEvent::Ime(_)
            | WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => true,
            WindowEvent::MouseWheel { .. } | WindowEvent::MouseInput { .. } => over,
            _ => false,
        }
    }

    fn close_emoji_picker(&mut self) {
        self.emoji_open = false;
        self.emoji_target = None;
        self.emoji_picker.close();
    }

    /// Give an open shared context menu first refusal. Its selection index is
    /// translated back to Chat's typed action only after the widget closes,
    /// keeping all mutation in the existing `ActionHost` path.
    fn route_context_menu_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let Some(mut menu) = self.menu.take() else {
            return false;
        };
        let widget_handled = drive_retained_control(&mut menu.widget, event, scale, modifiers, doc);
        let selection = menu.widget.take_selection();
        let selected = selection.and_then(|index| menu.items.get(index).copied());
        let target = menu.target.clone();
        let still_open = menu.widget.is_open();
        if still_open {
            self.menu = Some(menu);
        }
        if let Some(action) = selected {
            if self.can_participate() {
                self.apply_menu_action(target, action, doc);
            }
            return true;
        }

        // The menu is modal while open: typing, scrolling and left-button
        // transitions must not leak into the composer or content beneath it.
        widget_handled
            || matches!(
                event,
                WindowEvent::KeyboardInput { .. }
                    | WindowEvent::Ime(_)
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::MouseInput {
                        button: MouseButton::Left,
                        ..
                    }
            )
    }

    /// A pointer move: live-drags the floating call overlay, then forwards
    /// hover state to both composers.
    fn handle_cursor_moved(&mut self, last_mouse: Point, dispatch: EventDispatch<'_, '_>) {
        let EventDispatch {
            event,
            scale,
            modifiers,
            doc,
            handled,
        } = dispatch;
        self.last_mouse = last_mouse;
        // Live-drag the floating call overlay.
        if self.call.placement.is_dragging() {
            // Freeze the complete last in-app presentation, including its
            // soft silhouette and controls. The compositor uses this exact
            // region as the visual bridge until the app-owned native surface
            // commits its first buffer.
            let source_region = self.huddle_panel().inflate(8.0, 8.0);
            let desktop = self.desktop_floating_bounds();
            let handoff = self
                .call
                .placement
                .update_drag(self.last_mouse, self.rect, desktop);
            if handoff == ui::floating_surface::Handoff::Detached {
                let mut request = self
                    .call
                    .floating_request(source_region, self.call_video_rect());
                if let Some(request) = request.as_mut() {
                    request.presentation_rect = Some(self.huddle_panel().inflate(8.0, 8.0));
                }
                self.pending_floating = request;
            }
            self.request_relayout();
            *handled = true;
        }
        if self.agent_conversation_drag.is_some() {
            self.update_agent_conversation_drag(self.last_mouse);
            *handled = true;
        }
        let hovered = if self.call.placement.is_dragging() {
            None
        } else {
            self.hit_at(self.last_mouse).filter(|hit| {
                matches!(
                    hit,
                    Hit::CallParticipantsToggle
                        | Hit::CallMute
                        | Hit::CallDeafen
                        | Hit::CallCamera
                        | Hit::CallScreen
                        | Hit::CallExpand
                        | Hit::CallLeave
                )
            })
        };
        let hover_changed = match (&self.call_hovered, &hovered) {
            (Some(before), Some(after)) => {
                std::mem::discriminant(before) != std::mem::discriminant(after)
            }
            (None, None) => false,
            _ => true,
        };
        if hover_changed {
            self.call_hovered = hovered;
            *handled = true;
        }
        // Keep every shared input's hover/drag state current so clicks place
        // carets and press-drags extend selections on inline editors too.
        self.forward_pointer_to_text_inputs(event, scale, modifiers, doc);
    }

    /// A left-button press: begins a call-overlay drag, or resolves an
    /// ordinary hit (forwarding the press into a composer that was just
    /// focused by it).
    fn handle_left_press(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        let mut focus_requested = false;
        self.keyboard_target = None;
        // The composer's `@` list floats over the timeline: a press on it
        // picks someone in the composer, not whatever lies beneath.
        if self
            .focused_composer()
            .mention_picker_rect()
            .is_some_and(|list| list.contains(self.last_mouse))
        {
            self.route_composer_key(event, scale, modifiers, doc);
            *handled = true;
            return false;
        }
        match self.hit_at(self.last_mouse) {
            Some(Hit::CallDragHandle) => {
                let huddle = self.call_video_rect();
                self.call.placement.set_preferred_size(huddle.size());
                self.call.placement.begin_drag(
                    self.last_mouse,
                    self.rect,
                    self.desktop_floating_bounds(),
                );
                *handled = true;
            }
            Some(Hit::AgentDragHandle) => {
                self.begin_agent_conversation_drag(self.last_mouse);
                *handled = true;
            }
            Some(hit) => {
                let text_input_hit =
                    matches!(hit, Hit::FocusMain | Hit::FocusThread | Hit::FocusEdit);
                self.on_hit(hit, doc);
                // A click in a composer focuses it (via `on_hit`) and
                // then places the caret at the click point — forward the
                // real press so the widget resolves the position.
                if text_input_hit {
                    self.forward_press_to_focused_text_input(event, scale, modifiers, doc);
                    // `on_hit` above activated this actual input. Its local
                    // caret may already be focused on a cold mount, but the
                    // retained Chat owner must still claim keyboard/IME focus.
                    focus_requested = true;
                }
                *handled = true;
            }
            None => {}
        }
        focus_requested
    }

    /// A left-button release: ends any overlay drag or composer
    /// drag-selection.
    fn handle_left_release(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        self.call.placement.end_drag(self.desktop_floating_bounds());
        self.end_agent_conversation_drag();
        // End any composer drag-selection in progress.
        self.forward_pointer_to_text_inputs(event, scale, modifiers, doc);
    }

    /// A pressed key: inline shared inputs win while open; otherwise the
    /// focused composer receives it unless a popover owns the keyboard.
    fn handle_keyboard_input(&mut self, logical_key: &Key, dispatch: EventDispatch<'_, '_>) {
        let EventDispatch {
            event,
            scale,
            modifiers,
            doc,
            handled,
        } = dispatch;
        if self.editing.is_some() {
            if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                self.cancel_edit();
            } else {
                self.route_edit_input(event, scale, modifiers, doc);
            }
            *handled = true;
            return;
        }

        if self.thread_open.is_some()
            && self.right_flap_open()
            && (matches!(self.focus, Focus::Thread) || self.thread_body.contains(self.last_mouse))
        {
            let target = match logical_key {
                Key::Named(NamedKey::PageUp) => {
                    Some(f64::from(self.thread_scroll.offset_y()) - self.thread_body.height() * 0.9)
                }
                Key::Named(NamedKey::PageDown) => {
                    Some(f64::from(self.thread_scroll.offset_y()) + self.thread_body.height() * 0.9)
                }
                Key::Named(NamedKey::Home) if modifiers.state().control_key() => Some(0.0),
                Key::Named(NamedKey::End) if modifiers.state().control_key() => {
                    Some(f64::from(self.thread_scroll.max_offset_y()))
                }
                _ => None,
            };
            if let Some(target) = target {
                self.thread_scroll.set_offset_y(ui::num::f32_of_f64(target));
                self.thread_follow =
                    (self.thread_scroll.offset_y() - self.thread_scroll.max_offset_y()).abs() < 1.0;
                *handled = true;
                return;
            }
        }

        // Preserve TextInput's ordinary End behaviour, but offer the familiar
        // Ctrl+End route to the conversation's live edge from anywhere in the
        // app. The visible button below exposes the same operation to pointer,
        // Tab, and assistive-technology users.
        if modifiers.state().control_key() && matches!(logical_key, Key::Named(NamedKey::End)) {
            self.jump_to_latest();
            *handled = true;
            return;
        }

        // Tab walks Chat's real primary-control inventory. Enter/Space then
        // activates the same `Hit` path as a pointer, including the composers.
        // This check deliberately precedes composer routing: a multiline input
        // must not swallow the application's navigation key.
        // An open `@` list in the composer takes Tab to pick someone.
        if matches!(logical_key, Key::Named(NamedKey::Tab))
            && self.editing.is_none()
            && self.focused_composer().is_focused()
            && self.focused_composer().mention_picker_open()
        {
            self.route_composer_key(event, scale, modifiers, doc);
            *handled = true;
            return;
        }
        if matches!(logical_key, Key::Named(NamedKey::Tab)) {
            self.move_keyboard_target(modifiers.state().shift_key());
            *handled = true;
            return;
        }
        let activates = matches!(logical_key, Key::Named(NamedKey::Enter))
            || matches!(logical_key, Key::Character(value) if value.as_str() == " ");
        if self.keyboard_target.is_some() && activates && self.activate_keyboard_target(doc) {
            *handled = true;
            return;
        }

        let composer_owns = self.main_composer_can_edit()
            && self.menu.is_none()
            && !self.emoji_open
            && self.history_open.is_none()
            && !self.attach_open
            && self.live_shares.panel.is_none()
            && (!matches!(logical_key, Key::Named(NamedKey::Escape))
                || self.focused_composer().mention_picker_open());
        if composer_owns {
            self.route_composer_key(event, scale, modifiers, doc);
            *handled = true;
        } else if self.on_key(logical_key, modifiers, doc) {
            *handled = true;
        }
    }

    /// Route an IME event to whichever shared input owns focus. Composer
    /// commits still splice into the collaborative draft; an inline edit's
    /// buffer remains local until its explicit Save boundary.
    fn handle_text_input_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if self.editing.is_some() {
            self.route_edit_input(event, scale, modifiers, doc);
            true
        } else if self.main_composer_can_edit()
            && self.menu.is_none()
            && !self.emoji_open
            && self.history_open.is_none()
            && !self.attach_open
            && self.live_shares.panel.is_none()
        {
            self.route_composer_key(event, scale, modifiers, doc);
            true
        } else {
            false
        }
    }

    fn route_edit_input(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        self.edit_input.handle(event, scale, modifiers, doc);
        match self.edit_input.take_outcome() {
            Some(FieldOutcome::Submit) => self.commit_edit(doc),
            Some(FieldOutcome::Cancel) => self.cancel_edit(),
            None => {}
        }
    }

    /// Route a keystroke into the focused composer `TextInput`. Enter submits
    /// (Shift+Enter inserts a newline — the composer is multiline with
    /// enter-submits); any text edit is written back into the per-channel /
    /// thread draft `Text` in the CRDT (diff-spliced) so a shared draft stays
    /// collaborative + persistent. A direct-Agent draft stays in the
    /// peer-local composer source and never creates a document draft. On an
    /// empty composer, `ArrowUp` loads your last message into the inline
    /// editor (Slack/Discord convention).
    fn route_composer_key(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::ArrowUp),
                    ..
                },
            ..
        } = event
            && !modifiers.state().shift_key()
            && self.focused_composer().value().is_empty()
        {
            if let Some(id) = self.last_own_message_id() {
                self.begin_edit(id);
                self.reload(doc);
            }
            return;
        }
        // A draft is made in the person's own record the first time its
        // composer is written in.
        self.focused_composer_mut().ensure_draft(doc);
        self.focused_composer_mut()
            .handle(event, scale, modifiers, doc);
        // A pasted picture is shared as one, like a chosen file.
        for image in self.focused_composer_mut().take_pasted_images() {
            let extension = image.mime.rsplit('/').next().unwrap_or("png");
            self.device_files
                .push_back((format!("Pasted image.{extension}"), image.bytes));
        }
        match self.focused_composer_mut().take_outcome() {
            Some(FieldOutcome::Submit) => self.send_focused(doc),
            Some(FieldOutcome::Cancel) | None => {}
        }
        // A draft edit changes only this surface's draft; the full projection
        // of every channel's messages is rebuilt by `sync` because the
        // document moved.
        let draft = self.focused_composer().value();
        match self.focus {
            Focus::Thread if self.thread_open.is_some() => self.draft_thread = draft,
            _ => self.draft_main = draft,
        }
    }

    /// Forward a `CursorMoved` or mouse release to every text input so hover
    /// and drag-select state stays current regardless of which surface is
    /// focused, and drag-selection ends.
    fn forward_pointer_to_text_inputs(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        self.composer_main.handle(event, scale, modifiers, doc);
        self.composer_thread.handle(event, scale, modifiers, doc);
        self.edit_input.handle(event, scale, modifiers, doc);
    }

    /// Forward a press to the focused composer so it places the caret at the
    /// click point (the preceding `CursorMoved` already set its hover flag).
    fn forward_press_to_focused_text_input(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if self.editing.is_some() {
            self.edit_input.handle(event, scale, modifiers, doc)
        } else {
            self.focused_composer_mut()
                .handle(event, scale, modifiers, doc)
        }
    }

    /// Right-click opens a context menu for the message under the cursor,
    /// else for the room. The sidebar's rows ask for theirs through the
    /// shell ([`crate::sidebar`]).
    fn on_right_click(&mut self, hit: Hit) {
        let menu = match hit {
            Hit::CallParticipant(id) => {
                self.open_participant_menu(id);
                return;
            }
            // One of your own messages — offer the message actions (Edit).
            Hit::MessageMenu { msg } => {
                let mine = self.message(&msg).is_some_and(|m| self.is_me(&m.author));
                let author = self.message(&msg).map(|m| m.author.clone());
                let items = if mine {
                    let mut items = MenuAction::for_message().to_vec();
                    if self.message(&msg).is_some_and(|m| self.is_my_live_share(m)) {
                        items.insert(0, MenuAction::StopSharing);
                    }
                    items
                } else {
                    let mut items = MenuAction::for_others_message().to_vec();
                    if !self.perms.can_moderate() {
                        items.retain(|action| *action != MenuAction::DeleteMessage);
                    } else if author.is_some_and(|author| crate::moderation::bannable(&author)) {
                        items.push(MenuAction::BanAuthor);
                    }
                    items
                };
                Some(self.context_menu(MenuTarget::Message(msg), self.last_mouse, items))
            }
            _ if self.keeps_layout() => Some(self.context_menu(
                MenuTarget::Room,
                self.last_mouse,
                MenuAction::for_room().to_vec(),
            )),
            _ => None,
        };
        self.menu = menu;
    }

    /// How loud someone else in the call plays here: silenced, or at one of
    /// a few levels, on this device only.
    pub(crate) fn open_participant_menu(&mut self, id: String) {
        self.menu = Some(self.context_menu(
            MenuTarget::CallParticipant(id),
            self.last_mouse,
            MenuAction::for_call_participant().to_vec(),
        ));
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

    #[allow(
        clippy::too_many_lines,
        reason = "single exhaustive hit router; splitting by surface would obscure the shared pointer/keyboard activation path"
    )]
    pub(crate) fn on_hit(&mut self, hit: Hit, doc: &mut ui::widget::Edits<'_>) {
        // Search reveal accents are transient selection affordances. Once the
        // person interacts with Chat directly, ordinary navigation takes over.
        self.revealed_message = None;
        self.reveal_scroll_pending = false;
        self.revealed_person = None;
        // Ordinary content activation dismisses the modal context menu.
        self.menu = None;
        match hit {
            Hit::Channel(id) => self.on_hit_channel(id, doc),
            Hit::DirectPerson(public_key) => {
                self.call.expanded = false;
                if let Err(error) = self.open_person_conversation(&public_key) {
                    self.person_error = Some(error);
                }
            }
            Hit::AgentWindow => {
                if self.conversation_surface {
                    self.conversation_return_requested = true;
                } else {
                    self.float_agent_conversation();
                }
            }
            Hit::EditSave => self.commit_edit(doc),
            Hit::EditCancel => self.cancel_edit(),
            Hit::AddChannel { category } if self.keeps_layout() => {
                self.on_hit_add_channel(&category);
            }
            Hit::AddCategory if self.keeps_layout() => self.on_hit_add_to_room(),
            Hit::FocusEdit => self.set_composer_focus(),
            Hit::Reaction { msg, emoji } => self.on_hit_reaction(&msg, &emoji, doc),
            Hit::AddReaction { msg, anchor } => self.open_emoji_picker(Some(msg), anchor),
            Hit::OpenThread(id) => self.on_hit_open_thread(id),
            Hit::EditHistory { msg, anchor } => {
                self.history_open = Some(msg);
                self.history_anchor = anchor;
            }
            Hit::EmojiChoice(e) => self.on_hit_emoji_choice(&e, doc),
            Hit::FocusMain => {
                self.focus = Focus::Main;
                self.set_composer_focus();
            }
            Hit::FocusThread => {
                self.focus = Focus::Thread;
                self.set_composer_focus();
            }
            Hit::HostModel(command) => {
                self.activate_model_control(&command);
            }
            Hit::Send => self.send_focused(doc),
            Hit::UndoDelete => self.undo_delete(),
            Hit::JumpToLatest => self.jump_to_latest(),
            Hit::ThreadScroll(end) => {
                self.thread_follow = end;
                let target = if end {
                    self.thread_scroll.max_offset_y()
                } else {
                    0.0
                };
                self.thread_scroll.set_offset_y(target);
            }
            Hit::AgentReasoning(id) => {
                self.thread_follow = false;
                if !self.expanded_reasoning.remove(&id) {
                    self.expanded_reasoning.insert(id);
                }
            }
            Hit::ComposerEmoji(anchor) if self.can_participate() => {
                self.open_emoji_picker(None, anchor);
            }
            Hit::EmojiGroup(group) => self.emoji_picker.select_group(group),
            Hit::EmojiAddCustom => {
                if let Some(source) = ui::workspace_emoji::current() {
                    source.choose_and_add();
                }
            }
            Hit::Attach if self.can_participate() => self.open_attach_picker(),
            Hit::AttachPick(path) => {
                self.attach_file(doc, std::path::Path::new(&path));
            }
            Hit::AttachDevice => self.choose_device_files(),
            Hit::Attachment { msg, action } => match action {
                ui::widget::conversation::AttachmentAction::OpenFullSize => {
                    self.attachment_view = Some(crate::attachment_view::AttachmentView::new(msg));
                }
                ui::widget::conversation::AttachmentAction::Save => {
                    self.begin_attachment_save(&msg);
                }
                ui::widget::conversation::AttachmentAction::OpenSaved => {
                    self.reveal_saved_copy();
                }
                // Chat uploads an attachment in one step: nothing is in flight
                // long enough to cancel here.
                ui::widget::conversation::AttachmentAction::CancelUpload => {}
            },
            Hit::CloseAttachment => self.attachment_view = None,
            Hit::StartCall if self.call_allows(crate::call::CallAction::Join) => {
                self.on_hit_start_call(doc);
            }
            Hit::StopSharingLiveContext => {
                // The same device-local handler the public effect reaches.
                let _ = self.answer_effect(crate::effects::ChatEffect::StopSharingLiveContext);
            }
            Hit::CopyResourceLink(message) => self.begin_resource_navigation(&message),
            Hit::OpenResource(message) => self.open_shared_resource(&message),
            Hit::OpenShare(message) => self.open_share(&message),
            Hit::StopShare(message) => self.stop_share(doc, &message),
            Hit::SwitchShare(message, access) => self.switch_share(doc, &message, access),
            Hit::ShareWindowPanel if self.can_participate() => self.open_share_panel(None),
            Hit::ShareAccess(access) => self.choose_share_access(access),
            Hit::SharePick(tab) => self.choose_share_window(tab),
            Hit::ShareChosen => self.share_chosen(doc),
            Hit::CloseThread => {
                self.close_thread();
            }
            Hit::CallParticipantsToggle => {
                self.call.participants_expanded = !self.call.participants_expanded;
            }
            Hit::CallParticipant(id) => self.open_participant_menu(id),
            Hit::CallMute => {
                let channel = self
                    .native_voice
                    .channel
                    .clone()
                    .unwrap_or_else(|| self.call.channel.clone());
                if let Err(error) = self.native_voice_command(
                    &channel,
                    ui::native_media::Command::SetMuted {
                        muted: !self.call.muted,
                    },
                ) {
                    self.media_error = Some(error);
                }
            }
            Hit::CallDeafen => {
                let channel = self
                    .native_voice
                    .channel
                    .clone()
                    .unwrap_or_else(|| self.call.channel.clone());
                if let Err(error) = self.native_voice_command(
                    &channel,
                    ui::native_media::Command::SetDeafened {
                        deafened: !self.call.deafened,
                    },
                ) {
                    self.media_error = Some(error);
                }
            }
            Hit::CallCamera => self.toggle_call_picture(ui::native_media::PictureSource::Camera),
            Hit::CallScreen => self.toggle_call_picture(ui::native_media::PictureSource::Screen),
            Hit::CallExpand => {
                self.call.expanded = !self.call.expanded;
                self.request_relayout();
            }
            Hit::CallLeave => self.leave_voice(doc),
            Hit::ToggleRightFlap => {
                if self.pins_open && self.right_flap_open() && self.thread_open.is_none() {
                    self.pins_open = false;
                } else {
                    self.pins_open = false;
                    self.right_flap.toggle();
                }
            }
            Hit::TogglePins => {
                if self.pins_open && self.right_flap_open() && self.thread_open.is_none() {
                    self.right_flap.close();
                    self.pins_open = false;
                } else {
                    self.close_thread();
                    self.pins_open = true;
                    self.right_flap.open();
                }
            }
            // A pin whose message is gone has nowhere to lead.
            Hit::RevealMessage(id) => drop(self.reveal_message(&id)),
            // The desktop shows the one card it has for people.
            Hit::ShowPerson { person, .. } => {
                ui::person_card::show(&person, &self.user_name(&person));
            }
            Hit::Dismiss => self.on_hit_dismiss(),
            // MessageMenu is a right-click affordance and drag handles are
            // handled at press time; permission-gated fallthroughs are no-ops.
            _ => {}
        }
        self.reload(doc);
    }

    fn on_hit_channel(&mut self, id: Id, doc: &mut ui::widget::Edits<'_>) {
        if self.direct_person.is_some() {
            self.leave_floated_agent_route();
        }
        self.close_person_conversation();
        self.not_sent = None;
        let kind = self.channel(&id).map(|c| c.kind);
        if kind == Some(ChannelKind::Voice) {
            self.join_voice(&id, doc);
        } else {
            if self.call.expanded {
                // The conversation chosen shows; the call floats over it.
                self.call.expanded = false;
                self.request_relayout();
            }
            self.active_channel = id;
            self.thread_open = None;
            self.reset_timeline_to_latest();
            self.focus = Focus::Main;
            // Navigation is the user-visible read boundary. Other controls in
            // an already-open room must not append a second MarkRead action
            // after their own canonical action or obscure it in history.
            self.mark_active_read(doc);
        }
    }

    fn on_hit_add_channel(&mut self, category: &str) {
        self.open_channel_sheet(crate::channel_sheet::ChannelSheet::new_channel(
            category,
            ChannelKind::Text,
        ));
    }

    /// The window bar's Create: what can be made in the room.
    fn on_hit_add_to_room(&mut self) {
        self.menu = Some(self.context_menu(
            MenuTarget::Room,
            self.last_mouse,
            MenuAction::for_room().to_vec(),
        ));
    }

    /// Ask what to make or change in `sheet`, over everything else.
    pub(crate) fn open_channel_sheet(&mut self, sheet: crate::channel_sheet::ChannelSheet) {
        self.menu = None;
        self.emoji_open = false;
        self.attach_open = false;
        self.channel_sheet = Some(sheet);
        self.set_composer_focus();
    }

    /// Give an open modal (the channel sheet, a context menu) `event` first;
    /// returns whether one took it. The sheet takes every event.
    fn route_modal_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if self.channel_sheet.is_some() {
            self.route_channel_sheet_event(event, scale, modifiers, doc);
            return true;
        }
        self.route_context_menu_event(event, scale, modifiers, doc)
    }

    /// Route `event` to the open channel sheet and apply what it settles on.
    fn route_channel_sheet_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        let host_focused = self.host_focused;
        let Some(sheet) = self.channel_sheet.as_mut() else {
            return;
        };
        match sheet.handle(event, scale, modifiers, doc, host_focused) {
            crate::channel_sheet::SheetOutcome::Open => {}
            crate::channel_sheet::SheetOutcome::Cancelled => {
                self.channel_sheet = None;
                self.set_composer_focus();
            }
            crate::channel_sheet::SheetOutcome::Done(answer) => {
                self.channel_sheet = None;
                self.apply_channel_sheet(&answer, doc);
                self.set_composer_focus();
            }
        }
    }

    /// Make or change what the sheet settled on, through the record's verbs
    /// and field edits; a new text channel opens.
    pub(crate) fn apply_channel_sheet(
        &mut self,
        answer: &crate::channel_sheet::SheetAnswer,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        use crate::channel_sheet::SheetMode;
        let topic = (!answer.topic.is_empty()).then(|| answer.topic.clone());
        match &answer.mode {
            SheetMode::NewChannel => {
                let before: std::collections::HashSet<Id> =
                    self.channels.iter().map(|c| c.id.clone()).collect();
                self.apply_now(
                    doc,
                    Chat::CREATE_CHANNEL.with(chat::CreateChannel {
                        name: answer.name.clone(),
                        category: answer.category.as_deref().map(item),
                        kind: Some(answer.kind),
                        topic,
                    }),
                );
                self.reload(doc);
                let made = self
                    .channels
                    .iter()
                    .find(|c| !before.contains(&c.id))
                    .map(|c| c.id.clone());
                if let Some(id) = made
                    && answer.kind == ChannelKind::Text
                {
                    self.on_hit_channel(id, doc);
                }
            }
            SheetMode::NewCategory => {
                self.apply_now(
                    doc,
                    Chat::CREATE_CATEGORY.with(chat::CreateCategory {
                        name: answer.name.clone(),
                    }),
                );
                self.reload(doc);
            }
            SheetMode::EditChannel(id) => {
                let Some(channel) = self.channel(id).cloned() else {
                    return;
                };
                if channel.name != answer.name {
                    self.apply_now(
                        doc,
                        Chat::RENAME_CHANNEL.with(chat::RenameChannel {
                            channel: item(id),
                            name: answer.name.clone(),
                        }),
                    );
                }
                if channel.topic != answer.topic {
                    self.apply_now(
                        doc,
                        Chat::SET_CHANNEL_TOPIC.with(chat::SetChannelTopic {
                            channel: item(id),
                            topic: answer.topic.clone(),
                        }),
                    );
                }
                let category = (!channel.category.is_empty()).then(|| channel.category.clone());
                if category != answer.category {
                    self.apply_now(
                        doc,
                        Chat::SET_CHANNEL_CATEGORY.with(chat::SetChannelCategory {
                            channel: item(id),
                            category: answer.category.as_deref().map(item),
                        }),
                    );
                }
                self.reload(doc);
            }
        }
    }

    /// Add a channel named `name` to `category` now, as an insert under an
    /// id minted here, so what follows can name it at once.
    pub(crate) fn create_channel_now(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        category: &str,
        name: &str,
        kind: ChannelKind,
    ) -> String {
        let before: std::collections::HashSet<Id> =
            self.channels.iter().map(|c| c.id.clone()).collect();
        self.apply_now(
            doc,
            Chat::CREATE_CHANNEL.with(chat::CreateChannel {
                name: name.to_owned(),
                category: (!category.is_empty()).then(|| item(category)),
                kind: Some(kind),
                topic: None,
            }),
        );
        self.reload(doc);
        self.channels
            .iter()
            .find(|c| !before.contains(&c.id))
            .map(|c| c.id.clone())
            .unwrap_or_default()
    }

    fn on_hit_reaction(&mut self, msg: &str, emoji: &str, doc: &mut ui::widget::Edits<'_>) {
        if self.can_participate() {
            self.toggle_reaction(doc, msg, emoji);
            self.reload(doc);
        }
    }

    fn on_hit_open_thread(&mut self, id: Id) {
        if self.thread_open.as_ref() != Some(&id) {
            self.thread_follow = false;
            self.thread_scroll.set_offset_y(0.0);
        }
        if self.thread_open.as_deref() == Some(id.as_str()) {
            self.close_thread();
        } else {
            self.thread_open = Some(id);
            self.focus = Focus::Thread;
            self.right_flap.open();
        }
    }

    fn close_thread(&mut self) {
        let was_page = self.thread_is_page();
        self.thread_open = None;
        self.focus = Focus::Main;
        if was_page {
            self.right_flap.close();
        }
    }

    /// Open the picker fresh: an empty query on the first group, with the
    /// search field taking the keyboard so typing filters at once.
    fn open_emoji_picker(&mut self, target: Option<Id>, anchor: Rect) {
        self.emoji_open = true;
        self.emoji_target = target;
        self.emoji_anchor = anchor;
        self.emoji_picker.reopen();
        // Space and Enter now type into the query rather than re-activating
        // the control that opened the picker.
        self.keyboard_target = None;
    }

    fn on_hit_emoji_choice(&mut self, e: &str, doc: &mut ui::widget::Edits<'_>) {
        ui::emoji_catalogue::record_recent(e);
        if let Some(target) = self.emoji_target.take() {
            if self.can_participate() {
                self.toggle_reaction(doc, &target, e);
                self.reload(doc);
            }
        } else {
            self.insert_text(doc, e);
        }
        self.close_emoji_picker();
    }

    /// Join the room's voice channel; if it has none, make one to start the
    /// call in so the header "Call" button is never a dead end (a text-only
    /// room can still huddle).
    /// The header Call button. A click while the call is expanded collapses
    /// it — a button-only toggle; everything else is the published
    /// `start-call` effect's own path.
    fn on_hit_start_call(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if self.call.active && self.call.expanded {
            self.call.expanded = false;
            self.request_relayout();
            return;
        }
        if let Err(error) = self.start_call(Some(doc)) {
            self.media_error = Some(error);
        }
    }

    fn on_hit_dismiss(&mut self) {
        if self.attachment_view.take().is_some() {
            // The open attachment is the top-most layer: one click outside
            // the picture closes it, as Escape does.
        } else if self.attach_open {
            self.attach_open = false;
        } else if self.live_shares.panel.is_some() {
            self.live_shares.panel = None;
        } else if self.menu.is_some() {
            self.menu = None;
        } else if self.emoji_open {
            self.emoji_open = false;
            self.emoji_target = None;
        } else if self.history_open.is_some() {
            self.history_open = None;
        } else if self.right_overlay() && self.right_flap_open() {
            self.right_flap.close();
        }
    }

    /// Pointer and accessibility call controls use the same local service as
    /// exported Effect resources. They never mutate durable voice membership.
    fn join_voice(&mut self, channel: &str, _doc: &mut ui::widget::Edits<'_>) {
        if let Err(error) = self.native_voice_command(
            channel,
            ui::native_media::Command::Join {
                channel_id: channel.into(),
            },
        ) {
            self.media_error = Some(error);
        }
    }

    fn leave_voice(&mut self, _doc: &mut ui::widget::Edits<'_>) {
        let channel = self
            .native_voice
            .channel
            .clone()
            .unwrap_or_else(|| self.call.channel.clone());
        if let Err(error) = self.native_voice_command(&channel, ui::native_media::Command::Leave) {
            self.media_error = Some(error);
        }
    }

    pub(crate) fn apply_menu_action(
        &mut self,
        target: MenuTarget,
        action: MenuAction,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        match target {
            MenuTarget::CallParticipant(id) => {
                let mut listening = self.call_listening(&id);
                match action {
                    MenuAction::Silence => listening.silenced = !listening.silenced,
                    MenuAction::Volume(percent) => {
                        listening.percent = percent;
                        listening.silenced = false;
                    }
                    _ => return,
                }
                self.set_call_listening(&id, listening);
            }
            MenuTarget::Channel(id) => self.apply_channel_action(&id, action, doc),
            MenuTarget::Category(id) => self.apply_category_action(&id, action, doc),
            MenuTarget::Message(id) => match action {
                MenuAction::AddReaction => {
                    let anchor = Rect::from_origin_size(self.last_mouse, (1.0, 1.0));
                    self.open_emoji_picker(Some(id), anchor);
                }
                MenuAction::ReplyInThread => {
                    if self.thread_open.as_ref() != Some(&id) {
                        self.on_hit_open_thread(id);
                    }
                }
                MenuAction::EditMessage => self.begin_edit(id),
                MenuAction::PinMessage => {
                    let pinned = self.message(&id).is_some_and(|m| m.pinned);
                    self.apply_part(
                        doc,
                        ChatPart::PIN.with(chat_part::Pin {
                            message: item(&id),
                            pinned: !pinned,
                        }),
                    );
                }
                MenuAction::BanAuthor => self.ban_author(&id),
                MenuAction::StopSharing => self.stop_share(doc, &id),
                MenuAction::DeleteMessage => {
                    let mine = self.message(&id).is_some_and(|m| self.is_me(&m.author));
                    if mine {
                        // Your own message comes back with Undo, so
                        // nothing is asked; removing someone else's, as a
                        // moderator, is asked first.
                        self.delete_own_message(doc, &id);
                    } else {
                        self.apply_part(
                            doc,
                            ChatPart::REMOVE_MESSAGE
                                .with(chat_part::RemoveMessage { message: item(&id) }),
                        );
                    }
                }
                // The menu files a report with a plain reason; Ctrl+K's
                // `Report message` and `Report person` ask for one.
                MenuAction::ReportMessage => {
                    if let Some(author) = self.message(&id).map(|m| m.author.clone()) {
                        self.apply_now(
                            doc,
                            Chat::REPORT_MESSAGE.with(chat::ReportMessage {
                                message: id.clone(),
                                author,
                                reason: "Reported from the message menu".to_owned(),
                            }),
                        );
                    }
                }
                MenuAction::ReportAuthor => {
                    if let Some(author) = self.message(&id).map(|m| m.author.clone()) {
                        self.apply_now(
                            doc,
                            Chat::REPORT_PERSON.with(chat::ReportPerson {
                                person: item(&author),
                                reason: "Reported from the message menu".to_owned(),
                            }),
                        );
                    }
                }
                _ => {}
            },
            MenuTarget::Room => match action {
                MenuAction::NewCategory => {
                    self.open_channel_sheet(crate::channel_sheet::ChannelSheet::new_category());
                }
                MenuAction::NewChannelHere => {
                    self.open_channel_sheet(crate::channel_sheet::ChannelSheet::new_channel(
                        "",
                        ChannelKind::Text,
                    ));
                }
                _ => {}
            },
        }
        self.reload(doc);
    }

    fn apply_channel_action(
        &mut self,
        id: &str,
        action: MenuAction,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        match action {
            MenuAction::Rename => self.begin_rename(MenuTarget::Channel(id.to_string())),
            MenuAction::EditChannel => {
                if let Some(channel) = self.channel(id) {
                    let sheet = crate::channel_sheet::ChannelSheet::edit_channel(channel);
                    self.open_channel_sheet(sheet);
                }
            }
            MenuAction::Delete => {
                self.apply_now(
                    doc,
                    Chat::DELETE_CHANNEL.with(chat::DeleteChannel { channel: item(id) }),
                );
                if self.active_channel == id {
                    self.active_channel.clear(); // reload picks a new one
                }
            }
            MenuAction::Archive => {
                let archived = self.channel(id).is_some_and(|c| c.archived);
                self.apply_now(
                    doc,
                    Chat::ARCHIVE_CHANNEL.with(chat::ArchiveChannel {
                        channel: item(id),
                        archived: !archived,
                    }),
                );
                if !archived && self.active_channel == id {
                    self.active_channel.clear(); // an archived channel leaves the view
                }
            }
            MenuAction::MoveUp | MenuAction::MoveDown => {
                self.apply_now(
                    doc,
                    Chat::MOVE_CHANNEL.with(chat::MoveChannel {
                        channel: item(id),
                        up: action == MenuAction::MoveUp,
                    }),
                );
            }
            _ => {}
        }
        self.reload(doc);
    }

    fn apply_category_action(
        &mut self,
        id: &str,
        action: MenuAction,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        match action {
            MenuAction::Rename => self.begin_rename(MenuTarget::Category(id.to_string())),
            MenuAction::Delete => self.apply_now(
                doc,
                Chat::DELETE_CATEGORY.with(chat::DeleteCategory { category: item(id) }),
            ),
            MenuAction::ToggleCollapse => {
                let open = self.collapsed.contains(id);
                self.toggle_category(id);
                self.places
                    .set_open(&crate::sidebar::category_place(id), open);
            }
            MenuAction::NewChannelHere => self.open_channel_sheet(
                crate::channel_sheet::ChannelSheet::new_channel(id, ChannelKind::Text),
            ),
            MenuAction::NewVoiceChannelHere => self.open_channel_sheet(
                crate::channel_sheet::ChannelSheet::new_channel(id, ChannelKind::Voice),
            ),
            _ => {}
        }
        self.reload(doc);
    }

    /// Name a channel or category anew, in place in the sidebar: the
    /// event that asked hands it the keyboard, and the name comes back
    /// through [`Self::rename_place`].
    fn begin_rename(&mut self, target: MenuTarget) {
        self.sidebar_rename = match target {
            MenuTarget::Channel(id) => Some(crate::sidebar::channel_place(&id)),
            MenuTarget::Category(id) => Some(crate::sidebar::category_place(&id)),
            MenuTarget::Message(_) | MenuTarget::Room | MenuTarget::CallParticipant(_) => None,
        };
    }

    /// Give the channel or category `target` the name `name`.
    pub(crate) fn rename_place(
        &mut self,
        target: &MenuTarget,
        name: &str,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return;
        }
        match target {
            MenuTarget::Channel(id) => {
                self.apply_now(
                    doc,
                    Chat::RENAME_CHANNEL.with(chat::RenameChannel {
                        channel: item(id),
                        name,
                    }),
                );
            }
            MenuTarget::Category(id) => {
                self.apply_now(
                    doc,
                    Chat::RENAME_CATEGORY.with(chat::RenameCategory {
                        category: item(id),
                        name,
                    }),
                );
            }
            MenuTarget::Message(_) | MenuTarget::Room | MenuTarget::CallParticipant(_) => {}
        }
        self.reload(doc);
    }

    fn on_key(&mut self, key: &Key, modifiers: Modifiers, doc: &mut ui::widget::Edits<'_>) -> bool {
        // Escape unwinds the top-most layer.
        if matches!(key, Key::Named(NamedKey::Escape)) {
            if self.attachment_view.is_some() {
                self.attachment_view = None;
            } else if self.attach_open {
                self.attach_open = false;
            } else if self.live_shares.panel.is_some() {
                self.live_shares.panel = None;
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
                self.close_thread();
            } else if self.right_flap_open() && (self.pins_open || self.right_overlay()) {
                // Pinned messages, or People floating over the
                // conversation, are the last layer above it. People docked
                // beside a wide conversation is part of the layout, not a
                // layer.
                self.pins_open = false;
                self.right_flap.close();
                self.right_flap.snap(false);
                self.request_relayout();
            } else {
                return false;
            }
            return true;
        }
        // While a popover is up, swallow other keys.
        if self.emoji_open || self.history_open.is_some() || self.menu.is_some() || self.attach_open
        {
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
            Key::Named(NamedKey::ArrowUp) => {
                // Swallowed here (this path only runs for the inline message
                // editor / popovers). The composer's own ArrowUp — including
                // the empty-composer "edit my last message" shortcut — is
                // handled in `route_composer_key`.
                true
            }
            _ => false,
        }
    }

    /// Move synthetic focus over the controls that were actually painted in
    /// the previous frame. The compositor has its own retained-widget focus;
    /// Chat's immediate-mode children need this small local equivalent.
    fn move_keyboard_target(&mut self, backwards: bool) {
        let targets = self.semantic_hits();
        if targets.is_empty() {
            self.keyboard_target = None;
            return;
        }
        let next = match self.keyboard_target {
            Some(current) if backwards => current.checked_sub(1).unwrap_or(targets.len() - 1),
            Some(current) => (current + 1) % targets.len(),
            None if backwards => targets.len() - 1,
            None => 0,
        };
        self.keyboard_target = Some(next);
        let Some((_, hit, ..)) = targets.get(next) else {
            self.keyboard_target = None;
            return;
        };
        match hit {
            Hit::FocusMain => {
                self.focus = Focus::Main;
                self.composer_main.set_focus(true);
                self.composer_thread.set_focus(false);
            }
            Hit::FocusThread => {
                self.focus = Focus::Thread;
                self.composer_main.set_focus(false);
                self.composer_thread.set_focus(true);
            }
            _ => {
                self.composer_main.set_focus(false);
                self.composer_thread.set_focus(false);
            }
        }
    }

    fn activate_keyboard_target(&mut self, doc: &mut ui::widget::Edits<'_>) -> bool {
        let targets = self.semantic_hits();
        let Some((_, hit, ..)) = self
            .keyboard_target
            .and_then(|index| targets.get(index))
            .cloned()
        else {
            self.keyboard_target = None;
            return false;
        };
        self.on_hit(hit, doc);
        true
    }

    /// Insert `s` at the active shared input's caret. Message edits remain in
    /// memory until saved; otherwise this targets the focused composer
    /// `TextInput` — used by the emoji picker, which splices a picked glyph as
    /// if typed. Composer keystrokes themselves go straight to the widget via
    /// [`route_composer_key`].
    fn insert_text(&mut self, doc: &mut ui::widget::Edits<'_>, s: &str) {
        if !self.main_composer_can_edit() {
            return;
        }
        if self.editing.is_some() {
            if !self.can_participate() {
                return;
            }
            self.edit_input.insert_text(doc, s);
            return;
        }
        self.focused_composer_mut().ensure_draft(doc);
        self.focused_composer_mut().insert_text(doc, s);
        self.reload(doc);
    }

    /// Enter edit mode on one of your own messages: load its current body into
    /// the inline editor with the caret at the end. Only your own messages are
    /// editable; a foreign id is ignored. Closes any open popover.
    pub(crate) fn begin_edit(&mut self, id: Id) {
        if !self.can_participate() {
            return;
        }
        let Some(m) = self.message(&id) else { return };
        if m.author != self.me.id {
            return;
        }
        let blocks = m.blocks();
        self.edit_input.load(&blocks);
        self.editing = Some(id);
        // The message being edited comes into view.
        self.reveal_scroll_pending = true;
        self.focus = Focus::Main;
        self.menu = None;
        self.emoji_open = false;
        self.emoji_target = None;
        self.history_open = None;
        self.set_composer_focus();
    }

    /// The id of your most recent message in the surface the composer is
    /// focused on (the main timeline, or the open thread) — the target of
    /// ArrowUp-to-edit from an empty composer.
    fn last_own_message_id(&self) -> Option<Id> {
        let in_thread = matches!(self.focus, Focus::Thread) && self.thread_open.is_some();
        let parent = self.thread_open.clone();
        let me = self.me.id.clone();
        self.messages
            .iter()
            .rev()
            .find(|m| {
                m.author == me
                    && if in_thread {
                        m.parent == parent
                    } else {
                        m.parent.is_none()
                    }
            })
            .map(|m| m.id.clone())
    }

    /// Commit the inline message editor: submit the record's `edit-message`
    /// (which keeps the prior text in the message's history and flags it
    /// edited), then close the editor. An
    /// edit that empties the message is discarded rather than blanking it.
    fn commit_edit(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if !self.can_participate() {
            return;
        }
        let Some(id) = self.editing.take() else {
            return;
        };
        let text = self.edit_input.value();
        let body = self.edit_input.clips();
        self.edit_input.set_text("");
        self.set_composer_focus();
        if text.trim().is_empty() {
            return;
        }
        self.apply_part(
            doc,
            ChatPart::EDIT_MESSAGE.with(chat_part::EditMessage {
                message: item(&id),
                text,
                body: Some(body),
                mentions: None,
            }),
        );
        self.reload(doc);
    }

    /// Discard the inline message editor without writing.
    fn cancel_edit(&mut self) {
        self.editing = None;
        self.edit_input.set_text("");
        self.set_composer_focus();
    }

    fn send_focused(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if self.direct_person.is_some() {
            self.send_person_focused();
            return;
        }
        // Posting is a write into the workspace: gated by the OS permission
        // context (a View-only / ungranted peer can read but not send).
        if !self.can_participate() {
            return;
        }
        let parent = match self.focus {
            Focus::Thread => self.thread_open.clone(),
            Focus::Main => None,
        };
        let text = self.focused_composer().value();
        if text.trim().is_empty() {
            return;
        }
        let body = self.focused_composer().clips();
        // Posting is the record's `post`, the verb the palette and agents
        // use; the post binds `@name` to participants once, so no reader
        // re-parses the body to work out who it was addressed to. The draft
        // is the writer's own and is emptied in their own record once the
        // post is taken; a refused post keeps it, to send again, and says
        // so beside the composer.
        let posted = self.try_apply_part(
            doc,
            ChatPart::POST.with(chat_part::Post {
                message: NewMessage {
                    channel: item(&self.active_channel),
                    text,
                    parent: parent.as_deref().map(item),
                    body: Some(body),
                    attachment: None,
                    mentions: None,
                    written: None,
                },
            }),
        );
        if let Err(reason) = posted {
            self.not_sent = Some(reason);
            return;
        }
        self.not_sent = None;
        self.last_post = matches!(self.focus, Focus::Main)
            .then(|| (self.active_channel.clone(), self.focused_composer().value()));
        self.focused_composer_mut().clear(doc);
        self.focused_composer_mut().sync(doc);
        if matches!(self.focus, Focus::Main) {
            self.jump_to_latest();
        }
        self.reload(doc);
    }

    /// When the room refused a message this window had already shown as
    /// sent (and withdrew it), say so under the composer and put its text
    /// back there to send again.
    pub(crate) fn settle_refused_post(&mut self) {
        let Some(chain) = &self.chain else {
            return;
        };
        let refused = chain.dispatcher().refused();
        let reason = refused.with(|refused| {
            refused
                .as_ref()
                .filter(|refused| refused.by_room && refused.action == "post")
                .map(|refused| refused.reason.clone())
        });
        let Some(reason) = reason else {
            return;
        };
        refused.set(None);
        self.not_sent = Some(reason);
        if let Some((channel, text)) = self.last_post.take()
            && channel == self.active_channel
            && self.composer_main.value().trim().is_empty()
        {
            // The draft is the person's own record (or this device's), not
            // the room's document: no room edit carries it.
            let mut own = ui::tilo_resource::Doc::new();
            self.composer_main.ensure_draft(&own);
            self.composer_main
                .insert_text(&mut ui::widget::Edits::lend(&mut own), &text);
        }
    }

    /// Return to the newest message. This is intentionally local state: it
    /// changes no shared message/read record by itself; the next sync records
    /// a read watermark only because the reader can then see that edge.
    pub(crate) fn jump_to_latest(&mut self) {
        self.scroll = 0.0;
        self.timeline_at_end = true;
    }

    /// A channel switch has no meaningful visual anchor to preserve, so start
    /// at the current conversation's newest edge and discard the previous
    /// channel's measured height.
    pub(crate) fn reset_timeline_to_latest(&mut self) {
        self.jump_to_latest();
        self.timeline_total_height = 0.0;
    }

    /// Open the workspace file picker (the composer "+"). Lists the real
    /// files in the shared workspace so the user shares an actual file —
    /// nothing is fabricated. In a headless environment with no workspace the
    /// list is empty and the picker says so.
    fn open_attach_picker(&mut self) {
        self.menu = None;
        self.emoji_open = false;
        self.emoji_target = None;
        self.history_open = None;
        self.live_shares.panel = None;
        // Without a workspace on this device (a browser page) the files to
        // share are the person's own: the platform's chooser opens at once,
        // unless there is a window to share too, which the menu offers.
        if !self.external_services {
            if ui::window_share::windows().is_empty() {
                self.choose_device_files();
            } else {
                self.attachment_status = None;
                self.attach_entries.clear();
                self.attach_open = true;
            }
            return;
        }
        // A dismissed picker may still have one bounded enumeration worker.
        // Reopen its pending request instead of spawning another reader; this
        // keeps the reply channel backpressured and prevents a click storm
        // from creating unbounded directory scans.
        if self.attachment_picker_pending.is_some() {
            self.attachment_status = Some("Loading attachments…".into());
            self.attach_open = true;
            return;
        }
        let generation = self.attachment_picker_generation;
        let Some(next) = generation.checked_add(1) else {
            self.attachment_status =
                Some("Attachment picker unavailable: request counter exhausted".into());
            return;
        };
        self.attachment_picker_generation = next;
        self.attachment_picker_pending = Some(generation);
        self.attachment_status = Some("Loading attachments…".into());
        self.attach_entries.clear();
        self.attach_open = true;
        let sender = self.attachment_picker_result_tx.clone();
        let dir = crate::picker::attach_dir();
        let job = move || {
            // No workspace here: the picker offers this device's files
            // alone, with nothing to apologise for.
            let entries = if crate::picker::folder_readable(&dir) {
                crate::picker::list_attachments(&dir)
            } else {
                Vec::new()
            };
            let result = AttachmentPickerResult {
                generation,
                entries,
            };
            let _ = sender.send(result);
        };
        run_worker("chat-attachment-picker", job, &mut self.attachment_status);
    }

    /// Apply one bounded directory enumeration result during sync. Generation
    /// checks make dismissed/reopened pickers fail closed against late workers.
    pub(crate) fn settle_attachment_picker(&mut self) {
        let Ok(result) = self.attachment_picker_result_rx.try_recv() else {
            return;
        };
        let generation = result.generation;
        if self.attachment_picker_pending != Some(generation) {
            return;
        }
        if !self.attach_open {
            // The worker completed after the user dismissed the popover. It
            // has a definite cancelled terminal outcome and frees the single
            // bounded slot; its entries must never flash on a later picker.
            self.attachment_picker_pending = None;
            self.attachment_status = Some("Attachment picker cancelled".into());
            return;
        }
        self.attachment_picker_pending = None;
        self.attach_entries = result.entries;
        self.attachment_status = None;
    }

    /// Open the platform's file chooser for files on this device (on the web,
    /// the browser's own). What is chosen is imported as shared content and
    /// posted, one file at a time.
    pub(crate) fn choose_device_files(&mut self) {
        if !self.can_participate() || self.device_pick.is_some() {
            return;
        }
        self.attach_open = false;
        let limit = usize::try_from(crate::record::MAX_ATTACHMENT_BYTES).unwrap_or(usize::MAX);
        self.device_pick = Some(
            ui::portal::PickRequest::open_multiple(
                &crate::i18n::t("chat-attach-picker-title"),
                &crate::i18n::t("chat-attach-picker-accept"),
            )
            .max_contents_bytes(limit)
            .send_contents(),
        );
    }

    /// Take what the file chooser answered, and start the next waiting
    /// file's import once none is in flight.
    pub(crate) fn settle_device_files(&mut self) {
        if let Some(pick) = &self.device_pick {
            match pick.try_recv() {
                Ok(ui::portal::ContentsOutcome::Picked(files)) => {
                    self.device_pick = None;
                    self.device_files
                        .extend(files.into_iter().map(|file| (file.name, file.bytes)));
                }
                Ok(ui::portal::ContentsOutcome::Cancelled) => self.device_pick = None,
                Ok(ui::portal::ContentsOutcome::Failed(reason)) => {
                    self.device_pick = None;
                    self.attachment_status = Some(format!(
                        "Couldn't open the files ({reason}). Try choosing them again."
                    ));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.device_pick = None;
                    self.attachment_status =
                        Some("The file chooser closed. Try choosing the files again.".into());
                }
            }
        }
        if self.attachment_pending.is_none()
            && let Some((name, bytes)) = self.device_files.pop_front()
        {
            self.begin_bytes_import(name, bytes);
        }
    }

    /// Import `bytes`, a file named `name` from this device (chosen, pasted
    /// or dropped), into the active channel.
    pub(crate) fn begin_bytes_import(&mut self, name: String, bytes: Vec<u8>) -> bool {
        if !self.can_participate() || self.attachment_pending.is_some() {
            return false;
        }
        let token = self.attachment_next_token;
        let Some(next) = token.checked_add(1) else {
            return false;
        };
        self.attachment_next_token = next;
        self.attachment_pending = Some(AttachmentImportPending {
            token,
            channel: self.active_channel.clone(),
        });
        self.attachment_status = Some(format!("Uploading {name}…"));
        let sender = self.attachment_result_tx.clone();
        let content = self.content.clone();
        let job = move || {
            let _ = sender.send(store_attachment(&content, token, name, &bytes));
        };
        run_worker("chat-attachment-import", job, &mut self.attachment_status);
        true
    }

    /// Say why a file dropped on the page, which the page did not read,
    /// was not shared: as for any file too big to share, or one more than
    /// a drop hands over.
    fn refuse_unread_drop(&mut self, name: &str, size: u64, why: ui::portal::NotRead) {
        let reason = match why {
            ui::portal::NotRead::TooMany => {
                "more files were dropped than are taken at once; drop it again".to_owned()
            }
            // A drop hands over more than an attachment may be, so a file
            // too large for it is too large to share.
            ui::portal::NotRead::TooLarge => self
                .room_refuses_file(size)
                .unwrap_or_else(|| TOO_BIG_TO_SHARE.to_owned()),
        };
        self.attachment_status = Some(format!("Not shared: {name}: {reason}"));
    }

    /// Why the room's host would not take a file of `size` bytes from this
    /// person now, in plain words; `None` when it would. A file posted past
    /// what the host takes would stay on this device alone, so it is not
    /// posted at all.
    pub(crate) fn room_refuses_file(&self, size: u64) -> Option<String> {
        let files = self.room_files;
        if files.largest > 0 && size > files.largest {
            return Some(format!(
                "the file is too big; this room takes files up to {}",
                ui::format::memory(files.largest)
            ));
        }
        if files.member_hourly == 0 {
            return None;
        }
        let since = ui::tilo_resource::origin_now_ms()
            .div_euclid(1000)
            .saturating_sub(3600);
        let shared = self
            .held_messages()
            .iter()
            .filter(|(_, message)| message.ts >= since && self.is_me(&message.author.0))
            .filter_map(|(_, message)| match message.attachment.as_ref()? {
                crate::model::Attachment::File { size, .. }
                | crate::model::Attachment::Video { size, .. } => Some(*size),
                crate::model::Attachment::Image { .. } => None,
            })
            .fold(0, u64::saturating_add);
        (shared.saturating_add(size) > files.member_hourly).then(|| {
            format!(
                "you've reached this room's limit of {} of files an hour; try again later",
                ui::format::memory(files.member_hourly)
            )
        })
    }

    /// Accept one attachment import. The path is consumed by a single bounded
    /// worker and never enters the document/action payload. Its result settles
    /// in [`ChatApp::settle_attachment_import`] during `sync`, which is the
    /// only place the canonical `SendMessage` action may be dispatched.
    pub(crate) fn attach_file(
        &mut self,
        _doc: &mut ui::widget::Edits<'_>,
        path: &std::path::Path,
    ) -> bool {
        self.begin_attachment_import(path)
    }

    /// Accept an import without touching the document. This is shared by the
    /// pointer picker and the local resource-operation handler.
    pub(crate) fn begin_attachment_import(&mut self, path: &std::path::Path) -> bool {
        let channel = self.active_channel.clone();
        self.begin_attachment_import_into(&channel, path)
    }

    /// Start an import for an explicitly addressed channel resource.
    pub(crate) fn begin_attachment_import_into(
        &mut self,
        channel: &str,
        path: &std::path::Path,
    ) -> bool {
        if !self.external_services {
            self.attachment_status =
                Some("Attachment import unsupported: no host asset transport".into());
            return false;
        }

        if !self.can_participate() || self.attachment_pending.is_some() {
            return false;
        }
        let token = self.attachment_next_token;
        let Some(next) = token.checked_add(1) else {
            self.attachment_status =
                Some("Attachment import unavailable: request counter exhausted".into());
            return false;
        };
        self.attachment_next_token = next;
        self.attachment_pending = Some(AttachmentImportPending {
            token,
            channel: channel.into(),
        });
        self.attachment_status = Some("Preparing attachment…".into());
        let sender = self.attachment_result_tx.clone();
        let content = self.content.clone();
        let path = path.to_path_buf();
        let job = move || {
            let result = prepare_attachment_import(&content, token, &path);
            // One worker may be active and the reply channel holds one result,
            // so this can never accumulate unbounded buffers. If the app has
            // gone away, dropping the result is cancellation.
            let _ = sender.send(result);
        };
        run_worker("chat-attachment-import", job, &mut self.attachment_status);
        true
    }

    /// Drain at most one bounded worker result and settle it exactly once. A
    /// stale result, permission downgrade, I/O failure, or content-store failure
    /// becomes a visible terminal outcome; only `Ready` creates the message.
    pub(crate) fn settle_attachment_import(&mut self) {
        let Ok(result) = self.attachment_result_rx.try_recv() else {
            return;
        };
        let token = match &result {
            AttachmentImportResult::Ready { token, .. }
            | AttachmentImportResult::Failed { token, .. } => *token,
        };
        let Some(pending) = self.attachment_pending.as_ref() else {
            self.attachment_status = Some("Attachment import discarded: stale result".into());
            return;
        };
        if pending.token != token {
            self.attachment_status = Some("Attachment import discarded: stale result".into());
            return;
        }
        // Only the matching result may consume the accepted request. A stale
        // reply must never cancel a newer in-flight operation.
        let Some(pending) = self.attachment_pending.take() else {
            return;
        };
        let AttachmentImportResult::Ready {
            name,
            blob,
            size,
            width,
            height,
            video,
            ..
        } = result
        else {
            let AttachmentImportResult::Failed { reason, .. } = result else {
                return;
            };
            self.attachment_status = Some(format!("Not shared: {reason}"));
            return;
        };
        if !self.can_participate() {
            self.attachment_status = Some("Attachment import cancelled: permission changed".into());
            return;
        }
        if let Some(reason) = self.room_refuses_file(size) {
            self.attachment_status = Some(format!("Not shared: {reason}"));
            return;
        }
        let attachment = if video {
            crate::model::Attachment::Video {
                name,
                blob,
                w: 0,
                h: 0,
                size,
            }
        } else if width > 0 && height > 0 {
            crate::model::Attachment::Image {
                name,
                blob,
                w: width,
                h: height,
            }
        } else {
            crate::model::Attachment::File { name, blob, size }
        };
        self.submit_part(ChatPart::POST.with(chat_part::Post {
            message: NewMessage {
                channel: item(&pending.channel),
                text: String::new(),
                parent: None,
                body: None,
                attachment: Some(crate::record::AttachmentUpdate::of(&attachment)),
                mentions: None,
                written: None,
            },
        }));
        self.scroll = 0.0;
        self.attachment_status = Some("Attachment shared".into());
    }
}

#[cfg(test)]
mod tests;
